//! Sign Settings: sender name, public URL for links, link expiry, reminder cadence and the
//! signing key (a secret field, rotated by an action).

use crate::{doc_id, token};
use async_trait::async_trait;
use ferrum_kernel::{Ctx, Doc, DocHook, HookCtx, Kernel, Result, Tx};
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
    /// The brand of the signing page (`Brand`), when one is chosen.
    pub brand: Option<Uuid>,
    /// What signers agree to, in the workspace's own words; none: Sign's sentence.
    pub consent_text: Option<String>,
}

fn base(url: Option<String>) -> Option<String> {
    url.map(|u| u.trim().trim_end_matches('/').to_string()).filter(|u| u.starts_with("https://") || u.starts_with("http://"))
}

/// The tenant's settings, with defaults when nobody saved them yet. The public URL falls back to where the platform
/// sends people outside the workspace, as a signer is: through
/// the operator's link host when there is one (`/go/{workspace}/p`), so a link keeps working when the workspace's address
/// changes, else the workspace's own address.
pub async fn load(k: &Kernel, ctx: &Ctx, tx: &mut Tx) -> Result<Settings> {
    type Row = (Option<String>, Option<String>, Option<i64>, Option<i64>, Option<i64>, Option<Uuid>, Option<String>);
    let row: Option<Row> = sqlx::query_as(
        "select sender_name, public_url, link_expiry_days, reminder_every_days, max_reminders, brand, consent_text from e_sign_settings \
         where tenant_id = $1 limit 1",
    )
    .bind(ctx.tenant)
    .fetch_optional(&mut **tx)
    .await?;
    let (sender, url, expiry, every, max, brand, consent_text) = row.unwrap_or_default();
    let mut public_url = base(url);
    if public_url.is_none() {
        // With nothing after it: what comes before the paths Sign adds (`/api/site/…`, which every host of the workspace
        // serves, its portal's too).
        public_url = base(k.person_link(ctx.tenant, ferrum_kernel::notify::Audience::Outside, "").await);
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
        brand,
        consent_text: consent_text.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()),
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
