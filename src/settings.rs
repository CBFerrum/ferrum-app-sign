//! Sign Settings: sender name, public URL for links, link expiry, reminder cadence and the
//! signing key (a secret field, rotated by an action).

use crate::{doc_id, token};
use async_trait::async_trait;
use ferrum_kernel::{Ctx, Doc, DocHook, Error, HookCtx, Kernel, Result, Tx};
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
    /// Requests that bulk sends may make in 24 hours.
    pub bulk_per_day: i64,
    /// The time-stamping authority's connection, when sealing asks one.
    pub timestamp_connection: Option<String>,
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
    type Row = (Option<String>, Option<String>, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<Uuid>, Option<String>, Option<String>);
    let row: Option<Row> = sqlx::query_as(
        "select sender_name, public_url, link_expiry_days, reminder_every_days, max_reminders, bulk_per_day, brand, consent_text, timestamp_connection from e_sign_settings \
         where tenant_id = $1 limit 1",
    )
    .bind(ctx.tenant)
    .fetch_optional(&mut **tx)
    .await?;
    let (sender, url, expiry, every, max, bulk, brand, consent_text, stamper) = row.unwrap_or_default();
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
        bulk_per_day: bulk.unwrap_or(2000).clamp(1, 100_000),
        timestamp_connection: stamper.map(|c| c.trim().to_string()).filter(|c| !c.is_empty()),
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
    async fn action(&self, hc: &mut HookCtx<'_>, name: &str, doc: &Doc, params: &Value) -> Result<Option<Value>> {
        match name {
            "rotate_signing_key" => {
                token::rotate_key(hc.kernel, hc.ctx, hc.tx, doc_id(doc)?).await?;
                Ok(Some(json!({"rotated": true})))
            }
            "allow_embedding" => allow_embedding(hc, params).await.map(Some),
            "set_seal" => set_seal(hc, doc, params).await.map(Some),
            _ => Ok(None),
        }
    }
}

/// Sets the seal: the key must be on P-256 or P-384 and the certificate must be for it, checked here, before anything is stored
/// (the key goes into the vault with the settings' other secret).
async fn set_seal(hc: &mut HookCtx<'_>, doc: &Doc, params: &Value) -> Result<Value> {
    let given = |k: &str| params.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    let key = given("seal_key").ok_or_else(|| Error::field("seal_key", "give the private key"))?;
    let certificates = given("seal_certificate").ok_or_else(|| Error::field("seal_certificate", "give the certificate that goes with the key"))?;
    let subject = crate::seal_key::check(&key, &certificates).map_err(|m| Error::field("seal_key", m))?;
    let sys = ferrum_domain::util::sys(hc.ctx);
    let mut values = Doc::new();
    values.insert("seal_key".into(), json!(key));
    values.insert("seal_certificate".into(), json!(certificates));
    hc.kernel.update_tx(&sys, hc.tx, "SignSettings", doc_id(doc)?, values, None).await?;
    Ok(json!({"sealed_by": subject}))
}

/// The Embedding of the signing page (`sign/s`) holds these origins: the platform frames the page only for them. The
/// record is the workspace's own (written as the system, once, under a lock on its page address); the Embedding's own
/// check refuses a path, a plain-http site that is not local, or more than ten.
async fn allow_embedding(hc: &mut HookCtx<'_>, params: &Value) -> Result<Value> {
    let origins = params.get("origins").and_then(Value::as_str).map(|o| o.trim().to_string()).filter(|o| !o.is_empty());
    let origins = origins.ok_or_else(|| Error::field("origins", "name at least one site, such as https://www.example.com"))?;
    let sys = ferrum_domain::util::sys(hc.ctx);
    let mut input = Doc::new();
    input.insert("page".into(), json!(PAGE));
    input.insert("origins".into(), json!(origins));
    input.insert("note".into(), json!("Set by Sign: Allow Embedding"));
    let found: Option<Uuid> = sqlx::query_scalar("select id from e_embedding where tenant_id = $1 and page = $2 for update")
        .bind(hc.ctx.tenant)
        .bind(PAGE)
        .fetch_optional(&mut **hc.tx)
        .await?;
    match found {
        Some(id) => hc.kernel.update_tx(&sys, hc.tx, "Embedding", id, input, None).await.map(|_| ())?,
        None => hc.kernel.insert_tx(&sys, hc.tx, "Embedding", input).await.map(|_| ())?,
    }
    Ok(json!({"page": PAGE}))
}

/// The signing page's address in the platform's Embeddings.
const PAGE: &str = "sign/s";
