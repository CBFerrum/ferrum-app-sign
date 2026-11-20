//! Bulk send: one template sent to many recipients, one request each. A `SignBulkSend` is made (its recipients are rows of
//! it), started with `start` (every row checked at once), and then sent by upkeep, a few every minute, each row in a
//! transaction of its own under the bulk's and the row's locks: a recipient gets one request however often upkeep runs,
//! and a row that fails is recorded without stopping the others. The requests are made as the person who made the bulk
//! send, with their rights as they are now.

use crate::request::{check_reference, obj};
use crate::{child_table, clean, doc_id, id, rows, s, settings};
use async_trait::async_trait;
use ferrum_kernel::{Ctx, Doc, DocHook, Error, FieldError, HookCtx, Kernel, Result, with_retry};
use serde_json::{Map, Value, json};
use uuid::Uuid;

/// Rows a bulk send may hold.
const MAX_ROWS: usize = 1000;
/// Rows sent per bulk send and run (upkeep runs every minute).
const PER_RUN: i64 = 50;
/// Failed rows keep their people this long after the bulk send last changed, for a person to read why.
const KEEP_FAILED_DAYS: i64 = 30;

pub struct BulkHook;

#[async_trait]
impl DocHook for BulkHook {
    async fn before_validate(&self, hc: &mut HookCtx<'_>, doc: &mut Doc) -> Result<()> {
        if !doc.contains_key("created_at") {
            if s(doc, "sender_email").is_none() && !hc.ctx.user.is_nil() {
                doc.insert("sender_email".into(), json!(hc.ctx.email.to_lowercase()));
            }
            // Rows are numbered in the order given.
            let mut list = rows(doc, "recipients");
            if list.len() > MAX_ROWS {
                return Err(Error::field("recipients", format!("a bulk send has at most {MAX_ROWS} recipients")));
            }
            for (i, r) in list.iter_mut().enumerate() {
                r.insert("row_no".into(), json!(i + 1));
            }
            doc.insert("recipients".into(), Value::Array(list.into_iter().map(Value::Object).collect()));
        }
        Ok(())
    }

    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, _params: &Value) -> Result<Option<Value>> {
        match name {
            "start" => start(hc, doc).await.map(Some),
            "cancel" => cancel(hc, doc).await.map(Some),
            _ => Ok(None),
        }
    }
}

/// One recipient's people, as `role key → (name, email)`, from the row's JSON.
fn people(row: &Doc) -> std::result::Result<Vec<(String, String, String)>, String> {
    let raw = s(row, "signers").ok_or("name the people to sign")?;
    let v: Value = serde_json::from_str(raw).map_err(|_| "the people are not a JSON object of role → {name, email}".to_string())?;
    let m = v.as_object().ok_or("the people are not a JSON object of role → {name, email}")?;
    let mut out = Vec::new();
    for (role, who) in m {
        let who = who.as_object().ok_or_else(|| format!("{role}: give {{name, email}}"))?;
        let email = who.get("email").and_then(Value::as_str).map(str::trim).unwrap_or_default();
        let name = who.get("name").and_then(Value::as_str).map(str::trim).unwrap_or_default();
        if !looks_like_email(email) {
            return Err(format!("{role}: the email address is not valid"));
        }
        if name.is_empty() {
            return Err(format!("{role}: a name is needed"));
        }
        out.push((role.clone(), name.to_string(), email.to_string()));
    }
    Ok(out)
}

fn looks_like_email(e: &str) -> bool {
    let mut parts = e.split('@');
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if !a.is_empty() && b.contains('.') && !b.starts_with('.') && !b.ends_with('.') && !e.contains(char::is_whitespace))
}

/// Checks every row against the template and the caller, then sets `Sending`.
async fn start(hc: &mut HookCtx<'_>, doc: &Doc) -> Result<Value> {
    let (k, ctx) = (hc.kernel, hc.ctx);
    ctx.require("SignRequest", ferrum_kernel::Action::Create)?;
    let list = rows(doc, "recipients");
    if list.is_empty() || list.len() > MAX_ROWS {
        return Err(Error::field("recipients", format!("add 1 to {MAX_ROWS} recipients")));
    }
    let template = s(doc, "template").and_then(|t| t.parse::<Uuid>().ok()).ok_or_else(|| Error::field("template", "choose the template to send"))?;
    // Read as the caller: a template they may not read cannot be sent.
    let template = k.get_tx(ctx, hc.tx, "SignTemplate", template, false).await.map_err(|_| Error::field("template", "choose a template you may use"))?;
    if crate::b(&template, "disabled") {
        return Err(Error::field("template", "this template is disabled"));
    }
    let roles: Vec<String> = rows(&template, "roles").iter().map(|r| crate::document::role_key(s(r, "role").unwrap_or_default())).collect();
    let mut errors = Vec::new();
    for (i, row) in list.iter().enumerate() {
        let at = |field: &str, m: String| FieldError::new(field, m).in_row("recipients", i);
        match people(row) {
            Err(m) => errors.push(at("signers", m)),
            Ok(p) => {
                let given: Vec<String> = p.iter().map(|(r, _, _)| crate::document::role_key(r)).collect();
                if let Some(r) = given.iter().find(|r| !roles.contains(r)) {
                    errors.push(at("signers", format!("the template has no role {r}")));
                } else if let Some(r) = roles.iter().find(|r| !given.contains(r)) {
                    errors.push(at("signers", format!("the role {r} is missing")));
                }
            }
        }
        let mut input = Doc::new();
        for key in ["reference_entity", "reference_id"] {
            if let Some(v) = row.get(key).filter(|v| !v.is_null()) {
                input.insert(key.into(), v.clone());
            }
        }
        let mut found = Vec::new();
        check_reference(k, ctx, hc.tx, &input, &mut found).await;
        errors.extend(found.into_iter().map(|e| at(&e.field, e.message)));
    }
    if !errors.is_empty() {
        return Err(Error::Validation(errors));
    }
    k.set_values_tx(ctx, hc.tx, "SignBulkSend", doc_id(doc)?, obj(json!({"status": "Sending", "sent": 0, "failed": 0}))).await?;
    Ok(json!({"status": "Sending", "rows": list.len()}))
}

/// Stops further rows; the people of the rows not yet sent are forgotten in the same transaction.
async fn cancel(hc: &mut HookCtx<'_>, doc: &Doc) -> Result<Value> {
    let (k, ctx) = (hc.kernel, hc.ctx);
    let table = child_table(ctx, "SignBulkSend", "recipients")?;
    // Under the bulk's lock, so a row being sent finishes first and the rest are never started.
    sqlx::query("select 1 from e_sign_bulk_send where tenant_id = $1 and id = $2 for update").bind(ctx.tenant).bind(doc_id(doc)?).execute(&mut **hc.tx).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("update {table} set signers = null where tenant_id = $1 and parent_id = $2 and status = 'Waiting'")))
        .bind(ctx.tenant)
        .bind(doc_id(doc)?)
        .execute(&mut **hc.tx)
        .await?;
    k.set_values_tx(ctx, hc.tx, "SignBulkSend", doc_id(doc)?, obj(json!({"status": "Cancelled"}))).await?;
    Ok(json!({"status": "Cancelled"}))
}

/// Upkeep: sends the next rows of every bulk send that is sending. Returns how many requests were made.
pub async fn send_due(k: &Kernel, ctx: &Ctx) -> Result<usize> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let bulks: Vec<Uuid> = sqlx::query_scalar("select id from e_sign_bulk_send where tenant_id = $1 and status = 'Sending' order by updated_at limit 10")
        .bind(ctx.tenant)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    let mut made = 0;
    let mut first_error = None;
    for bulk in bulks {
        match send_rows(k, ctx, bulk).await {
            Ok(n) => made += n,
            Err(e) => {
                tracing::warn!(bulk = %bulk, "bulk sending failed: {e}");
                first_error.get_or_insert(e);
            }
        }
    }
    forget_failed(k, ctx).await?;
    first_error.map_or(Ok(made), Err)
}

/// The next rows of one bulk send, each in its own transaction.
async fn send_rows(k: &Kernel, ctx: &Ctx, bulk: Uuid) -> Result<usize> {
    let table = child_table(ctx, "SignBulkSend", "recipients")?;
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let next: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select id from {table} where tenant_id = $1 and parent_id = $2 and status = 'Waiting' and request is null order by row_no limit {PER_RUN}"
    )))
    .bind(ctx.tenant)
    .bind(bulk)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut made = 0;
    for row in &next {
        match send_row(k, ctx, bulk, *row).await {
            Ok(Step::Sent) => made += 1,
            Ok(Step::Skipped) => {}
            Ok(Step::Paused) => return Ok(made),
            Err(e) => {
                // The row is recorded as failed, with why, in a transaction of its own; the others go on.
                let note = clean(&e.to_string(), 500);
                with_retry(|| async {
                    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
                    let status: Option<String> = sqlx::query_scalar("select status from e_sign_bulk_send where tenant_id = $1 and id = $2 for update")
                        .bind(ctx.tenant)
                        .bind(bulk)
                        .fetch_optional(&mut *tx)
                        .await?;
                    if status.as_deref() != Some("Sending") {
                        tx.rollback().await?;
                        return Ok(());
                    }
                    k.set_child_values_tx(ctx, &mut tx, "SignBulkSend", "recipients", *row, obj(json!({"status": "Failed", "error": note}))).await?;
                    let failed: i64 = sqlx::query_scalar("select failed from e_sign_bulk_send where tenant_id = $1 and id = $2")
                        .bind(ctx.tenant)
                        .bind(bulk)
                        .fetch_one(&mut *tx)
                        .await?;
                    k.set_values_tx(ctx, &mut tx, "SignBulkSend", bulk, obj(json!({"failed": failed + 1}))).await?;
                    tx.commit().await?;
                    Ok(())
                })
                .await?;
            }
        }
    }
    finish(k, ctx, bulk, &table).await?;
    Ok(made)
}

enum Step {
    Sent,
    Skipped,
    /// The daily limit is reached: nothing more is made until room is free.
    Paused,
}

/// One row: its request is made and sent in the same transaction that marks the row sent.
async fn send_row(k: &Kernel, ctx: &Ctx, bulk: Uuid, row_id: Uuid) -> Result<Step> {
    let table = child_table(ctx, "SignBulkSend", "recipients")?;
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    // The bulk send first, then the row: still sending, and this row not made yet (another run may have made it).
    let head: Option<(String, Option<Uuid>, Option<Uuid>)> =
        sqlx::query_as("select status, created_by, template from e_sign_bulk_send where tenant_id = $1 and id = $2 for update")
            .bind(ctx.tenant)
            .bind(bulk)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((status, Some(creator), Some(template))) = head else {
        tx.rollback().await?;
        return Ok(Step::Skipped);
    };
    let row: Option<(Option<Uuid>, String)> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("select request, status from {table} where tenant_id = $1 and id = $2 for update")))
            .bind(ctx.tenant)
            .bind(row_id)
            .fetch_optional(&mut *tx)
            .await?;
    if status != "Sending" || !matches!(&row, Some((None, st)) if st == "Waiting") {
        tx.rollback().await?;
        return Ok(Step::Skipped);
    }
    let settings = settings::load(k, ctx, &mut tx).await?;
    // Rolling 24 hours, not the calendar day.
    let today: i64 =
        sqlx::query_scalar("select count(*) from e_sign_request where tenant_id = $1 and bulk_send is not null and created_at > now() - interval '24 hours'")
            .bind(ctx.tenant)
            .fetch_one(&mut *tx)
            .await?;
    if today >= settings.bulk_per_day {
        tx.rollback().await?;
        return Ok(Step::Paused);
    }
    let bulk_doc = k.get_tx(ctx, &mut tx, "SignBulkSend", bulk, false).await?;
    let rec = rows(&bulk_doc, "recipients").into_iter().find(|r| id(r, "id") == Some(row_id)).ok_or_else(|| Error::NotFound("recipient".into()))?;
    let signers: Vec<Value> =
        people(&rec).map_err(Error::invalid)?.into_iter().map(|(role, name, email)| json!({"role": role, "name": name, "email": email})).collect();
    // As the person who made the bulk send, with the rights they have now.
    let mut who = k.load_ctx(ctx.tenant, creator, None, format!("sign-bulk-{}", Uuid::now_v7())).await?;
    who.made = ferrum_kernel::Made::Job;
    let mut params = Map::new();
    params.insert("title".into(), json!(s(&bulk_doc, "title")));
    for key in ["message", "expires_on"] {
        if let Some(v) = bulk_doc.get(key).filter(|v| !v.is_null()) {
            params.insert(key.into(), v.clone());
        }
    }
    for key in ["reference_entity", "reference_id"] {
        if let Some(v) = rec.get(key).filter(|v| !v.is_null()) {
            params.insert(key.into(), v.clone());
        }
    }
    params.insert("signers".into(), Value::Array(signers));
    let made = k.run_action_tx(&who, &mut tx, "SignTemplate", template, "create_request", &Value::Object(params)).await?;
    let request = made.get("id").and_then(Value::as_str).and_then(|v| v.parse::<Uuid>().ok()).ok_or_else(|| Error::internal("a request without an id"))?;
    k.set_values_tx(ctx, &mut tx, "SignRequest", request, obj(json!({"bulk_send": bulk}))).await?;
    k.run_action_tx(&who, &mut tx, "SignRequest", request, "send", &json!({})).await?;
    k.set_child_values_tx(ctx, &mut tx, "SignBulkSend", "recipients", row_id, obj(json!({"status": "Sent", "request": request, "signers": null}))).await?;
    k.set_values_tx(ctx, &mut tx, "SignBulkSend", bulk, obj(json!({"sent": crate::int(&bulk_doc, "sent") + 1}))).await?;
    tx.commit().await?;
    Ok(Step::Sent)
}

/// When no row is waiting any more: the bulk send is done.
async fn finish(k: &Kernel, ctx: &Ctx, bulk: Uuid, table: &str) -> Result<()> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let status: Option<String> = sqlx::query_scalar("select status from e_sign_bulk_send where tenant_id = $1 and id = $2 for update")
        .bind(ctx.tenant)
        .bind(bulk)
        .fetch_optional(&mut *tx)
        .await?;
    let waiting: i64 =
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select count(*) from {table} where tenant_id = $1 and parent_id = $2 and status = 'Waiting'")))
            .bind(ctx.tenant)
            .bind(bulk)
            .fetch_one(&mut *tx)
            .await?;
    if status.as_deref() != Some("Sending") || waiting > 0 {
        tx.rollback().await?;
        return Ok(());
    }
    let failed: i64 =
        sqlx::query_scalar("select failed from e_sign_bulk_send where tenant_id = $1 and id = $2").bind(ctx.tenant).bind(bulk).fetch_one(&mut *tx).await?;
    let done = if failed > 0 { "Done With Failures" } else { "Done" };
    k.set_values_tx(ctx, &mut tx, "SignBulkSend", bulk, obj(json!({"status": done}))).await?;
    tx.commit().await?;
    Ok(())
}

/// The people of a failed row are kept a month after the bulk send last changed (to read why and fix the row), then forgotten.
async fn forget_failed(k: &Kernel, ctx: &Ctx) -> Result<()> {
    let table = child_table(ctx, "SignBulkSend", "recipients")?;
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "update {table} r set signers = null from e_sign_bulk_send b where r.tenant_id = $1 and b.tenant_id = r.tenant_id and b.id = r.parent_id \
         and r.status = 'Failed' and r.signers is not null and b.status <> 'Sending' and b.updated_at < now() - make_interval(days => {KEEP_FAILED_DAYS})"
    )))
    .bind(ctx.tenant)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
