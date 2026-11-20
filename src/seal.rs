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
use serde_json::json;
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
    tx.commit().await?;
    // An old Completed request keeps the moment it completed.
    let completed_at = if legacy(&doc) { time(&doc, "completed_at").unwrap_or_else(now) } else { now() };
    let built = if ready(&doc) {
        build(k, &sys, &doc, &trail, &settings, sender, completed_at).await
    } else {
        Err(Error::invalid("the request is Completed without every signature; a person must look at it"))
    };
    let (file, sealed) = match built {
        Ok(v) => v,
        Err(e) => {
            failed(k, &sys, &doc, &e).await;
            return Err(e);
        }
    };
    let sha = hex::encode(Sha256::digest(&sealed.bytes));
    let details =
        format!("Sealed PDF SHA-256 {sha} ({} pages); signed document SHA-256 {} (its first {} bytes)", sealed.pages, sealed.signed_sha256, sealed.signed_len);
    let done = with_retry(|| complete(k, &sys, request, file, &sha, &details, completed_at)).await;
    match done {
        Ok(true) => Ok(true),
        Ok(false) => {
            // Sealed meanwhile by someone else: drop this copy's record.
            let _ = k.delete_file(&sys, file).await;
            Ok(false)
        }
        Err(e) => {
            failed(k, &sys, &doc, &e).await;
            Err(e)
        }
    }
}

/// Renders and stores the sealed PDF (not yet attached to anything).
async fn build(
    k: &Kernel,
    sys: &Ctx,
    doc: &Doc,
    trail: &[event::Row],
    settings: &settings::Settings,
    sender: Option<(String, String)>,
    completed_at: DateTime<Utc>,
) -> Result<(Uuid, pdf::Sealed)> {
    let file = s(doc, "document").and_then(document::file_id).ok_or_else(|| Error::invalid("the request has no document"))?;
    let original = document::bytes(k, sys.tenant, file).await.map_err(|e| Error::invalid(format!("the original PDF cannot be read: {e}")))?;
    if Some(hex::encode(Sha256::digest(&original)).as_str()) != s(doc, "document_sha256") {
        return Err(Error::invalid("the stored original no longer matches its SHA-256; it was changed after sending"));
    }
    let stamps = stamps(doc).map_err(Error::invalid)?;
    let cert = Certificate::new(doc, trail, settings, sender, completed_at);
    let footer = cert.footer.clone();
    let sealed = tokio::task::spawn_blocking(move || pdf::seal(&original, &stamps, &footer, &|len, sha| cert.paras(len, sha)))
        .await
        .map_err(|_| Error::internal("sealing the PDF failed"))?
        .map_err(Error::invalid)?;
    let title: String = s(doc, "title").unwrap_or("document").chars().filter(|c| c.is_alphanumeric() || " -_.,()".contains(*c)).take(100).collect();
    let name = format!("{} - signed.pdf", if title.trim().is_empty() { s(doc, "number").unwrap_or("document") } else { title.trim() });
    let stored = k.store_file(sys, None, &name, "application/pdf", &sealed.bytes).await?;
    let id = stored.get("id").and_then(|v| v.as_str()).and_then(|v| v.parse().ok()).ok_or_else(|| Error::internal("stored file without id"))?;
    Ok((id, sealed))
}

/// Completes the request with the stored sealed PDF, if it is still ready.
async fn complete(k: &Kernel, sys: &Ctx, request: Uuid, file: Uuid, sha: &str, details: &str, at: DateTime<Utc>) -> Result<bool> {
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

/// The kind of signature every document sealed here carries (DC-43): what it is and what it is not.
pub const KIND: &str = "Simple electronic signature with an audit trail. Not an advanced or qualified electronic signature, and not a \
                        PAdES digital signature: no signing certificate, signature device or timestamp authority was used.";

/// What each field shows on the sealed pages.
pub fn stamps(doc: &Doc) -> std::result::Result<Vec<Stamp>, String> {
    let signers: HashMap<String, Doc> = rows(doc, "signers").into_iter().map(|r| (role_key(s(&r, "role").unwrap_or_default()), r)).collect();
    let mut out = Vec::new();
    for f in rows(doc, "fields") {
        let Some(sg) = signers.get(&role_key(s(&f, "role").unwrap_or_default())) else { continue };
        let ink = |kind: &str| -> std::result::Result<Option<Mark>, String> {
            if let Some(url) = s(sg, &format!("{kind}_image")) {
                return Ok(Some(Mark::Image(signature::decode(&signature::png_bytes(url)?)?)));
            }
            Ok(s(sg, &format!("{kind}_text")).map(|t| Mark::Typed(t.to_string())))
        };
        let mark = match s(&f, "field_type") {
            Some("Signature") => ink("signature")?,
            Some("Initials") => ink("initials")?,
            Some("Checkbox") => (s(&f, "value") == Some("Yes")).then_some(Mark::Check),
            _ => s(&f, "value").map(|v| Mark::Text(v.to_string())),
        };
        let (Some(mark), Some(x), Some(y), Some(width), Some(height)) = (mark, coord(&f, "x"), coord(&f, "y"), coord(&f, "width"), coord(&f, "height")) else {
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
    events: Vec<String>,
    footer: String,
}

impl Certificate {
    fn new(doc: &Doc, trail: &[event::Row], settings: &settings::Settings, sender: Option<(String, String)>, completed_at: DateTime<Utc>) -> Certificate {
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
        head.push(("Kind of signature".into(), KIND.into()));
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
        p.push(Para::new(format!("Audit trail SHA-256: {}", self.trail_head), 9.0).before(2.0));
        p.push(Para::new("The last link of the hash chain over the events below; verifying the request recomputes it.", 8.0).muted().indent(12.0));
        p.push(Para::new("Signers", 12.0).before(14.0));
        for (i, [who, how, device]) in self.signers.iter().enumerate() {
            p.push(Para::new(format!("{}. {who}", i + 1), 9.5).before(6.0));
            p.push(Para::new(how.clone(), 8.5).indent(12.0));
            p.push(Para::new(device.clone(), 8.0).muted().indent(12.0));
        }
        p.push(Para::new("Events", 12.0).before(14.0));
        for e in &self.events {
            p.push(Para::new(e.clone(), 8.0).before(1.5));
        }
        p.push(Para::new("About this certificate", 11.0).before(14.0));
        p.push(
            Para::new(
                "Each signer opened the document through a personal link sent to their email address (and entered an access code where noted), \
                 confirmed that they agree to sign electronically, and signed. The signed pages above and this record of who signed, when and from \
                 which device (addresses kept as salted hashes) form an electronic signature with an audit trail. It is not a PAdES digital \
                 signature: the document's integrity is shown by the SHA-256 values above, which Ferrum Sign recomputes when the request is verified.",
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
            brand: None,
            consent_text: consent.map(str::to_string),
        }
    }

    /// S11-G4: the sentence signers agreed to is on the certificate when the workspace worded it itself.
    #[test]
    fn the_certificate_prints_a_custom_consent_only() {
        let doc: Doc = serde_json::from_value(json!({"number": "SIG-1", "title": "T"})).unwrap();
        let line = |c: &Certificate| c.head.iter().find(|(k, _)| k == "Signers agreed to").map(|(_, v)| v.clone());
        assert_eq!(line(&Certificate::new(&doc, &[], &settings(Some("I accept Acme's terms.")), None, now())).as_deref(), Some("I accept Acme's terms."));
        assert_eq!(line(&Certificate::new(&doc, &[], &settings(None), None, now())), None);
    }
}
