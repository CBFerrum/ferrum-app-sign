//! What signers do through their links: open the request (only their own fields), fetch the PDF,
//! sign or decline. Every operation locks the request row, so concurrent signers and double
//! submissions serialize; a repeated identical signature is answered like the first.
//!
//! Unknown, malformed or forged tokens are "not found" whatever the reason. A genuine link of a
//! request that is closed, or of a signer who has acted, is "gone" (410) with a message.

use crate::document::{self, coord, role_key};
use crate::event::{self, Entry};
use crate::notice::{self, Outbox};
use crate::request::{self, obj, set_signer};
use crate::{child_table, clean, doc_id, human, id, int, now, pdf, rows, s, seal, signature, time, token, ts};
use ferrum_kernel::{Ctx, Doc, Error, FieldError, Kernel, Result, SiteResponse, Tx, with_retry};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Wrong access codes before a signer is locked out for [`LOCKOUT_MINUTES`].
pub const MAX_ATTEMPTS: i64 = 5;
pub const LOCKOUT_MINUTES: i64 = 15;
/// Largest signing request body (two images and some text values).
pub const MAX_SIGN_BODY: usize = 1024 * 1024;
const MAX_TEXT: usize = 500;

pub const CONSENT: &str = "I agree to sign this document electronically and that my electronic signature is as valid as my handwritten one.";

/// Who is calling: the address (hashed before it is kept), the browser and the access code.
pub struct Client {
    pub ip: String,
    pub user_agent: Option<String>,
    pub access_code: Option<String>,
}

pub enum Reply {
    Ok(Value),
    Gone(String, Value),
}

impl Reply {
    pub fn into_response(self) -> SiteResponse {
        match self {
            Reply::Ok(v) => SiteResponse::json(200, &v),
            Reply::Gone(message, state) => {
                let mut body = json!({"error": "gone", "message": message});
                if let (Value::Object(b), Value::Object(st)) = (&mut body, state) {
                    b.extend(st);
                }
                SiteResponse::json(410, &body)
            }
        }
    }
}

fn not_found() -> Error {
    Error::NotFound("signing link".into())
}

pub async fn system(k: &Kernel, tenant: Uuid) -> Result<Ctx> {
    let mut ctx = Ctx::system(tenant, k.registry.schema(tenant).await?);
    ctx.request_id = format!("sign-{}", Uuid::now_v7());
    Ok(ctx)
}

/// A link opened under the request's row lock.
struct Opened {
    request: Doc,
    signer: Doc,
    key: Vec<u8>,
}

impl Opened {
    fn ip_hash(&self, c: &Client) -> String {
        token::ip_hash(&self.key, &c.ip)
    }
    fn state(&self) -> Value {
        json!({"request_status": s(&self.request, "status"), "signer_status": s(&self.signer, "status")})
    }
    fn my_fields(&self) -> Vec<Doc> {
        let role = role_key(s(&self.signer, "role").unwrap_or_default());
        rows(&self.request, "fields").into_iter().filter(|f| role_key(s(f, "role").unwrap_or_default()) == role).collect()
    }
}

async fn open(k: &Kernel, sys: &Ctx, tx: &mut Tx, token_str: &str) -> Result<Opened> {
    let (signer_id, tag) = token::parse(token_str).ok_or_else(not_found)?;
    let table = child_table(sys, "SignRequest", "signers")?;
    let row: Option<(Uuid, Option<String>)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("select parent_id, email from {table} where tenant_id = $1 and id = $2")))
            .bind(sys.tenant)
            .bind(signer_id)
            .fetch_optional(&mut **tx)
            .await?;
    let (parent, email) = row.ok_or_else(not_found)?;
    let key = token::signing_key(k, tx, sys.tenant).await?;
    // The tag is checked before anything is locked: forged tokens cost a lookup, nothing more.
    let email = email.unwrap_or_default();
    if !token::link_valid(&key, sys.tenant, parent, signer_id, &email, &tag) {
        return Err(not_found());
    }
    let request = crate::lock_request(k, sys, tx, parent).await.map_err(|e| match e {
        Error::NotFound(_) => not_found(),
        other => other,
    })?;
    let signer = rows(&request, "signers").into_iter().find(|r| id(r, "id") == Some(signer_id)).ok_or_else(not_found)?;
    // A correction may have changed the email while the lock was awaited.
    if s(&signer, "email").unwrap_or_default() != email || s(&request, "status") == Some("Draft") {
        return Err(not_found());
    }
    Ok(Opened { request, signer, key })
}

/// Whether the link may be used now: `Ok(Some(message))` when it is gone for good.
fn gate(o: &Opened) -> Result<Option<String>> {
    let gone = match s(&o.request, "status") {
        Some("Completed") => Some("Everyone has signed this document; the link no longer works."),
        Some("Declined") => Some("This request was declined, so it can no longer be signed."),
        Some("Voided") => Some("The sender withdrew this request."),
        Some("Expired") => Some("This request has expired."),
        _ if time(&o.request, "expires_at").is_some_and(|t| t <= now()) => Some("This request has expired."),
        _ => match s(&o.signer, "status") {
            Some("Signed") => Some("You have already signed this document."),
            Some("Declined") => Some("You declined to sign this document."),
            _ => None,
        },
    };
    if let Some(m) = gone {
        return Ok(Some(m.to_string()));
    }
    if s(&o.signer, "status") == Some("Waiting") {
        return Err(Error::Forbidden("it is not your turn to sign yet; you will be sent the link again when it is".into()));
    }
    Ok(None)
}

/// Checks the signer's access code, when they have one. A wrong code counts towards a lockout and
/// is recorded; the caller commits that before returning the error.
async fn access(k: &Kernel, sys: &Ctx, tx: &mut Tx, o: &Opened, c: &Client) -> Result<Option<Error>> {
    let Some(hash) = s(&o.signer, "access_code_hash").map(str::to_string) else { return Ok(None) };
    let at = now();
    if let Some(until) = time(&o.signer, "locked_until").filter(|u| *u > at) {
        return Ok(Some(Error::TooManyRequests(format!("too many wrong access codes; try again after {}", human(until)))));
    }
    let Some(code) = c.access_code.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(Some(Error::Unauthorized("enter the access code the sender gave you".into())));
    };
    let signer = doc_id(&o.signer)?;
    if token::code_matches(code, &hash).await {
        if int(&o.signer, "failed_attempts") > 0 || o.signer.get("locked_until").is_some_and(|v| !v.is_null()) {
            set_signer(k, sys, tx, signer, json!({"failed_attempts": 0, "locked_until": null})).await?;
        }
        return Ok(None);
    }
    let failed = int(&o.signer, "failed_attempts") + 1;
    let locked = (failed >= MAX_ATTEMPTS).then(|| at + chrono::Duration::minutes(LOCKOUT_MINUTES));
    set_signer(k, sys, tx, signer, json!({"failed_attempts": if locked.is_some() { 0 } else { failed }, "locked_until": locked.map(ts)})).await?;
    let detail = if locked.is_some() { format!("wrong access code; locked for {LOCKOUT_MINUTES} minutes") } else { "wrong access code".to_string() };
    let actor = s(&o.signer, "email").unwrap_or_default().to_string();
    event::record(k, sys, tx, &o.request, Entry::new("Access Denied", &actor).signer(&o.signer).client(&o.ip_hash(c), c.user_agent.as_deref()).details(detail))
        .await?;
    Ok(Some(match locked {
        Some(until) => Error::TooManyRequests(format!("too many wrong access codes; try again after {}", human(until))),
        None => Error::Forbidden("wrong access code".into()),
    }))
}

/// First access through a link: the signer has viewed the request.
async fn mark_viewed(k: &Kernel, sys: &Ctx, tx: &mut Tx, o: &Opened, c: &Client) -> Result<()> {
    if o.signer.get("viewed_at").is_some_and(|v| !v.is_null()) {
        return Ok(());
    }
    set_signer(k, sys, tx, doc_id(&o.signer)?, json!({"status": "Viewed", "viewed_at": ts(now())})).await?;
    let actor = s(&o.signer, "email").unwrap_or_default().to_string();
    event::record(k, sys, tx, &o.request, Entry::new("Viewed", &actor).signer(&o.signer).client(&o.ip_hash(c), c.user_agent.as_deref())).await?;
    Ok(())
}

/// A link that is still usable, or gone for good (with a message).
pub enum Gate<T> {
    Open(T),
    Gone(String, Value),
}

impl Gate<Value> {
    pub fn reply(self) -> Reply {
        match self {
            Gate::Open(v) => Reply::Ok(v),
            Gate::Gone(m, st) => Reply::Gone(m, st),
        }
    }
}

/// Opens a link, applies the gate and the access code, records the first view, then runs `then`
/// in the same transaction (which `then` commits).
async fn with_link<T, F, Fut>(k: &Kernel, tenant: Uuid, token_str: &str, c: &Client, then: F) -> Result<Gate<T>>
where
    F: Fn(Opened, Tx, Ctx) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let sys = system(k, tenant).await?;
    with_retry(|| async {
        let mut tx = k.db.tenant_tx(tenant).await?;
        let o = open(k, &sys, &mut tx, token_str).await?;
        if let Some(message) = gate(&o)? {
            tx.rollback().await?;
            return Ok(Gate::Gone(message, o.state()));
        }
        if let Some(e) = access(k, &sys, &mut tx, &o, c).await? {
            tx.commit().await?;
            return Err(e);
        }
        mark_viewed(k, &sys, &mut tx, &o, c).await?;
        then(o, tx, sys.clone()).await.map(Gate::Open)
    })
    .await
}

fn field_view(f: &Doc) -> Value {
    json!({
        "id": s(f, "id"), "field_type": s(f, "field_type"), "label": s(f, "label"), "page": int(f, "page"),
        "x": coord(f, "x"), "y": coord(f, "y"), "width": coord(f, "width"), "height": coord(f, "height"), "required": crate::b(f, "required"),
    })
}

/// `GET /signing/{token}`: the request as this signer may see it.
pub async fn view(k: &Kernel, tenant: Uuid, token_str: &str, c: &Client) -> Result<Reply> {
    let gate = with_link(k, tenant, token_str, c, |o, mut tx, sys| async move {
        let settings = crate::settings::load(k, &sys, &mut tx).await?;
        let sender = notice::sender_of(&mut tx, tenant, &o.request).await?;
        tx.commit().await?;
        let r = &o.request;
        let mine = o.my_fields();
        let needs = |t: &str| mine.iter().any(|f| s(f, "field_type") == Some(t));
        let pages: Vec<Value> =
            document::page_sizes(r).unwrap_or_default().iter().enumerate().map(|(i, p)| json!({"page": i + 1, "width": p.width, "height": p.height})).collect();
        Ok(json!({
            "request": {
                "number": s(r, "number"), "title": s(r, "title"), "message": s(r, "message"), "status": s(r, "status"),
                "signing_mode": s(r, "signing_mode"), "expires_at": s(r, "expires_at"), "document_name": s(r, "document_name"),
                "document_sha256": s(r, "document_sha256"), "pages": pages,
                "sender": {"organization": settings.sender_name, "name": sender.as_ref().map(|x| &x.0), "email": sender.as_ref().map(|x| &x.1)},
            },
            "signer": {"role": s(&o.signer, "role"), "name": s(&o.signer, "name"), "email": s(&o.signer, "email"), "status": "Viewed"},
            "fields": mine.iter().map(field_view).collect::<Vec<_>>(),
            "needs": {"signature": needs("Signature"), "initials": needs("Initials")},
            "consent": CONSENT,
        }))
    })
    .await?;
    Ok(gate.reply())
}

/// `GET /signing/{token}/document`: the original PDF `(file name, bytes)`, after checking it
/// still matches its hash.
pub async fn document(k: &Kernel, tenant: Uuid, token_str: &str, c: &Client) -> Result<Gate<(String, Vec<u8>)>> {
    let gate = with_link(k, tenant, token_str, c, |o, tx, _sys| async move {
        tx.commit().await?;
        let file = s(&o.request, "document").and_then(document::file_id).ok_or_else(not_found)?;
        Ok((file, s(&o.request, "document_sha256").unwrap_or_default().to_string(), s(&o.request, "document_name").unwrap_or("document.pdf").to_string()))
    })
    .await?;
    let (file, sha, name) = match gate {
        Gate::Open(v) => v,
        Gate::Gone(m, st) => return Ok(Gate::Gone(m, st)),
    };
    let bytes = document::bytes(k, tenant, file).await.map_err(|_| Error::Unavailable("the document cannot be read right now".into()))?;
    if hex::encode(Sha256::digest(&bytes)) != sha {
        return Err(Error::Unavailable("the document failed its integrity check; contact the sender".into()));
    }
    Ok(Gate::Open((name, bytes)))
}

/// A drawn (PNG data URL) or typed mark.
enum Ink {
    Drawn(String),
    Typed(String),
}

fn ink(v: Option<&Value>, what: &str) -> std::result::Result<Option<Ink>, FieldError> {
    let bad = |m: String| FieldError::new(what, m);
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(m)) => match (m.get("image").and_then(Value::as_str), m.get("text").and_then(Value::as_str)) {
            (Some(url), None) => {
                signature::decode(&signature::png_bytes(url).map_err(bad)?).map_err(bad)?;
                Ok(Some(Ink::Drawn(url.to_string())))
            }
            (None, Some(t)) => Ok(Some(Ink::Typed(signature::typed(t).map_err(bad)?))),
            _ => Err(bad("send either {\"image\": \"data:image/png;base64,...\"} or {\"text\": \"...\"}".into())),
        },
        Some(_) => Err(bad("send either {\"image\": ...} or {\"text\": ...}".into())),
    }
}

/// The validated submission: the signer's marks and every field's value.
struct Submission {
    signature: Option<Ink>,
    initials: Option<Ink>,
    values: Vec<(Uuid, String)>,
}

fn prepare(o: &Opened, body: &Map<String, Value>, today: &str) -> Result<Submission> {
    let mut errors = Vec::new();
    if body.get("consent") != Some(&Value::Bool(true)) {
        errors.push(FieldError::new("consent", "confirm that you agree to sign electronically"));
    }
    let mine = o.my_fields();
    let empty = Map::new();
    let values = match body.get("values") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(m)) => m,
        Some(_) => return Err(Error::field("values", "must be an object of field id → value")),
    };
    for key in values.keys() {
        let entry = mine.iter().find(|f| s(f, "id") == Some(key.as_str()));
        if !entry.is_some_and(|f| matches!(s(f, "field_type"), Some("Text" | "Checkbox"))) {
            errors.push(FieldError::new(format!("values.{key}"), "is not one of your text or checkbox fields"));
        }
    }
    let signature = ink(body.get("signature"), "signature").unwrap_or_else(|e| {
        errors.push(e);
        None
    });
    let mut initials = ink(body.get("initials"), "initials").unwrap_or_else(|e| {
        errors.push(e);
        None
    });
    let has = |t: &str| mine.iter().any(|f| s(f, "field_type") == Some(t));
    if has("Signature") && signature.is_none() {
        errors.push(FieldError::new("signature", "draw or type your signature"));
    }
    if has("Initials") && initials.is_none() {
        let from = match &signature {
            Some(Ink::Typed(t)) => t.clone(),
            _ => s(&o.signer, "name").unwrap_or_default().to_string(),
        };
        let derived = signature::initials_of(&from);
        if derived.is_empty() {
            errors.push(FieldError::new("initials", "add your initials"));
        } else {
            initials = Some(Ink::Typed(derived));
        }
    }
    let mut out = Vec::new();
    for f in &mine {
        let fid = doc_id(f)?;
        let label = s(f, "label").or(s(f, "field_type")).unwrap_or("field").to_string();
        let required = crate::b(f, "required");
        let at = |m: &str| FieldError::new(format!("values.{fid}"), format!("{label}: {m}"));
        let value = match s(f, "field_type") {
            Some("Signature") => signature.as_ref().map(|i| match i {
                Ink::Drawn(_) => "Drawn signature".to_string(),
                Ink::Typed(t) => t.clone(),
            }),
            Some("Initials") => initials.as_ref().map(|i| match i {
                Ink::Drawn(_) => "Drawn initials".to_string(),
                Ink::Typed(t) => t.clone(),
            }),
            Some("Name") => s(&o.signer, "name").map(str::to_string),
            Some("Date Signed") => Some(today.to_string()),
            Some("Checkbox") => match values.get(fid.to_string().as_str()) {
                None | Some(Value::Null) | Some(Value::Bool(false)) if required => {
                    errors.push(at("tick this box to sign"));
                    None
                }
                None | Some(Value::Null) | Some(Value::Bool(false)) => Some("No".to_string()),
                Some(Value::Bool(true)) => Some("Yes".to_string()),
                Some(_) => {
                    errors.push(at("must be true or false"));
                    None
                }
            },
            _ => match values.get(fid.to_string().as_str()) {
                None | Some(Value::Null) => None,
                Some(Value::String(t)) => Some(clean(t, MAX_TEXT + 1)).filter(|t| !t.is_empty()),
                Some(_) => {
                    errors.push(at("must be text"));
                    None
                }
            }
            .and_then(|t| {
                if t.chars().count() > MAX_TEXT {
                    errors.push(at(&format!("at most {MAX_TEXT} characters")));
                    None
                } else if pdf::fit_text(&t, coord(f, "width").unwrap_or_default(), coord(f, "height").unwrap_or_default()).is_none() {
                    errors.push(at("too long to fit its box"));
                    None
                } else {
                    Some(t)
                }
            }),
        };
        match value {
            Some(v) => out.push((fid, v)),
            None if required && s(f, "field_type") == Some("Text") => errors.push(at("is required")),
            None => {}
        }
    }
    if errors.is_empty() { Ok(Submission { signature, initials, values: out }) } else { Err(Error::Validation(errors)) }
}

/// `POST /signing/{token}/sign`.
pub async fn sign(k: &Kernel, tenant: Uuid, token_str: &str, c: &Client, raw: &[u8]) -> Result<Reply> {
    if raw.len() > MAX_SIGN_BODY {
        return Err(Error::invalid(format!("the request is larger than {} KB", MAX_SIGN_BODY / 1024)));
    }
    let body: Map<String, Value> = match serde_json::from_slice(raw) {
        Ok(Value::Object(m)) => m,
        _ => return Err(Error::invalid("send a JSON object")),
    };
    let submission = hex::encode(Sha256::digest(raw));
    let sys = system(k, tenant).await?;
    let (reply, seal_id) = with_retry(|| async {
        let mut tx = k.db.tenant_tx(tenant).await?;
        let o = open(k, &sys, &mut tx, token_str).await?;
        // A repeated submission (a retry after a lost answer) gets the first one's answer.
        if s(&o.signer, "status") == Some("Signed") {
            tx.rollback().await?;
            if s(&o.signer, "submission_hash") == Some(submission.as_str()) {
                return Ok((Reply::Ok(json!({"status": "Signed", "request_status": s(&o.request, "status"), "replayed": true})), None));
            }
            return Err(Error::Conflict("you have already signed this document".into()));
        }
        if let Some(message) = gate(&o)? {
            tx.rollback().await?;
            return Ok((Reply::Gone(message, o.state()), None));
        }
        if let Some(e) = access(k, &sys, &mut tx, &o, c).await? {
            tx.commit().await?;
            return Err(e);
        }
        let plan = prepare(&o, &body, &sys.today().to_string())?;
        let (status, all_signed) = apply_signature(k, &sys, &mut tx, &o, plan, c, &submission).await?;
        tx.commit().await?;
        let rid = doc_id(&o.request)?;
        Ok((Reply::Ok(json!({"status": "Signed", "request_status": status, "replayed": false})), all_signed.then_some(rid)))
    })
    .await?;
    let Some(rid) = seal_id else { return Ok(reply) };
    // The last signature: seal now. A failure is kept on the request and retried by maintenance.
    let completed = matches!(seal::seal(k, tenant, rid).await, Ok(true));
    Ok(match reply {
        Reply::Ok(mut v) => {
            if completed {
                v["request_status"] = json!("Completed");
            }
            Reply::Ok(v)
        }
        other => other,
    })
}

async fn apply_signature(k: &Kernel, sys: &Ctx, tx: &mut Tx, o: &Opened, plan: Submission, c: &Client, submission: &str) -> Result<(String, bool)> {
    let at = ts(now());
    let mut values = json!({
        "status": "Signed", "signed_at": at, "ip_hash": o.ip_hash(c), "user_agent": c.user_agent.as_deref().map(|u| clean(u, 500)),
        "submission_hash": submission, "failed_attempts": 0, "locked_until": null,
    });
    for (kind, mark) in [("signature", &plan.signature), ("initials", &plan.initials)] {
        match mark {
            Some(Ink::Drawn(url)) => values[format!("{kind}_image")] = json!(url),
            Some(Ink::Typed(t)) => values[format!("{kind}_text")] = json!(t),
            None => {}
        }
    }
    let sid = doc_id(&o.signer)?;
    set_signer(k, sys, tx, sid, values).await?;
    for (fid, v) in &plan.values {
        k.set_child_values_tx(sys, tx, "SignRequest", "fields", *fid, obj(json!({"value": v}))).await?;
    }
    let actor = s(&o.signer, "email").unwrap_or_default().to_string();
    let method = match plan.signature {
        Some(Ink::Drawn(_)) => "signature drawn",
        _ => "signature typed",
    };
    event::record(k, sys, tx, &o.request, Entry::new("Signed", &actor).signer(&o.signer).client(&o.ip_hash(c), c.user_agent.as_deref()).details(method))
        .await?;

    // Who signs next: the request as it now stands.
    let rid = doc_id(&o.request)?;
    let mut after = k.get_tx(sys, tx, "SignRequest", rid, false).await?;
    let all_signed = rows(&after, "signers").iter().all(|r| s(r, "status") == Some("Signed"));
    if !all_signed && request::sequential(&after) {
        let next: Vec<Doc> = request::turn(&after).into_iter().filter(|r| s(r, "status") == Some("Waiting")).collect();
        if !next.is_empty() {
            let out = Outbox::load(k, sys, tx).await?;
            let mut links = Vec::new();
            for sg in &next {
                set_signer(k, sys, tx, doc_id(sg)?, json!({"status": "Sent", "sent_at": at})).await?;
            }
            after = k.get_tx(sys, tx, "SignRequest", rid, false).await?;
            for sg in request::turn(&after).iter().filter(|r| next.iter().any(|n| n.get("id") == r.get("id"))) {
                let emailed = out.invite(k, sys, tx, &after, sg, false).await?;
                let detail = if emailed { "Emailed the signing link" } else { "Signing link given to the sender" };
                event::record(k, sys, tx, &after, Entry::new("Sent", "system").signer(sg).details(detail)).await?;
                links.push(notice::link_row(&out, &after, sg, emailed)?);
            }
            if !out.mail {
                let names: Vec<&str> = next.iter().filter_map(|r| s(r, "name")).collect();
                let title = format!("{}: it is {}'s turn to sign", s(&after, "number").unwrap_or_default(), names.join(", "));
                let body: Vec<String> = links.iter().filter_map(|l| Some(format!("{}: {}", l["email"].as_str()?, l["url"].as_str()?))).collect();
                notice::notify_sender(tx, sys.tenant, &after, &title, Some(&format!("Send them their link:\n{}", body.join("\n")))).await?;
            }
        }
    }
    let status = if s(&after, "status") == Some("Sent") { "In Progress" } else { s(&after, "status").unwrap_or("In Progress") };
    k.update_tx(sys, tx, "SignRequest", rid, obj(json!({"status": status})), None).await?;
    Ok((status.to_string(), all_signed))
}

/// `POST /signing/{token}/decline` with `{reason}`.
pub async fn decline(k: &Kernel, tenant: Uuid, token_str: &str, c: &Client, raw: &[u8]) -> Result<Reply> {
    let body: Map<String, Value> = match serde_json::from_slice(raw) {
        Ok(Value::Object(m)) if raw.len() <= 64 * 1024 => m,
        _ => return Err(Error::invalid("send a JSON object with a reason")),
    };
    let reason = body
        .get("reason")
        .and_then(Value::as_str)
        .map(|r| clean(r, 1000))
        .filter(|r| !r.is_empty())
        .ok_or_else(|| Error::field("reason", "tell the sender why you decline"))?;
    let sys = system(k, tenant).await?;
    with_retry(|| async {
        let mut tx = k.db.tenant_tx(tenant).await?;
        let o = open(k, &sys, &mut tx, token_str).await?;
        if s(&o.signer, "status") == Some("Declined") {
            tx.rollback().await?;
            return Ok(Reply::Ok(json!({"status": "Declined", "request_status": s(&o.request, "status"), "replayed": true})));
        }
        if let Some(message) = gate(&o)? {
            tx.rollback().await?;
            return Ok(Reply::Gone(message, o.state()));
        }
        if let Some(e) = access(k, &sys, &mut tx, &o, c).await? {
            tx.commit().await?;
            return Err(e);
        }
        let at = ts(now());
        let sid = doc_id(&o.signer)?;
        let ua = c.user_agent.as_deref().map(|u| clean(u, 500));
        set_signer(
            k,
            &sys,
            &mut tx,
            sid,
            json!({"status": "Declined", "declined_at": at, "decline_reason": reason, "ip_hash": o.ip_hash(c), "user_agent": ua}),
        )
        .await?;
        let actor = s(&o.signer, "email").unwrap_or_default().to_string();
        event::record(
            k,
            &sys,
            &mut tx,
            &o.request,
            Entry::new("Declined", &actor).signer(&o.signer).client(&o.ip_hash(c), c.user_agent.as_deref()).details(reason.clone()),
        )
        .await?;
        let rid = doc_id(&o.request)?;
        let done = k.update_tx(&sys, &mut tx, "SignRequest", rid, obj(json!({"status": "Declined"})), None).await?;
        let title = format!("{} declined {}", s(&o.signer, "name").unwrap_or("A signer"), s(&done, "number").unwrap_or_default());
        notice::notify_sender(&mut tx, tenant, &done, &title, Some(&reason)).await?;
        tx.commit().await?;
        Ok(Reply::Ok(json!({"status": "Declined", "request_status": "Declined", "replayed": false})))
    })
    .await
}

/// `POST /verify` with a PDF as the body: is it a document sealed here, unchanged?
pub async fn verify_presented(k: &Kernel, tenant: Uuid, bytes: &[u8]) -> Result<Value> {
    if bytes.is_empty() {
        return Err(Error::field("file", "send the PDF as the request body"));
    }
    let sha = hex::encode(Sha256::digest(bytes));
    let sys = system(k, tenant).await?;
    let mut tx = k.db.tenant_tx(tenant).await?;
    let found: Option<Uuid> = sqlx::query_scalar("select id from e_sign_request where tenant_id = $1 and sealed_sha256 = $2 and status = 'Completed' limit 1")
        .bind(tenant)
        .bind(&sha)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(rid) = found else {
        tx.commit().await?;
        return Ok(
            json!({"valid": false, "sha256": sha, "message": "This file does not match any document sealed here: it is not one, or it was changed after signing."}),
        );
    };
    let doc = k.get_tx(&sys, &mut tx, "SignRequest", rid, false).await?;
    tx.commit().await?;
    // The stored copy must still match its record too.
    let stored = match s(&doc, "sealed_document").and_then(document::file_id) {
        Some(f) => document::bytes(k, tenant, f).await.ok().map(|b| hex::encode(Sha256::digest(&b))),
        None => None,
    };
    if stored.as_deref() != Some(sha.as_str()) {
        return Ok(json!({"valid": false, "sha256": sha, "message": "The sealed copy kept here no longer matches its record; contact the sender."}));
    }
    let signers: Vec<Value> =
        rows(&doc, "signers").iter().map(|r| json!({"name": s(r, "name"), "role": s(r, "role"), "signed_at": s(r, "signed_at")})).collect();
    Ok(json!({
        "valid": true, "sha256": sha,
        "request": {"number": s(&doc, "number"), "title": s(&doc, "title"), "completed_at": s(&doc, "completed_at"), "signers": signers},
    }))
}
