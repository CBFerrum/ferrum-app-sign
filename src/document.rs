//! The PDF a template or request is about: reading the uploaded file with the caller's access,
//! measuring its pages, making the file belong to the record (so its access follows the record)
//! and checking field boxes against the pages.

use crate::pdf::{self, PageSize};
use crate::{doc_id, int, rows, s};
use ferrum_kernel::{Ctx, Doc, Error, FieldError, HookCtx, Kernel, Result, Tx};
use rust_decimal::prelude::ToPrimitive;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use uuid::Uuid;

/// The file id in a `/files/<id>` path.
pub fn file_id(path: &str) -> Option<Uuid> {
    path.trim().strip_prefix("/files/")?.parse().ok()
}

struct FileRow {
    id: Uuid,
    name: String,
    entity: Option<String>,
    record: Option<Uuid>,
    created_by: Option<Uuid>,
}

/// A stored file's bytes, read with engine rights (callers check access first).
pub async fn bytes(k: &Kernel, tenant: Uuid, file: Uuid) -> Result<Vec<u8>> {
    let sys = Ctx::system(tenant, k.registry.schema(tenant).await?);
    Ok(k.read_file(&sys, file).await?.2)
}

/// Attaches a stored file to a record: it lists with it and follows its access rules.
pub async fn attach(tx: &mut Tx, tenant: Uuid, file: Uuid, entity: &str, record: Uuid) -> Result<()> {
    sqlx::query("update file_object set entity = $3, record_id = $4 where tenant_id = $1 and id = $2")
        .bind(tenant)
        .bind(file)
        .bind(entity)
        .bind(record)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Hands a file to an audit event: it then follows the event's access rules and, with no uploader
/// any more, only someone who may edit audit events (an administrator) can delete it — not the
/// sender, who is a party to the signature.
pub async fn freeze(tx: &mut Tx, tenant: Uuid, file: Uuid, event: Uuid) -> Result<()> {
    sqlx::query("update file_object set entity = 'SignEvent', record_id = $3, created_by = null where tenant_id = $1 and id = $2")
        .bind(tenant)
        .bind(file)
        .bind(event)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// When `document` changed: checks it is a PDF the caller may use (their own upload, or a file
/// of a record they can read), records its name, SHA-256 and page sizes, and makes the file the
/// record's own — the caller's unattached upload is attached, anything else is copied.
pub async fn derive(hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
    let Some(path) = s(doc, "document").map(str::to_string) else { return Ok(()) };
    let record = doc_id(doc)?;
    let tenant = hc.ctx.tenant;
    let stored: Option<Option<String>> =
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select document from {} where tenant_id = $1 and id = $2", ferrum_kernel::ddl::q(&hc.entity.table))))
            .bind(tenant)
            .bind(record)
            .fetch_optional(&mut **hc.tx)
            .await?;
    if stored.flatten().as_deref() == Some(path.as_str()) && doc.get("page_count").is_some_and(|v| !v.is_null()) {
        return Ok(());
    }
    let bad = |m: &str| Error::field("document", m);
    let unusable = || bad("the file does not exist or you may not use it");
    let file = file_id(&path).ok_or_else(|| bad("upload the PDF with POST /api/files and use its /files/<id> path"))?;
    let row: Option<(String, Option<String>, Option<Uuid>, Option<Uuid>)> =
        sqlx::query_as("select file_name, entity, record_id, created_by from file_object where tenant_id = $1 and id = $2")
            .bind(tenant)
            .bind(file)
            .fetch_optional(&mut **hc.tx)
            .await?;
    let (name, entity, rec, created_by) = row.ok_or_else(unusable)?;
    let row = FileRow { id: file, name, entity, record: rec, created_by };
    let readable = match (&row.entity, row.record) {
        (Some(e), Some(r)) => hc.ctx.is_system || (hc.ctx.schema.entity(e).is_some() && hc.kernel.get_tx(hc.ctx, hc.tx, e, r, false).await.is_ok()),
        _ => hc.ctx.is_system || (!hc.ctx.user.is_nil() && row.created_by == Some(hc.ctx.user)),
    };
    if !readable {
        return Err(unusable());
    }
    let data = bytes(hc.kernel, tenant, file).await.map_err(|_| bad("the file cannot be read"))?;
    // Parsing untrusted PDFs is CPU work: keep it off the async workers, and a parser panic
    // becomes a refusal instead of a crash.
    let (data, pages) = tokio::task::spawn_blocking(move || {
        let pages = pdf::inspect(&data);
        (data, pages)
    })
    .await
    .map_err(|_| bad("the PDF cannot be read"))?;
    let pages = pages.map_err(|m| bad(&m))?;
    let owned = own(hc, &row, &data, record).await?;
    doc.insert("document".into(), json!(format!("/files/{owned}")));
    doc.insert("document_name".into(), json!(crate::clean(&row.name, 200)));
    doc.insert("document_sha256".into(), json!(hex::encode(Sha256::digest(&data))));
    doc.insert("page_count".into(), json!(pages.len()));
    doc.insert("page_sizes".into(), json!(pages.iter().map(|p| [p.width, p.height]).collect::<Vec<_>>()));
    Ok(())
}

async fn own(hc: &mut HookCtx<'_>, row: &FileRow, data: &[u8], record: Uuid) -> Result<Uuid> {
    let entity = hc.entity.def.name.clone();
    match (&row.entity, row.record) {
        (Some(e), Some(r)) if *e == entity && r == record => Ok(row.id),
        (None, _) | (_, None) if !hc.ctx.user.is_nil() && row.created_by == Some(hc.ctx.user) => {
            attach(hc.tx, hc.ctx.tenant, row.id, &entity, record).await?;
            Ok(row.id)
        }
        _ => {
            let stored = hc.kernel.store_file(hc.ctx, None, &row.name, "application/pdf", data).await?;
            let copy = stored.get("id").and_then(|v| v.as_str()).and_then(|v| v.parse().ok()).ok_or_else(|| Error::internal("stored file without id"))?;
            attach(hc.tx, hc.ctx.tenant, copy, &entity, record).await?;
            Ok(copy)
        }
    }
}

/// Signer roles as compared: trimmed, case-insensitive.
pub fn role_key(role: &str) -> String {
    role.trim().to_lowercase()
}

/// Displayed page sizes recorded by [`derive`].
pub fn page_sizes(doc: &Doc) -> Option<Vec<PageSize>> {
    doc.get("page_sizes")?
        .as_array()?
        .iter()
        .map(|p| {
            let a = p.as_array()?;
            Some(PageSize { width: a.first()?.as_f64()?, height: a.get(1)?.as_f64()? })
        })
        .collect()
}

/// A decimal field as a float (positions need no exact arithmetic).
pub fn coord(row: &Doc, key: &str) -> Option<f64> {
    row.get(key).and_then(|v| ferrum_kernel::value::parse_decimal(v).ok()).and_then(|d| d.to_f64())
}

/// Every field names an existing role and lies on an existing page, inside it.
pub fn check_fields(doc: &Doc, roles: &HashSet<String>, errors: &mut Vec<FieldError>) {
    let pages = page_sizes(doc);
    for (i, f) in rows(doc, "fields").iter().enumerate() {
        let at = |field: &str, m: String| FieldError::new(field, m).in_row("fields", i);
        let role = s(f, "role").unwrap_or_default();
        if !role.trim().is_empty() && !roles.contains(&role_key(role)) {
            errors.push(at("role", format!("no signer has the role {}", role.trim())));
        }
        let Some(pages) = &pages else { continue };
        let page = int(f, "page");
        if page < 1 || page as usize > pages.len() {
            errors.push(at("page", format!("page {page} does not exist; the document has {} pages", pages.len())));
            continue;
        }
        let p = pages[page as usize - 1];
        if let (Some(x), Some(y), Some(w), Some(h)) = (coord(f, "x"), coord(f, "y"), coord(f, "width"), coord(f, "height"))
            && (x + w > p.width + 0.01 || y + h > p.height + 0.01)
        {
            errors.push(at("x", format!("the field reaches outside page {page}, which is {} × {} points", p.width, p.height)));
        }
    }
}

/// Roles must be filled in and distinct; returns them as compared.
pub fn check_roles(doc: &Doc, table: &str, errors: &mut Vec<FieldError>) -> HashSet<String> {
    let mut seen = HashSet::new();
    for (i, r) in rows(doc, table).iter().enumerate() {
        let key = role_key(s(r, "role").unwrap_or_default());
        if !key.is_empty() && !seen.insert(key) {
            errors.push(FieldError::new("role", "two rows have the same role").in_row(table, i));
        }
    }
    seen
}
