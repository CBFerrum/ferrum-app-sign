//! Sign Settings: sender name, public URL for links, link expiry, reminder cadence and the
//! signing key (a secret field, rotated by an action).

use crate::{doc_id, s, token};
use async_trait::async_trait;
use ferrum_kernel::{Ctx, Doc, DocHook, Filter, HookCtx, Kernel, Result, Tx};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Settings {
    pub sender_name: String,
    /// Absolute base for links (`https://erp.example.com`), when known.
    pub public_url: Option<String>,
    pub link_expiry_days: i64,
    pub reminder_every_days: i64,
    pub max_reminders: i64,
}

fn base(url: Option<String>) -> Option<String> {
    url.map(|u| u.trim().trim_end_matches('/').to_string()).filter(|u| u.starts_with("https://") || u.starts_with("http://"))
}

/// The tenant's settings, with defaults when nobody saved them yet. The public URL falls back to
/// the Mail app's when Sign has none.
pub async fn load(k: &Kernel, ctx: &Ctx, tx: &mut Tx) -> Result<Settings> {
    type Row = (Option<String>, Option<String>, Option<i64>, Option<i64>, Option<i64>);
    let row: Option<Row> = sqlx::query_as(
        "select sender_name, public_url, link_expiry_days, reminder_every_days, max_reminders from e_sign_settings where tenant_id = $1 limit 1",
    )
    .bind(ctx.tenant)
    .fetch_optional(&mut **tx)
    .await?;
    let (sender, url, expiry, every, max) = row.unwrap_or_default();
    let mut public_url = base(url);
    if public_url.is_none() && ctx.schema.entity("CommsSettings").is_some() {
        let comms = k.find_tx(&ferrum_domain::util::sys(ctx), tx, "CommsSettings", &[] as &[Filter], None, 1).await?;
        public_url = base(comms.first().and_then(|d| s(d, "public_url")).map(str::to_string));
    }
    let sender_name = match sender.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
        Some(v) => v,
        None => sqlx::query_scalar("select name from tenant where id = $1").bind(ctx.tenant).fetch_one(&mut **tx).await?,
    };
    Ok(Settings {
        sender_name,
        public_url,
        link_expiry_days: expiry.unwrap_or(30).clamp(1, 365),
        reminder_every_days: every.unwrap_or(3).clamp(0, 60),
        max_reminders: max.unwrap_or(3).clamp(0, 20),
    })
}

/// The workspace slug, for public paths (`/api/site/{slug}/sign/...`).
pub async fn slug(tx: &mut Tx, tenant: Uuid) -> Result<String> {
    Ok(sqlx::query_scalar("select slug from tenant where id = $1").bind(tenant).fetch_one(&mut **tx).await?)
}

pub struct SettingsHook;

#[async_trait]
impl DocHook for SettingsHook {
    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, _params: &Value) -> Result<Option<Value>> {
        if name != "rotate_signing_key" {
            return Ok(None);
        }
        token::rotate_key(hc.kernel, hc.ctx, hc.tx, doc_id(doc)?).await?;
        Ok(Some(json!({"rotated": true})))
    }
}
