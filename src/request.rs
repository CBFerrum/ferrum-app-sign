//! Sign requests: drafting (document, signers, fields), sending, reminding, resending, voiding,
//! verification, and corrections of signers who have not signed yet after a request was sent.
//! Once sent, a request changes only through these actions and the signers' own endpoints.

use crate::document::{self, role_key};
use crate::event::{self, Entry};
use crate::notice::{self, Outbox};
use crate::{clean, doc_id, elevated, end_of_day, id, int, now, rows, s, time, token, ts};
use async_trait::async_trait;
use chrono::{Days, NaiveDate};
use ferrum_kernel::{Ctx, Doc, DocHook, Error, FieldError, HookCtx, Kernel, Result, Tx};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Statuses in which signers can still act.
pub const OPEN: [&str; 2] = ["Sent", "In Progress"];

pub fn obj(v: Value) -> Doc {
    match v {
        Value::Object(m) => m,
        _ => Doc::new(),
    }
}

fn status(doc: &Doc) -> &str {
    s(doc, "status").unwrap_or("Draft")
}

fn date(doc: &Doc, key: &str) -> Option<NaiveDate> {
    s(doc, key).and_then(|v| NaiveDate::parse_from_str(&v[..v.len().min(10)], "%Y-%m-%d").ok())
}

pub fn sequential(doc: &Doc) -> bool {
    s(doc, "signing_mode") != Some("Parallel")
}

/// Signers who may sign now: everyone who has not signed in parallel mode; in sequential mode
/// those with the lowest order among them (equal orders sign in parallel).
pub fn turn(doc: &Doc) -> Vec<Doc> {
    let pending: Vec<Doc> = rows(doc, "signers").into_iter().filter(|r| !matches!(s(r, "status"), Some("Signed" | "Declined"))).collect();
    if !sequential(doc) {
        return pending;
    }
    let first = pending.iter().map(|r| int(r, "signing_order")).min();
    pending.into_iter().filter(|r| Some(int(r, "signing_order")) == first).collect()
}

/// Signers whose link is out and who have not acted.
fn waiting(doc: &Doc) -> Vec<Doc> {
    turn(doc).into_iter().filter(|r| matches!(s(r, "status"), Some("Sent" | "Viewed"))).collect()
}

pub async fn set_signer(k: &Kernel, ctx: &Ctx, tx: &mut Tx, signer: Uuid, values: Value) -> Result<()> {
    k.set_child_values_tx(ctx, tx, "SignRequest", "signers", signer, obj(values)).await
}

pub struct RequestHook;

#[async_trait]
impl DocHook for RequestHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if status(doc) != "Draft" {
            if !hc.ctx.is_system && doc.contains_key("created_at") {
                fresh_signers(hc, doc).await?;
            }
            return Ok(());
        }
        if !doc.contains_key("created_at") && s(doc, "sender_email").is_none() && !hc.ctx.user.is_nil() {
            doc.insert("sender_email".into(), json!(hc.ctx.email.to_lowercase()));
        }
        // Access codes are kept only as Argon2id hashes.
        if let Some(Value::Array(signers)) = doc.get_mut("signers") {
            for (i, row) in signers.iter_mut().enumerate() {
                let Value::Object(r) = row else { continue };
                let code = r.get("access_code").and_then(Value::as_str).map(str::trim).filter(|c| !c.is_empty()).map(str::to_string);
                if let Some(code) = code {
                    if !(4..=64).contains(&code.chars().count()) {
                        return Err(Error::Validation(vec![FieldError::new("access_code", "use 4 to 64 characters").in_row("signers", i)]));
                    }
                    r.insert("access_code_hash".into(), json!(token::hash_code(&code).await?));
                }
                r.insert("access_code".into(), Value::Null);
            }
        }
        document::derive(hc, doc).await
    }

    async fn validate(&self, hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if status(doc) != "Draft" {
            return Ok(());
        }
        let mut errors = Vec::new();
        let roles = document::check_roles(doc, "signers", &mut errors);
        document::check_fields(doc, &roles, &mut errors);
        if date(doc, "expires_on").is_some_and(|d| d < hc.ctx.today()) {
            errors.push(FieldError::new("expires_on", "is in the past"));
        }
        if !hc.ctx.is_system {
            check_reference(hc.kernel, hc.ctx, hc.tx, doc, &mut errors).await;
        }
        if errors.is_empty() { Ok(()) } else { Err(Error::Validation(errors)) }
    }

    async fn after_save(&self, hc: &mut HookCtx<'_>, doc: &Doc, old: Option<&Doc>) -> Result<()> {
        match old {
            Some(old) if !hc.ctx.is_system && status(old) != "Draft" => corrections(hc, doc, old).await,
            _ => Ok(()),
        }
    }

    async fn before_delete(&self, _hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
        if status(doc) != "Draft" {
            return Err(Error::invalid("only draft requests can be deleted; void a sent one instead"));
        }
        Ok(())
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        if !["send", "remind", "resend", "void", "verify"].contains(&name) {
            return Ok(None);
        }
        // The kernel locked the row; read it again so the signers are current (see `lock_request`).
        let fresh = hc.kernel.get_tx(&ferrum_domain::util::sys(hc.ctx), hc.tx, "SignRequest", doc_id(doc)?, false).await?;
        let doc = &fresh;
        Ok(Some(match name {
            "send" => send(hc, doc).await?,
            "remind" => remind(hc, doc).await?,
            "resend" => resend(hc, doc, params).await?,
            "void" => void(hc, doc, params).await?,
            "verify" => verify(hc.kernel, hc.ctx, hc.tx, doc).await?,
            _ => return Ok(None),
        }))
    }
}

/// A person's edit of a sent request is built on the signers as the kernel's locking read saw
/// them, which can predate a signature committed while it waited. Under the row lock a new read
/// is current: when the two differ, the edit starts over on the current request.
async fn fresh_signers(hc: &mut HookCtx<'_>, doc: &Doc) -> Result<()> {
    let table = crate::child_table(hc.ctx, "SignRequest", "signers")?;
    let current: Vec<(Uuid, Option<String>)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("select id, status from {table} where tenant_id = $1 and parent_id = $2")))
            .bind(hc.ctx.tenant)
            .bind(doc_id(doc)?)
            .fetch_all(&mut **hc.tx)
            .await?;
    let stale = rows(doc, "signers").iter().any(|r| {
        let seen = s(r, "status").map(str::to_string);
        id(r, "id").is_some_and(|rid| current.iter().any(|(cid, st)| *cid == rid && *st != seen))
    });
    if stale {
        return Err(Error::Retryable("a signer acted on this request meanwhile".into()));
    }
    Ok(())
}

/// The linked record must exist and be readable by the person linking it.
pub async fn check_reference(k: &Kernel, ctx: &Ctx, tx: &mut Tx, doc: &Doc, errors: &mut Vec<FieldError>) {
    match (s(doc, "reference_entity"), s(doc, "reference_id")) {
        (None, None) => {}
        (Some(e), Some(r)) => {
            if ctx.schema.entity(e).is_none() {
                errors.push(FieldError::new("reference_entity", format!("{e} is not an entity of this workspace")));
            } else if let Ok(rid) = r.parse::<Uuid>() {
                if k.get_tx(ctx, tx, e, rid, false).await.is_err() {
                    errors.push(FieldError::new("reference_id", "the record does not exist or you cannot read it"));
                }
            } else {
                errors.push(FieldError::new("reference_id", "must be a record id"));
            }
        }
        _ => errors.push(FieldError::new("reference_id", "set both the entity and the record, or neither")),
    }
}

/// What must hold before a request goes out.
fn check_sendable(doc: &Doc) -> Vec<FieldError> {
    let mut errors = Vec::new();
    let signers = rows(doc, "signers");
    if signers.is_empty() {
        errors.push(FieldError::new("signers", "add at least one signer"));
    }
    if document::page_sizes(doc).is_none() {
        errors.push(FieldError::new("document", "attach the PDF to sign"));
    }
    let fields = rows(doc, "fields");
    for (i, sg) in signers.iter().enumerate() {
        for f in ["name", "email"] {
            if s(sg, f).is_none() {
                errors.push(FieldError::new(f, "is needed to send the request").in_row("signers", i));
            }
        }
        let role = role_key(s(sg, "role").unwrap_or_default());
        let signs =
            fields.iter().any(|f| role_key(s(f, "role").unwrap_or_default()) == role && s(f, "field_type") == Some("Signature") && crate::b(f, "required"));
        if !signs {
            errors.push(
                FieldError::new("role", format!("place at least one required Signature field for {}", s(sg, "role").unwrap_or("this signer")))
                    .in_row("signers", i),
            );
        }
    }
    let roles = document::check_roles(doc, "signers", &mut errors);
    document::check_fields(doc, &roles, &mut errors);
    errors
}

fn not_expired(doc: &Doc) -> Result<()> {
    match time(doc, "expires_at") {
        Some(t) if t <= now() => Err(Error::invalid("this request has expired")),
        _ => Ok(()),
    }
}

async fn send(hc: &mut HookCtx<'_>, doc: &Doc) -> Result<Value> {
    let (k, ctx) = (hc.kernel, hc.ctx);
    if status(doc) != "Draft" {
        return Err(Error::invalid("only draft requests can be sent"));
    }
    let errors = check_sendable(doc);
    if !errors.is_empty() {
        return Err(Error::Validation(errors));
    }
    let out = Outbox::load(k, ctx, hc.tx).await?;
    out.require_url()?;
    let today = ctx.today();
    let days = match id(doc, "template") {
        Some(t) => sqlx::query_scalar::<_, Option<i64>>("select expiry_days from e_sign_template where tenant_id = $1 and id = $2")
            .bind(ctx.tenant)
            .bind(t)
            .fetch_optional(&mut **hc.tx)
            .await?
            .flatten(),
        None => None,
    }
    .unwrap_or(out.settings.link_expiry_days);
    let expires_on = date(doc, "expires_on").unwrap_or_else(|| today.checked_add_days(Days::new(days.max(1) as u64)).unwrap_or(today));
    if expires_on < today {
        return Err(Error::field("expires_on", "is in the past"));
    }
    let expires_at = end_of_day(expires_on, &ctx.schema.timezone);
    let at = now();
    let ectx = elevated(ctx);
    let rid = doc_id(doc)?;
    for sg in turn(doc) {
        set_signer(k, &ectx, hc.tx, doc_id(&sg)?, json!({"status": "Sent", "sent_at": ts(at)})).await?;
    }
    let patch = json!({"status": "Sent", "sent_at": ts(at), "expires_on": expires_on.to_string(), "expires_at": ts(expires_at)});
    let sent = k.update_tx(&ectx, hc.tx, "SignRequest", rid, obj(patch), None).await?;
    let mut links = Vec::new();
    let mut first_event = None;
    for sg in turn(&sent) {
        let emailed = out.invite(k, &ectx, hc.tx, &sent, &sg, false).await?;
        let detail = if emailed { "Emailed the signing link" } else { "Signing link given to the sender" };
        let ev = event::record(k, &ectx, hc.tx, &sent, Entry::new("Sent", &ctx.email).signer(&sg).details(detail)).await?;
        first_event.get_or_insert(ev);
        links.push(notice::link_row(&out, &sent, &sg, emailed)?);
    }
    // The original now belongs to the audit trail: its sender can no longer delete it.
    if let (Some(ev), Some(file)) = (first_event, s(&sent, "document").and_then(document::file_id)) {
        document::freeze(hc.tx, ctx.tenant, file, ev).await?;
    }
    if !out.mail {
        notice::links_to(hc.tx, ctx, &sent, &links).await?;
    }
    Ok(json!({"status": "Sent", "expires_at": ts(expires_at), "signers": links}))
}

async fn remind(hc: &mut HookCtx<'_>, doc: &Doc) -> Result<Value> {
    let (k, ctx) = (hc.kernel, hc.ctx);
    not_expired(doc)?;
    let targets = waiting(doc);
    if targets.is_empty() {
        return Err(Error::invalid("nobody is waiting to sign"));
    }
    let out = Outbox::load(k, ctx, hc.tx).await?;
    let ectx = elevated(ctx);
    let at = ts(now());
    let mut links = Vec::new();
    for sg in targets {
        let emailed = out.invite(k, &ectx, hc.tx, doc, &sg, true).await?;
        set_signer(k, &ectx, hc.tx, doc_id(&sg)?, json!({"last_reminded_at": at})).await?;
        let detail = if emailed { "Emailed a reminder" } else { "Signing link given to the sender" };
        event::record(k, &ectx, hc.tx, doc, Entry::new("Reminded", &ctx.email).signer(&sg).details(detail)).await?;
        links.push(notice::link_row(&out, doc, &sg, emailed)?);
    }
    if !out.mail {
        notice::links_to(hc.tx, ctx, doc, &links).await?;
    }
    Ok(json!({"reminded": links}))
}

/// Sends the invitation again: to everyone whose turn it is, or to one signer (`signer`: a row
/// id, role or email) — after their email was corrected, or when they lost it.
async fn resend(hc: &mut HookCtx<'_>, doc: &Doc, params: &Value) -> Result<Value> {
    let (k, ctx) = (hc.kernel, hc.ctx);
    not_expired(doc)?;
    let current = waiting(doc);
    let targets = match params.get("signer").and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()) {
        None => current,
        Some(want) => {
            let all = rows(doc, "signers");
            let found = all
                .iter()
                .find(|r| {
                    s(r, "id") == Some(want)
                        || role_key(s(r, "role").unwrap_or_default()) == role_key(want)
                        || s(r, "email").is_some_and(|e| e.eq_ignore_ascii_case(want))
                })
                .ok_or_else(|| Error::field("signer", format!("no signer {want} on this request")))?;
            match current.into_iter().find(|r| id(r, "id") == id(found, "id")) {
                Some(r) => vec![r],
                None => {
                    return Err(Error::invalid(format!(
                        "{} cannot sign now (status {})",
                        s(found, "role").unwrap_or("this signer"),
                        s(found, "status").unwrap_or("Waiting")
                    )));
                }
            }
        }
    };
    if targets.is_empty() {
        return Err(Error::invalid("nobody is waiting to sign"));
    }
    let out = Outbox::load(k, ctx, hc.tx).await?;
    let ectx = elevated(ctx);
    let at = ts(now());
    let mut links = Vec::new();
    for sg in targets {
        let emailed = out.invite(k, &ectx, hc.tx, doc, &sg, false).await?;
        set_signer(k, &ectx, hc.tx, doc_id(&sg)?, json!({"sent_at": at})).await?;
        let detail = if emailed { "Emailed the signing link again" } else { "Signing link given to the sender again" };
        event::record(k, &ectx, hc.tx, doc, Entry::new("Resent", &ctx.email).signer(&sg).details(detail)).await?;
        links.push(notice::link_row(&out, doc, &sg, emailed)?);
    }
    if !out.mail {
        notice::links_to(hc.tx, ctx, doc, &links).await?;
    }
    Ok(json!({"resent": links}))
}

async fn void(hc: &mut HookCtx<'_>, doc: &Doc, params: &Value) -> Result<Value> {
    let (k, ctx) = (hc.kernel, hc.ctx);
    if !OPEN.contains(&status(doc)) {
        return Err(Error::invalid("only requests that are out for signature can be voided"));
    }
    let reason = params.get("reason").and_then(Value::as_str).map(|r| clean(r, 1000)).filter(|r| !r.is_empty());
    let ectx = elevated(ctx);
    let voided = k.update_tx(&ectx, hc.tx, "SignRequest", doc_id(doc)?, obj(json!({"status": "Voided", "void_reason": reason})), None).await?;
    let entry = Entry::new("Voided", &ctx.email);
    event::record(
        k,
        &ectx,
        hc.tx,
        &voided,
        match &reason {
            Some(r) => entry.details(r.clone()),
            None => entry,
        },
    )
    .await?;
    if crate::can_email(k, ctx) {
        let out = Outbox::load(k, ctx, hc.tx).await?;
        for sg in rows(&voided, "signers").iter().filter(|r| matches!(s(r, "status"), Some("Sent" | "Viewed" | "Signed"))) {
            out.withdrawn(k, &ectx, hc.tx, &voided, sg).await?;
        }
    }
    Ok(json!({"status": "Voided"}))
}

/// Changes to a sent request: only the name and email of signers who have not signed or
/// declined. A corrected email retires the old link; when it is that signer's turn the new link
/// goes out (emailed, or in a notification to whoever corrected it).
async fn corrections(hc: &mut HookCtx<'_>, doc: &Doc, old: &Doc) -> Result<()> {
    let frozen = || Error::invalid("a sent request cannot change; only the name and email of signers who have not signed yet can be corrected");
    for f in ["title", "message", "document", "signing_mode", "expires_on", "reference_entity", "reference_id", "fields"] {
        if doc.get(f) != old.get(f) {
            return Err(frozen());
        }
    }
    let (new, prev) = (rows(doc, "signers"), rows(old, "signers"));
    if new.len() != prev.len() {
        return Err(frozen());
    }
    let mut changed = Vec::new();
    for (n, o) in new.iter().zip(&prev) {
        if ["id", "role", "signing_order", "access_code_hash"].iter().any(|f| n.get(*f) != o.get(*f)) || s(n, "access_code").is_some() {
            return Err(frozen());
        }
        if n.get("name") == o.get("name") && n.get("email") == o.get("email") {
            continue;
        }
        if matches!(s(o, "status"), Some("Signed" | "Declined")) {
            return Err(Error::invalid(format!(
                "{} already {}; their details cannot change",
                s(o, "role").unwrap_or("this signer"),
                s(o, "status").unwrap_or_default().to_lowercase()
            )));
        }
        if s(n, "name").is_none() || s(n, "email").is_none() {
            return Err(Error::invalid("a signer of a sent request needs a name and an email"));
        }
        changed.push((n.clone(), o.clone()));
    }
    if changed.is_empty() {
        return Ok(());
    }
    let (k, ctx) = (hc.kernel, hc.ctx);
    let ectx = elevated(ctx);
    let out = Outbox::load(k, ctx, hc.tx).await?;
    let mut links = Vec::new();
    for (n, o) in changed {
        let mut what = Vec::new();
        for f in ["name", "email"] {
            if n.get(f) != o.get(f) {
                what.push(format!("{f}: {} → {}", s(&o, f).unwrap_or("-"), s(&n, f).unwrap_or("-")));
            }
        }
        let resent = n.get("email") != o.get("email") && matches!(s(&n, "status"), Some("Sent" | "Viewed"));
        let mut emailed = false;
        if resent {
            set_signer(
                k,
                &ectx,
                hc.tx,
                doc_id(&n)?,
                json!({"status": "Sent", "sent_at": ts(now()), "viewed_at": null, "failed_attempts": 0, "locked_until": null}),
            )
            .await?;
            emailed = out.invite(k, &ectx, hc.tx, doc, &n, false).await?;
            links.push(notice::link_row(&out, doc, &n, emailed)?);
        }
        let mut detail = what.join("; ");
        if resent {
            detail.push_str(if emailed { "; emailed the new link" } else { "; new link given to the sender" });
        }
        event::record(k, &ectx, hc.tx, doc, Entry::new("Corrected", &ctx.email).signer(&n).details(detail)).await?;
    }
    if !out.mail {
        notice::links_to(hc.tx, ctx, doc, &links).await?;
    }
    Ok(())
}

async fn file_check(k: &Kernel, tenant: Uuid, path: Option<&str>, expected: Option<&str>, pdf: bool) -> Value {
    let Some(file) = path.and_then(document::file_id) else { return json!({"ok": false, "error": "no file"}) };
    match document::bytes(k, tenant, file).await {
        Err(_) => json!({"ok": false, "expected": expected, "error": "the stored file is missing"}),
        Ok(bytes) => {
            let sha = hex::encode(Sha256::digest(&bytes));
            let readable = !pdf || crate::pdf::inspect(&bytes).is_ok();
            json!({"ok": Some(sha.as_str()) == expected && readable, "sha256": sha, "expected": expected, "readable": readable})
        }
    }
}

/// Recomputes the hashes of the stored original and sealed PDFs and of the audit trail, and
/// checks the trail matches what the request says happened.
pub async fn verify(k: &Kernel, ctx: &Ctx, tx: &mut Tx, doc: &Doc) -> Result<Value> {
    let rid = doc_id(doc)?;
    let original = file_check(k, ctx.tenant, s(doc, "document"), s(doc, "document_sha256"), false).await;
    let sealed = match s(doc, "sealed_document") {
        Some(path) => file_check(k, ctx.tenant, Some(path), s(doc, "sealed_sha256"), true).await,
        None => Value::Null,
    };
    let trail = event::list(tx, ctx.tenant, rid).await?;
    let check = event::check(rid, &trail);
    let signed = rows(doc, "signers").iter().filter(|r| s(r, "status") == Some("Signed")).count();
    let count = |e: &str| trail.iter().filter(|r| r.event == e).count();
    let mut problem = check.problem.clone();
    if problem.is_none() && count("Signed") < signed {
        problem = Some("signatures are missing from the trail".into());
    }
    if problem.is_none() && status(doc) == "Completed" && trail.last().map(|r| r.event.as_str()) != Some("Completed") {
        problem = Some("the trail does not end with the completion".into());
    }
    let valid = original["ok"] == json!(true) && (sealed.is_null() || sealed["ok"] == json!(true)) && problem.is_none();
    Ok(json!({
        "valid": valid,
        "status": status(doc),
        "original": original,
        "sealed": sealed,
        "audit_trail": {"ok": problem.is_none(), "events": check.events, "head": check.head, "problem": problem},
    }))
}
