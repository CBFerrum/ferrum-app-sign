//! The PDF a template or request is about: reading the uploaded file with the caller's access,
//! measuring its pages, making the file belong to the record (so its access follows the record)
//! and checking field boxes against the pages.

use crate::pdf::{self, PageSize};
use crate::{doc_id, int, rows, s};
use ferrum_kernel::{Ctx, Doc, Error, FieldError, HookCtx, Kernel, Result, Tx};
use rust_decimal::prelude::ToPrimitive;
use serde_json::{Value, json};
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
/// record's own — the caller's unattached upload is attached, anything else is copied. Then the same for each of the
/// request's more documents.
pub async fn derive(hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
    let record = doc_id(doc)?;
    let stored: Option<Option<String>> =
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select document from {} where tenant_id = $1 and id = $2", ferrum_kernel::ddl::q(&hc.entity.table))))
            .bind(hc.ctx.tenant)
            .bind(record)
            .fetch_optional(&mut **hc.tx)
            .await?;
    derive_file(hc, doc, record, stored.flatten(), "document").await?;
    // More documents: each row the same way (a row already saved with the same file keeps what was derived).
    let mut more = rows(doc, "more_documents");
    if more.len() > 19 {
        return Err(Error::field("more_documents", "a request has at most 20 documents"));
    }
    if more.is_empty() {
        return Ok(());
    }
    let table = crate::child_table(hc.ctx, "SignRequest", "more_documents")?;
    for (i, row) in more.iter_mut().enumerate() {
        let row_id = crate::id(row, "id");
        let stored: Option<Option<String>> = match row_id {
            Some(rid) => {
                sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select document from {table} where tenant_id = $1 and id = $2")))
                    .bind(hc.ctx.tenant)
                    .bind(rid)
                    .fetch_optional(&mut **hc.tx)
                    .await?
            }
            None => None,
        };
        derive_file(hc, row, record, stored.flatten(), "document").await.map_err(|e| match e {
            Error::Validation(v) => Error::Validation(v.into_iter().map(|f| f.in_row("more_documents", i)).collect()),
            other => other,
        })?;
    }
    doc.insert("more_documents".into(), Value::Array(more.into_iter().map(Value::Object).collect()));
    Ok(())
}

/// One document's file (`doc["document"]`, in `record`'s name): checked and measured, as [`derive`] says.
async fn derive_file(hc: &mut HookCtx<'_>, doc: &mut Doc, record: Uuid, stored: Option<String>, key: &str) -> Result<()> {
    let Some(path) = s(doc, key).map(str::to_string) else { return Ok(()) };
    let tenant = hc.ctx.tenant;
    if stored.as_deref() == Some(path.as_str()) && doc.get("page_count").is_some_and(|v| !v.is_null()) {
        return Ok(());
    }
    let bad = |m: &str| Error::field(key, m);
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
    doc.insert(key.into(), json!(format!("/files/{owned}")));
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

/// One document of a request, as the engine reads it: `1` is the main document (the request's own `document` columns),
/// `2…` are its `more_documents` rows in order.
pub struct DocRef {
    pub n: usize,
    pub file: Uuid,
    pub name: String,
    pub sha256: String,
    pub pages: Vec<PageSize>,
    /// The row of `more_documents` (none for the main document).
    pub row: Option<Uuid>,
    pub sealed_document: Option<String>,
    pub sealed_sha256: Option<String>,
}

/// Every document of the request, the main one first; a row without a readable file is left out.
pub fn documents(doc: &Doc) -> Vec<DocRef> {
    let one = |n: usize, d: &Doc, row: Option<Uuid>| {
        Some(DocRef {
            n,
            file: s(d, "document").and_then(file_id)?,
            name: s(d, "document_name").unwrap_or("document.pdf").to_string(),
            sha256: s(d, "document_sha256").unwrap_or_default().to_string(),
            pages: page_sizes(d).unwrap_or_default(),
            row,
            sealed_document: s(d, "sealed_document").map(str::to_string),
            sealed_sha256: s(d, "sealed_sha256").map(str::to_string),
        })
    };
    let mut out: Vec<DocRef> = one(1, doc, None).into_iter().collect();
    for (i, r) in rows(doc, "more_documents").iter().enumerate() {
        out.extend(one(i + 2, r, crate::id(r, "id")));
    }
    out
}

/// Which document a field is on.
pub fn document_no(f: &Doc) -> usize {
    int(f, "document_no").max(1) as usize
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

/// The kinds of file an Attachment field may take.
pub const ACCEPT: [&str; 3] = ["pdf", "png", "jpg"];

/// What a Dropdown or Radio offers.
pub fn choices(f: &Doc) -> Vec<String> {
    f.get("options").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// What an Attachment field takes: its `accept` list, else every kind.
pub fn accepted(f: &Doc) -> Vec<String> {
    let listed: Vec<String> = s(f, "accept").unwrap_or_default().split(',').map(|k| k.trim().to_lowercase()).filter(|k| !k.is_empty()).collect();
    if listed.is_empty() { ACCEPT.iter().map(|k| k.to_string()).collect() } else { listed }
}

/// A Radio's boxes, in the order of its choices: `[x, y, width, height]` each.
pub fn option_boxes(f: &Doc) -> Vec<[f64; 4]> {
    let one = |b: &Value| {
        let a = b.as_array().filter(|a| a.len() == 4)?;
        Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?, a[3].as_f64()?])
    };
    f.get("option_boxes").and_then(Value::as_array).map(|a| a.iter().filter_map(one).collect()).unwrap_or_default()
}

/// What a field of one of the richer types must carry: its choices, boxes, bounds or kinds of file.
fn check_kind(f: &Doc, at: &dyn Fn(&str, String) -> FieldError, page: Option<PageSize>, errors: &mut Vec<FieldError>) {
    let kind = s(f, "field_type").unwrap_or_default();
    match kind {
        "Dropdown" | "Radio" => {
            let given = f.get("options").and_then(Value::as_array);
            let n = given.map_or(0, Vec::len);
            let texts = choices(f);
            let mut seen = HashSet::new();
            if n == 0 || n > 50 || texts.len() != n {
                errors.push(at("options", "give 1 to 50 choices, each a text".into()));
            } else if texts.iter().any(|o| o.trim().is_empty() || o.chars().count() > 100) {
                errors.push(at("options", "each choice is 1 to 100 characters".into()));
            } else if !texts.iter().all(|o| seen.insert(o.trim().to_lowercase())) {
                errors.push(at("options", "two choices are the same".into()));
            } else if kind == "Radio" {
                let boxes = option_boxes(f);
                let listed = f.get("option_boxes").and_then(Value::as_array).map_or(0, Vec::len);
                if boxes.len() != n || listed != n {
                    errors.push(at("option_boxes", "a Radio needs one box [x, y, width, height] for each choice".into()));
                } else if let Some(p) = page
                    && boxes.iter().any(|[x, y, w, h]| *x < 0.0 || *y < 0.0 || *w < 4.0 || *h < 4.0 || x + w > p.width + 0.01 || y + h > p.height + 0.01)
                {
                    errors.push(at("option_boxes", "each box lies inside the page and is at least 4 points wide and high".into()));
                }
            }
        }
        "Number" => {
            let bound = |key: &str| s(f, key).map(|v| ferrum_kernel::value::parse_decimal(&Value::String(v.to_string())).map_err(|_| key.to_string()));
            let (min, max) = (bound("min_value"), bound("max_value"));
            for b in [&min, &max].into_iter().flatten() {
                if let Err(key) = b {
                    errors.push(at(key, "is not a number".into()));
                }
            }
            if let (Some(Ok(a)), Some(Ok(b))) = (&min, &max)
                && a > b
            {
                errors.push(at("min_value", "is larger than the largest value".into()));
            }
        }
        "Attachment" if accepted(f).iter().any(|k| !ACCEPT.contains(&k.as_str())) => {
            errors.push(at("accept", "list kinds from pdf, png, jpg".into()));
        }
        _ => {}
    }
}

/// Every field names an existing role and lies on an existing page, inside it; a Dropdown, Radio, Number or
/// Attachment carries what it needs (`check_kind`).
pub fn check_fields(doc: &Doc, roles: &HashSet<String>, errors: &mut Vec<FieldError>) {
    let docs = documents(doc);
    let more = rows(doc, "more_documents").len();
    for (i, f) in rows(doc, "fields").iter().enumerate() {
        let at = |field: &str, m: String| FieldError::new(field, m).in_row("fields", i);
        let n = document_no(f);
        if n > more + 1 {
            errors.push(at("document_no", format!("the request has {} document{}, not {n}", more + 1, if more == 0 { "" } else { "s" })));
            continue;
        }
        // The pages of the document the box is on (none yet while that file is not read).
        let pages = docs.iter().find(|d| d.n == n).map(|d| d.pages.clone()).filter(|p| !p.is_empty());
        let role = s(f, "role").unwrap_or_default();
        if !role.trim().is_empty() && !roles.contains(&role_key(role)) {
            errors.push(at("role", format!("no signer has the role {}", role.trim())));
        }
        let page = int(f, "page");
        let on = pages.as_ref().filter(|p| page >= 1 && page as usize <= p.len()).map(|p| p[page as usize - 1]);
        check_kind(f, &at, on, errors);
        let Some(pages) = &pages else { continue };
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
