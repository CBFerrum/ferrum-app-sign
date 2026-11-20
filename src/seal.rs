//! Sealing a fully signed request: every value is stamped onto the original pages, the audit
//! certificate is appended, the sealed PDF is stored with its SHA-256 and the request completes —
//! the sender is notified and, with Mail, every signer gets a copy.
//!
//! The PDF work happens outside any transaction; the completion is then applied under the
//! request's row lock only if the request is still fully signed and unsealed, so a retry (by
//! maintenance, for sealing that failed) never completes a request twice.

use crate::document::{self, coord, role_key};
use crate::event::{self, Entry};
use crate::notice::{self, Outbox};
use crate::pdf::{self, Mark, Para, Stamp};
use crate::request::obj;
use crate::{doc_id, human, int, now, rows, s, settings, signature, time, ts};
use chrono::{DateTime, Utc};
use ferrum_kernel::{Ctx, Doc, Error, Kernel, Result, with_retry};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use uuid::Uuid;

/// A request an older release let complete without its seal (the database now refuses it, `sign_completed_is_sealed`,
/// but rows from before are not checked): Completed, with no sealed PDF or no hash of it.
pub fn legacy(doc: &Doc) -> bool {
    s(doc, "status") == Some("Completed") && (s(doc, "sealed_document").is_none() || s(doc, "sealed_sha256").is_none())
}

/// Whether a request waits for sealing: every signer signed and nothing sealed yet, in progress (it then completes), or
/// already Completed without a seal (it then only gets the seal).
pub fn ready(doc: &Doc) -> bool {
    let waiting = (s(doc, "status") == Some("In Progress") && s(doc, "sealed_document").is_none()) || legacy(doc);
    waiting && crate::request::signed_by_all(doc)
}

/// Seals the request if it is ready; `Ok(false)` when there was nothing to do. On failure the
/// reason is kept in `seal_error` (and the sender told once) for maintenance to retry.
pub async fn seal(k: &Kernel, tenant: Uuid, request: Uuid) -> Result<bool> {
    let sys = Ctx::system(tenant, k.registry.schema(tenant).await?);
    let mut tx = k.db.tenant_tx(tenant).await?;
    let doc = k.get_tx(&sys, &mut tx, "SignRequest", request, false).await?;
    // A legacy request that is not fully signed cannot be sealed: it fails below, for a person.
    if !ready(&doc) && !legacy(&doc) {
        tx.rollback().await?;
        return Ok(false);
    }
    let trail = event::list(&mut tx, tenant, request).await?;
    let settings = settings::load(k, &sys, &mut tx).await?;
    let sender = notice::sender_of(&mut tx, tenant, &doc).await?;
    // The workspace's seal key (made now if this is the first seal), unless it chose none.
    let digital = match crate::seal_key::load(k, &mut tx, tenant).await {
        Ok(v) => v,
        Err(e) => {
            tx.rollback().await?;
            failed(k, &sys, &doc, &e).await;
            return Err(e);
        }
    };
    tx.commit().await?;
    // An old Completed request keeps the moment it completed.
    let completed_at = if legacy(&doc) { time(&doc, "completed_at").unwrap_or_else(now) } else { now() };
    let built = if ready(&doc) {
        build(k, &sys, &doc, &trail, &settings, sender, completed_at, digital).await
    } else {
        Err(Error::invalid("the request is Completed without every signature; a person must look at it"))
    };
    let Built { file, sealed, extra } = match built {
        Ok(v) => v,
        Err(e) => {
            failed(k, &sys, &doc, &e).await;
            return Err(e);
        }
    };
    let sha = hex::encode(Sha256::digest(&sealed.bytes));
    let details =
        format!("Sealed PDF SHA-256 {sha} ({} pages); signed document SHA-256 {} (its first {} bytes)", sealed.pages, sealed.signed_sha256, sealed.signed_len);
    let done = with_retry(|| complete(k, &sys, request, file, &extra, &sha, &details, completed_at)).await;
    match done {
        Ok(true) => Ok(true),
        Ok(false) => {
            // Sealed meanwhile by someone else: drop this copy's record.
            for f in std::iter::once(file).chain(extra.iter().map(|e| e.file)) {
                let _ = k.delete_file(&sys, f).await;
            }
            Ok(false)
        }
        Err(e) => {
            failed(k, &sys, &doc, &e).await;
            Err(e)
        }
    }
}

/// The audit trail as the file carries it (`audit-trail.json`): every event, exactly as chained.
fn trail_json(trail: &[event::Row]) -> Value {
    json!({"events": trail.iter().map(|e| json!({
        "seq": e.seq, "event": e.event, "occurred_at": ts(e.occurred_at), "role": e.role, "signer_name": e.signer_name, "signer_email": e.signer_email,
        "actor": e.actor, "ip_hash": e.ip_hash, "user_agent": e.user_agent, "details": e.details, "prev_hash": e.prev_hash, "event_hash": e.event_hash,
    })).collect::<Vec<_>>()})
}

/// What sealing stored: the main document's sealed PDF with the certificate, and each further document signed.
struct Built {
    file: Uuid,
    sealed: pdf::Sealed,
    extra: Vec<Extra>,
}

/// One more document, signed: its row, the stored file and that file's SHA-256 (and, for the certificate, what it was).
struct Extra {
    row: Uuid,
    file: Uuid,
    sha: String,
    n: usize,
    name: String,
    original_sha: String,
}

/// Renders and stores the sealed PDFs (not yet attached to anything): every further document signed with its own values,
/// then the main one with the certificate, which lists them all.
async fn build(
    k: &Kernel,
    sys: &Ctx,
    doc: &Doc,
    trail: &[event::Row],
    settings: &settings::Settings,
    sender: Option<(String, String)>,
    completed_at: DateTime<Utc>,
    digital: Option<crate::seal_key::SealKey>,
) -> Result<Built> {
    let docs = document::documents(doc);
    let main = docs.iter().find(|d| d.n == 1).ok_or_else(|| Error::invalid("the request has no document"))?;
    let read = |d: &document::DocRef| {
        let (n, file, sha) = (d.n, d.file, d.sha256.clone());
        async move {
            let bytes = document::bytes(k, sys.tenant, file).await.map_err(|e| Error::invalid(format!("the original PDF of document {n} cannot be read: {e}")))?;
            if hex::encode(Sha256::digest(&bytes)) != sha {
                return Err(Error::invalid(format!("the stored original of document {n} no longer matches its SHA-256; it was changed after sending")));
            }
            Ok(bytes)
        }
    };
    let store = |name: String, bytes: Vec<u8>| async move {
        let stored = k.store_file(sys, None, &name, "application/pdf", &bytes).await?;
        stored.get("id").and_then(|v| v.as_str()).and_then(|v| v.parse::<Uuid>().ok()).ok_or_else(|| Error::internal("stored file without id"))
    };
    let mut extra: Vec<Extra> = Vec::new();
    for d in docs.iter().filter(|d| d.n > 1) {
        let original = read(d).await?;
        let marks = stamps(doc, d.n).map_err(Error::invalid)?;
        let signed = tokio::task::spawn_blocking(move || pdf::stamp(&original, &marks))
            .await
            .map_err(|_| Error::internal("sealing the PDF failed"))?
            .map_err(Error::invalid)?;
        let base: String = d.name.trim_end_matches(".pdf").chars().filter(|c| c.is_alphanumeric() || " -_.,()".contains(*c)).take(100).collect();
        let file = store(format!("{} - signed.pdf", if base.trim().is_empty() { "document" } else { base.trim() }), signed.bytes).await?;
        let row = d.row.ok_or_else(|| Error::internal("a further document without its row"))?;
        extra.push(Extra { row, file, sha: signed.sha256, n: d.n, name: d.name.clone(), original_sha: d.sha256.clone() });
    }
    let original = read(main).await?;
    let marks = stamps(doc, 1).map_err(Error::invalid)?;
    let more = extra.iter().map(|e| (e.n, e.name.clone(), e.original_sha.clone(), e.sha.clone())).collect();
    let note = match &digital {
        Some(d) => Some(SealNote { subject: d.subject(), certificate_sha256: d.certificate_sha256()?, stamped: settings.timestamp_connection.is_some() }),
        None => None,
    };
    let cert = Certificate::new(doc, trail, settings, sender, completed_at, more, note);
    let footer = cert.footer.clone();
    let mut sealed = tokio::task::spawn_blocking(move || pdf::seal(&original, &marks, &footer, &|len, sha| cert.paras(len, sha)))
        .await
        .map_err(|_| Error::internal("sealing the PDF failed"))?
        .map_err(Error::invalid)?;
    // The digital seal, the last revision: the workspace's signature over the whole file, the audit trail embedded, and
    // (outside any transaction) the time-stamping authority's stamp of the signature when the workspace uses one.
    if let Some(digital) = digital {
        let json = serde_json::to_vec(&trail_json(trail)).map_err(|e| Error::internal(e.to_string()))?;
        let bytes = std::mem::take(&mut sealed.bytes);
        let begun = tokio::task::spawn_blocking(move || crate::pades::begin(&bytes, &digital, &json, completed_at))
            .await
            .map_err(|_| Error::internal("sealing the PDF failed"))?
            .map_err(Error::invalid)?;
        let token = match &settings.timestamp_connection {
            Some(connection) => Some(crate::tsa::stamp(k, sys, connection, &crate::tsa::imprint_of(begun.signature())).await?.0),
            None => None,
        };
        sealed.bytes = begun.finish(token.as_deref()).map_err(Error::invalid)?;
    }
    let title: String = s(doc, "title").unwrap_or("document").chars().filter(|c| c.is_alphanumeric() || " -_.,()".contains(*c)).take(100).collect();
    let name = format!("{} - signed.pdf", if title.trim().is_empty() { s(doc, "number").unwrap_or("document") } else { title.trim() });
    let file = store(name, sealed.bytes.clone()).await?;
    Ok(Built { file, sealed, extra })
}

/// Completes the request with the stored sealed PDF, if it is still ready.
#[allow(clippy::too_many_arguments)]
async fn complete(k: &Kernel, sys: &Ctx, request: Uuid, file: Uuid, extra: &[Extra], sha: &str, details: &str, at: DateTime<Utc>) -> Result<bool> {
    let mut tx = k.db.tenant_tx(sys.tenant).await?;
    let doc = crate::lock_request(k, sys, &mut tx, request).await?;
    if !ready(&doc) {
        tx.rollback().await?;
        return Ok(false);
    }
    let old = legacy(&doc);
    // An old request was completed (and everyone told) before: it only gets its seal, recorded as `Sealed`.
    let ev = event::record(k, sys, &mut tx, &doc, Entry::new(if old { "Sealed" } else { "Completed" }, "system").details(details)).await?;
    // The sealed PDF belongs to the audit trail.
    document::freeze(&mut tx, sys.tenant, file, ev).await?;
    // The further documents, signed, are the audit trail's too, and each is recorded on its row under the same lock.
    for e in extra {
        document::freeze(&mut tx, sys.tenant, e.file, ev).await?;
        k.set_child_values_tx(sys, &mut tx, "SignRequest", "more_documents", e.row, obj(json!({"sealed_document": format!("/files/{}", e.file), "sealed_sha256": e.sha}))).await?;
    }
    let mut patch = json!({"sealed_document": format!("/files/{file}"), "sealed_sha256": sha, "seal_error": null});
    if !old {
        patch["status"] = json!("Completed");
        patch["completed_at"] = json!(ts(at));
    }
    let done = k.update_tx(sys, &mut tx, "SignRequest", request, obj(patch), None).await?;
    if old {
        let number = s(&done, "number").unwrap_or_default().to_string();
        notice::notify_sender(&mut tx, sys.tenant, &done, &format!("{number} now has its sealed PDF"), Some("The sealed PDF is on the request.")).await?;
        tx.commit().await?;
        return Ok(true);
    }
    crate::envelope::ended(&mut tx, sys, &done, "Completed").await?;
    let number = s(&done, "number").unwrap_or_default().to_string();
    notice::notify_sender(
        &mut tx,
        sys.tenant,
        &done,
        &format!("{number} is signed by everyone"),
        Some(&format!("\"{}\" is complete; the sealed PDF is on the request.", s(&done, "title").unwrap_or_default())),
    )
    .await?;
    if crate::can_email(k) {
        let out = Outbox::load(k, sys, &mut tx).await?;
        for sg in rows(&done, "signers") {
            out.completed(k, sys, &mut tx, &done, &sg, file).await?;
        }
    }
    tx.commit().await?;
    Ok(true)
}

/// Keeps why sealing failed; the first failure is announced to the sender.
async fn failed(k: &Kernel, sys: &Ctx, doc: &Doc, e: &Error) {
    let res = async {
        let mut tx = k.db.tenant_tx(sys.tenant).await?;
        let id = doc_id(doc)?;
        let first = if legacy(doc) {
            // The request itself cannot be written while it is Completed without a seal (the database's check), so the
            // reason goes to the trail, once, under the request's lock.
            let fresh = crate::lock_request(k, sys, &mut tx, id).await?;
            let told: bool = sqlx::query_scalar("select exists (select 1 from e_sign_event where tenant_id = $1 and request = $2 and event = 'Seal Failed')")
                .bind(sys.tenant)
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
            if legacy(&fresh) && !told {
                event::record(k, sys, &mut tx, &fresh, Entry::new("Seal Failed", "system").details(crate::clean(&e.to_string(), 500))).await?;
            }
            legacy(&fresh) && !told
        } else {
            let first = s(doc, "seal_error").is_none();
            k.set_values_tx(sys, &mut tx, "SignRequest", id, obj(json!({"seal_error": crate::clean(&e.to_string(), 2000)}))).await?;
            first
        };
        if first {
            let title = format!("{} is fully signed but could not be sealed", s(doc, "number").unwrap_or_default());
            notice::notify_sender(&mut tx, sys.tenant, doc, &title, Some(&format!("{e}. Sealing is retried automatically."))).await?;
        }
        tx.commit().await?;
        Ok::<_, Error>(())
    }
    .await;
    if let Err(err) = res {
        tracing::warn!(tenant = %sys.tenant, "recording a sealing failure failed: {err}");
    }
    tracing::warn!(tenant = %sys.tenant, request = ?s(doc, "id"), "sealing failed: {e}");
}

/// What the workspace's seal says of itself on the certificate page.
pub struct SealNote {
    pub subject: String,
    pub certificate_sha256: String,
    pub stamped: bool,
}

/// The kind of signature every document sealed here carries (DC-43): what it is and what it is not.
pub fn kind(seal: Option<&SealNote>) -> String {
    match seal {
        None => "Simple electronic signature with an audit trail. Not an advanced or qualified electronic signature, and not a PAdES digital \
                 signature: no signing certificate, signature device or timestamp authority was used."
            .into(),
        Some(_) => "Simple electronic signature with an audit trail, sealed digitally by the workspace's own key (a PAdES signature of the whole file). The \
                    seal shows the file is unchanged since it was sealed; it is not an advanced or qualified electronic signature of any signer."
            .into(),
    }
}

/// What each field shows on the sealed pages.
pub fn stamps(doc: &Doc, document_no: usize) -> std::result::Result<Vec<Stamp>, String> {
    let signers: HashMap<String, Doc> = rows(doc, "signers").into_iter().map(|r| (role_key(s(&r, "role").unwrap_or_default()), r)).collect();
    let mut out = Vec::new();
    for f in rows(doc, "fields").into_iter().filter(|f| document::document_no(f) == document_no) {
        let Some(sg) = signers.get(&role_key(s(&f, "role").unwrap_or_default())) else { continue };
        let ink = |kind: &str| -> std::result::Result<Option<Mark>, String> {
            if let Some(url) = s(sg, &format!("{kind}_image")) {
                return Ok(Some(Mark::Image(signature::decode(&signature::png_bytes(url)?)?)));
            }
            Ok(s(sg, &format!("{kind}_text")).map(|t| Mark::Typed(t.to_string())))
        };
        let value = s(&f, "value");
        let mark = match s(&f, "field_type") {
            Some("Signature") => ink("signature")?,
            Some("Initials") => ink("initials")?,
            Some("Checkbox") => (value == Some("Yes")).then_some(Mark::Check),
            // The chosen box of a Radio carries the tick; the others stay empty. An Attachment is shown nowhere on the pages
            // (the certificate lists it).
            Some("Radio") => value.is_some().then_some(Mark::Check),
            Some("Attachment") => None,
            _ => value.map(|v| Mark::Text(v.to_string())),
        };
        let mut rect = (coord(&f, "x"), coord(&f, "y"), coord(&f, "width"), coord(&f, "height"));
        if s(&f, "field_type") == Some("Radio") {
            let at = document::choices(&f).iter().position(|c| Some(c.as_str()) == value);
            rect = match at.and_then(|i| document::option_boxes(&f).get(i).copied()) {
                Some([x, y, w, h]) => (Some(x), Some(y), Some(w), Some(h)),
                None => (None, None, None, None),
            };
        }
        let (Some(mark), (Some(x), Some(y), Some(width), Some(height))) = (mark, rect) else {
            continue;
        };
        out.push(Stamp { page: int(&f, "page") as u32, x, y, width, height, mark });
    }
    Ok(out)
}

/// The certificate's content, gathered before the PDF work starts.
struct Certificate {
    head: Vec<(String, String)>,
    original_sha: String,
    trail_head: String,
    signers: Vec<[String; 3]>,
    /// Per signer: the files they attached, with their size and SHA-256.
    attached: Vec<Vec<String>>,
    /// The further documents: `(n, name, original SHA-256, signed SHA-256)`.
    more: Vec<(usize, String, String, String)>,
    /// Whether the file carries the workspace's digital seal.
    sealed: bool,
    events: Vec<String>,
    footer: String,
}

impl Certificate {
    fn new(
        doc: &Doc,
        trail: &[event::Row],
        settings: &settings::Settings,
        sender: Option<(String, String)>,
        completed_at: DateTime<Utc>,
        more: Vec<(usize, String, String, String)>,
        digital: Option<SealNote>,
    ) -> Certificate {
        let when = |key: &str| time(doc, key).map(human).unwrap_or_default();
        let number = s(doc, "number").unwrap_or_default().to_string();
        let sender = match sender {
            Some((name, email)) => format!("{name} <{email}>, {}", settings.sender_name),
            None => settings.sender_name.clone(),
        };
        let mut head = vec![
            ("Request".to_string(), number.clone()),
            ("Title".into(), s(doc, "title").unwrap_or_default().to_string()),
            ("Sender".into(), sender),
            ("Sent".into(), when("sent_at")),
            ("Completed".into(), human(completed_at)),
            ("Signing order".into(), s(doc, "signing_mode").unwrap_or("Sequential").to_string()),
            ("Document".into(), format!("{}, {} pages", s(doc, "document_name").unwrap_or("document.pdf"), int(doc, "page_count"))),
        ];
        if let (Some(e), Some(r)) = (s(doc, "reference_entity"), s(doc, "reference_id")) {
            head.push(("Linked record".into(), format!("{e} {r}")));
        }
        // What kind of signature this is, in plain words: nobody should take it for more than it is.
        head.push(("Kind of signature".into(), kind(digital.as_ref())));
        if let Some(d) = &digital {
            head.push((
                "Digital seal".into(),
                format!(
                    "Sealed with the workspace's key: {} · certificate SHA-256 {} · {}",
                    d.subject,
                    d.certificate_sha256,
                    if d.stamped { "time-stamped by a time-stamping authority" } else { "not time-stamped" }
                ),
            ));
        }
        if let Some(c) = &settings.consent_text {
            head.push(("Signers agreed to".into(), c.clone()));
        }
        let signers = rows(doc, "signers")
            .iter()
            .map(|r| {
                let who = format!("{} <{}>, {}", s(r, "name").unwrap_or_default(), s(r, "email").unwrap_or_default(), s(r, "role").unwrap_or_default());
                match s(r, "status") {
                    _ if crate::request::copy_only(r) => [who, "Received a copy; signed nothing".into(), String::new()],
                    Some("Not Needed") => [who, "Did not sign: another signer of their group signed".into(), String::new()],
                    _ => {
                        let method = if s(r, "signature_image").is_some() { "signature drawn" } else { "signature typed" };
                        let code = if s(r, "access_code_hash").is_some() { " · access code verified" } else { "" };
                        let identity = match s(r, "identity_channel") {
                            Some("email") => " · one-time code sent to their e-mail address confirmed",
                            Some("text") => " · one-time code sent to their phone confirmed",
                            _ => "",
                        };
                        let hosted = match s(r, "hosted_by") {
                            Some(host) if s(r, "delivery") == Some("In Person") => format!(" · signed in person, hosted by {host}"),
                            _ => String::new(),
                        };
                        [
                            who,
                            format!(
                                "Signed {} · {method}{code}{identity}{hosted} · agreed to sign electronically",
                                time(r, "signed_at").map(human).unwrap_or_default()
                            ),
                            format!("IP hash {} · {}", s(r, "ip_hash").unwrap_or("-"), s(r, "user_agent").unwrap_or("unknown device")),
                        ]
                    }
                }
            })
            .collect();
        let attached = rows(doc, "signers")
            .iter()
            .map(|r| {
                let uploads = r.get("attachments").and_then(Value::as_object).cloned().unwrap_or_default();
                let fields = rows(doc, "fields");
                // Only the file a field finally holds is listed, under the field's label.
                fields
                    .iter()
                    .filter(|f| s(f, "field_type") == Some("Attachment"))
                    .filter_map(|f| {
                        let u = uploads.get(s(f, "id")?)?;
                        (s(f, "value") == u.get("file").and_then(Value::as_str)).then(|| {
                            format!(
                                "{}: {} · {} bytes · SHA-256 {}",
                                s(f, "label").unwrap_or("Attachment"),
                                u.get("name").and_then(Value::as_str).unwrap_or("file"),
                                u.get("size").and_then(Value::as_i64).unwrap_or(0),
                                u.get("sha256").and_then(Value::as_str).unwrap_or_default()
                            )
                        })
                    })
                    .collect()
            })
            .collect();
        let events = trail
            .iter()
            .map(|e| {
                let mut line = format!("{}. {} · {}", e.seq, human(e.occurred_at), e.event);
                if let Some(who) = e.signer_email.as_deref() {
                    line.push_str(&format!(" · {} <{who}>", e.signer_name.as_deref().unwrap_or_default()));
                }
                if let Some(actor) = e.actor.as_deref().filter(|a| Some(*a) != e.signer_email.as_deref()) {
                    line.push_str(&format!(" · by {actor}"));
                }
                if let Some(ip) = e.ip_hash.as_deref() {
                    line.push_str(&format!(" · IP hash {}", &ip[..ip.len().min(12)]));
                }
                if let Some(d) = e.details.as_deref() {
                    line.push_str(&format!(" · {d}"));
                }
                line
            })
            .collect();
        let title: String = s(doc, "title").unwrap_or_default().chars().take(60).collect();
        Certificate {
            head,
            original_sha: s(doc, "document_sha256").unwrap_or_default().to_string(),
            trail_head: trail.last().and_then(|e| e.event_hash.clone()).unwrap_or_default(),
            signers,
            attached,
            more,
            sealed: digital.is_some(),
            events,
            footer: format!("{number} · {title}"),
        }
    }

    fn paras(&self, len: usize, sha: &str) -> Vec<Para> {
        let mut p = vec![Para::new("Certificate of Completion", 18.0), Para::new("Electronic signature record · Ferrum Sign", 9.0).muted().before(2.0)];
        p.push(Para::new("Request", 12.0).before(16.0));
        for (k, v) in &self.head {
            p.push(Para::new(format!("{k}: {v}"), 9.0).before(2.0));
        }
        p.push(Para::new("Document integrity", 12.0).before(14.0));
        p.push(Para::new(format!("Original document SHA-256: {}", self.original_sha), 9.0).before(2.0));
        p.push(Para::new(format!("Signed document SHA-256: {sha}"), 9.0).before(2.0));
        p.push(
            Para::new(
                format!(
                    "The signed document is the first {len} bytes of this file: the original pages with every signer's entries, before this certificate. \
                     Check it with: head -c {len} file.pdf | sha256sum"
                ),
                8.0,
            )
            .muted()
            .indent(12.0),
        );
        for (n, name, original, signed) in &self.more {
            p.push(Para::new(format!("Document {n}: {name}"), 9.0).before(6.0));
            p.push(Para::new(format!("Original SHA-256: {original}"), 8.5).before(1.0).indent(12.0));
            p.push(Para::new(format!("Signed SHA-256: {signed} (the whole file: it carries no certificate pages)"), 8.5).before(1.0).indent(12.0));
        }
        p.push(Para::new(format!("Audit trail SHA-256: {}", self.trail_head), 9.0).before(2.0));
        p.push(Para::new("The last link of the hash chain over the events below; verifying the request recomputes it.", 8.0).muted().indent(12.0));
        p.push(Para::new("Signers", 12.0).before(14.0));
        for (i, [who, how, device]) in self.signers.iter().enumerate() {
            p.push(Para::new(format!("{}. {who}", i + 1), 9.5).before(6.0));
            p.push(Para::new(how.clone(), 8.5).indent(12.0));
            p.push(Para::new(device.clone(), 8.0).muted().indent(12.0));
            for line in self.attached.get(i).into_iter().flatten() {
                p.push(Para::new(format!("Attached · {line}"), 8.0).muted().indent(12.0));
            }
        }
        p.push(Para::new("Events", 12.0).before(14.0));
        for e in &self.events {
            p.push(Para::new(e.clone(), 8.0).before(1.5));
        }
        p.push(Para::new("About this certificate", 11.0).before(14.0));
        p.push(
            Para::new(
                format!(
                    "Each signer opened the document through a personal link sent to their email address (and entered an access code where noted), \
                     confirmed that they agree to sign electronically, and signed. The signed pages above and this record of who signed, when and from \
                     which device (addresses kept as salted hashes) form an electronic signature with an audit trail. {} The document's integrity is \
                     shown by the SHA-256 values above, which Ferrum Sign recomputes when the request is verified.",
                    if self.sealed {
                        "The file as a whole then carries the workspace's digital seal (a PAdES signature that allows no further change), and the audit trail as an attachment."
                    } else {
                        "It is not a PAdES digital signature."
                    }
                ),
                8.0,
            )
            .muted()
            .before(2.0),
        );
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(consent: Option<&str>) -> settings::Settings {
        settings::Settings {
            sender_name: "Acme".into(),
            public_url: None,
            link_expiry_days: 30,
            reminder_every_days: 3,
            max_reminders: 3,
            bulk_per_day: 2000,
            timestamp_connection: None,
            brand: None,
            consent_text: consent.map(str::to_string),
        }
    }

    /// S11-G4: the sentence signers agreed to is on the certificate when the workspace worded it itself.
    #[test]
    fn the_certificate_prints_a_custom_consent_only() {
        let doc: Doc = serde_json::from_value(json!({"number": "SIG-1", "title": "T"})).unwrap();
        let line = |c: &Certificate| c.head.iter().find(|(k, _)| k == "Signers agreed to").map(|(_, v)| v.clone());
        assert_eq!(line(&Certificate::new(&doc, &[], &settings(Some("I accept Acme's terms.")), None, now(), vec![], None)).as_deref(), Some("I accept Acme's terms."));
        assert_eq!(line(&Certificate::new(&doc, &[], &settings(None), None, now(), vec![], None)), None);
    }

    fn request(fields: Value) -> Doc {
        let signers = json!([{"role": "Client", "name": "Cleo", "email": "cleo@client.test", "status": "Signed", "signature_text": "Cleo",
            "attachments": {"att": {"file": "f1", "name": "id.png", "size": 42, "sha256": "abc123", "uploads": 1}}}]);
        serde_json::from_value(json!({"number": "SIG-1", "title": "T", "signers": signers, "fields": fields})).unwrap()
    }

    /// S11-G2: a Radio ticks only its chosen box, a Dropdown and a Number show their text, an Attachment shows nothing
    /// on the pages and is listed, with its hash, on the certificate.
    #[test]
    fn a_radio_marks_only_its_chosen_box_and_an_attachment_is_listed() {
        let doc = request(json!([
            {"id": "r", "role": "Client", "field_type": "Radio", "page": 1, "x": 10, "y": 10, "width": 100, "height": 14, "value": "Blue",
             "options": ["Red", "Blue"], "option_boxes": [[10, 10, 14, 14], [60, 10, 14, 14]]},
            {"id": "d", "role": "Client", "field_type": "Dropdown", "page": 1, "x": 10, "y": 40, "width": 100, "height": 20, "value": "Pro", "options": ["Basic", "Pro"]},
            {"id": "n", "role": "Client", "field_type": "Number", "page": 1, "x": 10, "y": 70, "width": 100, "height": 20, "value": "7"},
            {"id": "att", "label": "ID card", "role": "Client", "field_type": "Attachment", "page": 1, "x": 10, "y": 100, "width": 100, "height": 20, "value": "f1"},
        ]));
        let stamps = stamps(&doc, 1).unwrap();
        assert_eq!(stamps.len(), 3, "the attachment has no stamp");
        assert!(matches!(stamps[0].mark, Mark::Check) && (stamps[0].x, stamps[0].width) == (60.0, 14.0), "the chosen box carries the tick");
        assert!(matches!(&stamps[1].mark, Mark::Text(t) if t == "Pro") && matches!(&stamps[2].mark, Mark::Text(t) if t == "7"));
        let cert = Certificate::new(&doc, &[], &settings(None), None, now(), vec![], None);
        assert_eq!(cert.attached, vec![vec!["ID card: id.png · 42 bytes · SHA-256 abc123".to_string()]]);
        let none = request(json!([{"id": "r", "role": "Client", "field_type": "Radio", "page": 1, "x": 10, "y": 10, "width": 100, "height": 14,
            "options": ["Red"], "option_boxes": [[10, 10, 14, 14]]}]));
        assert!(stamps_of(&none).is_empty(), "an unanswered radio marks nothing");
    }

    fn stamps_of(doc: &Doc) -> Vec<Stamp> {
        stamps(doc, 1).unwrap()
    }
}
