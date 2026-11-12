//! Signature from any record (DC-47): "send this file to these people for signature, about this record".
//!
//! A Sign Request made with `send_on_create` is sent as soon as it is made: from Ferrum's records API, from the
//! `sign.sign_request.create` capability (a configured button's or transition's `invoke`, after a `generate` that
//! made the file), or from [`crate::send_for_signature`]. Its document is a stored file; `source_sha256`, when
//! given, must be that file's digest (a generated document's `sha256`), so what is signed is what was generated.
//! Without fields of its own, a signature page is added after the document's last page with a box for each signer
//! (the original's bytes stay its prefix), and the fields are placed there. A file generated in the same transaction
//! (a configured button's `generate`, then its `invoke`) cannot be read until that transaction commits: the request
//! is made then as a draft, and upkeep adds the page and sends it within the minute. The request ends by its record's own
//! update (Completed, Declined, Expired), which automations on Sign Request take, and by the package's events
//! `sign.request_completed`, `sign.request_declined` and `sign.request_expired`, which carry the record it is about.

use crate::document::{self, page_sizes};
use crate::{doc_id, pdf, rows, s};
use ferrum_kernel::{Ctx, Doc, Error, HookCtx, Result, Tx};
use serde_json::{Value, json};
use uuid::Uuid;

/// Checks the file against `source_sha256`, takes the document as any draft does, and adds the signature page and
/// its fields when the request has none.
pub(crate) async fn prepare(hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
    let path = s(doc, "document").ok_or_else(|| Error::field("document", "name the file to sign"))?.to_string();
    let file = document::file_id(&path).ok_or_else(|| Error::field("document", "is a /files/<id> path"))?;
    if let Some(want) = s(doc, "source_sha256").map(|v| v.trim().to_lowercase()) {
        let have: Option<Option<String>> = sqlx::query_scalar("select sha256 from file_object where tenant_id = $1 and id = $2")
            .bind(hc.ctx.tenant)
            .bind(file)
            .fetch_optional(&mut **hc.tx)
            .await?;
        if have.flatten().map(|h| h.to_lowercase()) != Some(want) {
            return Err(Error::field("source_sha256", "the file is not the one generated: its SHA-256 differs"));
        }
    }
    // Not readable yet: made in this very transaction. Upkeep finishes it once it has committed. A page document
    // (what a server without a PDF service generates) is refused now, in the caller's transaction, so what asked
    // (a button that also changed a record's status) is undone with it instead of leaving a request that cannot go.
    let made: Option<(String, String)> = sqlx::query_as("select file_name, content_type from file_object where tenant_id = $1 and id = $2")
        .bind(hc.ctx.tenant)
        .bind(file)
        .fetch_optional(&mut **hc.tx)
        .await?;
    if made.is_some_and(|(name, kind)| {
        let name = name.to_lowercase();
        kind.starts_with("text/html") || name.ends_with(".html") || name.ends_with(".htm")
    }) {
        return Err(Error::field("document", pdf::NOT_A_PDF));
    }
    if document::bytes(hc.kernel, hc.ctx.tenant, file).await.is_err() && !doc.contains_key("created_at") {
        return Ok(());
    }
    document::derive(hc, doc).await?;
    if !rows(doc, "fields").is_empty() {
        return Ok(());
    }
    let signing: Vec<Doc> = rows(doc, "signers").into_iter().filter(|r| !crate::request::copy_only(r)).collect();
    if signing.len() > pdf::SIGNATURE_PAGE_HOLDS {
        return Err(Error::field("signers", format!("a signature page holds {} signers", pdf::SIGNATURE_PAGE_HOLDS)));
    }
    let size = page_sizes(doc).and_then(|p| p.last().copied()).ok_or_else(|| Error::field("document", "attach the PDF to sign"))?;
    let owned = s(doc, "document").and_then(document::file_id).ok_or_else(|| Error::internal("a document without a file"))?;
    let original = document::bytes(hc.kernel, hc.ctx.tenant, owned).await?;
    let labels: Vec<String> = signing.iter().map(|r| format!("{}: {}", s(r, "role").unwrap_or_default(), s(r, "name").unwrap_or_default())).collect();
    let bytes = tokio::task::spawn_blocking(move || pdf::with_signature_page(original, &labels, size))
        .await
        .map_err(|_| Error::field("document", "the signature page could not be added"))?
        .map_err(|m| Error::field("document", m))?;
    let name = s(doc, "document_name").unwrap_or("document.pdf").to_string();
    let stored = hc.kernel.store_file(hc.ctx, None, &name, "application/pdf", &bytes).await?;
    let new_file: Uuid = stored.get("id").and_then(Value::as_str).and_then(|v| v.parse().ok()).ok_or_else(|| Error::internal("stored file without id"))?;
    document::attach(hc.tx, hc.ctx.tenant, new_file, "SignRequest", doc_id(doc)?).await?;
    doc.insert("document".into(), json!(format!("/files/{new_file}")));
    doc.insert("page_count".into(), Value::Null);
    document::derive(hc, doc).await?;
    let page = doc.get("page_count").and_then(Value::as_i64).unwrap_or(1);
    let fields: Vec<Value> = signing
        .iter()
        .enumerate()
        .map(|(n, r)| {
            let (x, y) = pdf::signature_box(n, size.width);
            json!({"id": Uuid::now_v7().to_string(), "role": s(r, "role"), "field_type": "Signature", "label": format!("{} signature", s(r, "role").unwrap_or_default()),
                   "required": true, "page": page, "x": x, "y": y, "width": 200, "height": 50})
        })
        .collect();
    doc.insert("fields".into(), Value::Array(fields));
    doc.insert("signature_page".into(), json!(page));
    Ok(())
}

/// Requests made to be sent whose document was not readable when they were made: their signature page added and
/// sent now. One that cannot be sent stays a draft for its sender, told why once.
pub(crate) async fn send_pending(k: &ferrum_kernel::Kernel, ctx: &Ctx) -> Result<usize> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "select id from e_sign_request where tenant_id = $1 and status = 'Draft' and coalesce(send_on_create, false) order by created_at limit 100",
    )
    .bind(ctx.tenant)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut sent = 0;
    for id in ids {
        let mut tx = k.db.tenant_tx(ctx.tenant).await?;
        match k.update_tx(ctx, &mut tx, "SignRequest", id, Doc::new(), None).await {
            Ok(_) => {
                tx.commit().await?;
                sent += 1;
            }
            Err(e) => {
                tx.rollback().await?;
                let mut tx = k.db.tenant_tx(ctx.tenant).await?;
                let doc = k.get_tx(ctx, &mut tx, "SignRequest", id, false).await?;
                k.set_values_tx(ctx, &mut tx, "SignRequest", id, crate::request::obj(json!({"send_on_create": false}))).await?;
                let title = format!("{} could not be sent", s(&doc, "number").unwrap_or_default());
                crate::notice::notify_sender(&mut tx, ctx.tenant, &doc, &title, Some(&e.to_string())).await?;
                // For a process or automation waiting on it, as for one that ended.
                let payload = json!({"request": s(&doc, "id"), "number": s(&doc, "number"), "reason": e.to_string(),
                                     "reference_entity": s(&doc, "reference_entity"), "reference_id": s(&doc, "reference_id")});
                ferrum_kernel::outbox::enqueue(&mut tx, ctx.tenant, &format!("{}.request_failed", crate::PACKAGE), Some("SignRequest"), Some(id), payload)
                    .await?;
                tx.commit().await?;
            }
        }
    }
    Ok(sent)
}

/// Says on the outbox that a request about a record ended, for processes waiting on it.
pub(crate) async fn ended(tx: &mut Tx, ctx: &Ctx, request: &Doc, status: &str) -> Result<()> {
    let name = match status {
        "Completed" => "request_completed",
        "Declined" => "request_declined",
        "Expired" => "request_expired",
        _ => return Ok(()),
    };
    let payload = json!({"request": s(request, "id"), "number": s(request, "number"), "status": status,
                         "reference_entity": s(request, "reference_entity"), "reference_id": s(request, "reference_id"),
                         "sealed_document": s(request, "sealed_document")});
    ferrum_kernel::outbox::enqueue(tx, ctx.tenant, &format!("{}.{name}", crate::PACKAGE), Some("SignRequest"), Some(doc_id(request)?), payload).await
}
