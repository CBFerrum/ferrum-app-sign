//! Getting word to people: signers' links and emails, sent with the workspace's own wording (the package's message
//! templates `sign.invitation`, `sign.reminder`, `sign.voided`, `sign.completed`, which a workspace edits) through the
//! platform's messages and the server's mailer, with or without the Mail app; and in-app notifications for users when
//! no email can go out. A signing link is a secret value of its message: sealed while it waits, a placeholder in the
//! message log.

use crate::settings::{self, Settings};
use crate::{doc_id, human, id, s, time, token};
use ferrum_kernel::messages::{Message, Recipient};
use ferrum_kernel::{Ctx, Doc, Error, Kernel, Result, Tx};
use serde_json::{Map, Value, json};
use uuid::Uuid;

/// What a delivery needs: the settings, the workspace slug for links and the signing key.
pub struct Outbox {
    /// Emails can go out: the server has a mailer.
    pub mail: bool,
    /// The sealed PDF can travel with the completion email.
    attach: bool,
    pub settings: Settings,
    tenant: Uuid,
    slug: String,
    key: Vec<u8>,
}

impl Outbox {
    pub async fn load(k: &Kernel, ctx: &Ctx, tx: &mut Tx) -> Result<Outbox> {
        let settings = settings::load(k, ctx, tx).await?;
        let key = token::signing_key(k, tx, ctx.tenant).await?;
        let slug = settings::slug(tx, ctx.tenant).await?;
        let attach = k.mailer.as_ref().is_some_and(|m| m.can_attach());
        Ok(Outbox { mail: crate::can_email(k), attach, settings, tenant: ctx.tenant, slug, key })
    }

    /// The signer's link to the signing page (absolute when the public URL is known).
    pub fn link(&self, request: &Doc, signer: &Doc) -> Result<String> {
        let token = token::link_token(&self.key, self.tenant, doc_id(request)?, doc_id(signer)?, s(signer, "email").unwrap_or_default());
        Ok(format!("{}/api/site/{}/sign/s/{token}", self.settings.public_url.as_deref().unwrap_or_default(), self.slug))
    }

    /// A signer's page for `minutes` from now (never past the request's expiry), for a signer whose link is not mailed:
    /// the interface's page with the session (`?e=`), framed when `embed`. With its moment.
    pub fn session_link(&self, request: &Doc, signer: &Doc, minutes: i64, embed: bool) -> Result<(String, chrono::DateTime<chrono::Utc>)> {
        let mut until = crate::now() + chrono::Duration::minutes(minutes);
        if let Some(end) = time(request, "expires_at") {
            until = until.min(end);
        }
        let email = s(signer, "email").unwrap_or_default();
        let token = token::link_token(&self.key, self.tenant, doc_id(request)?, doc_id(signer)?, email);
        let session = token::session_token(&self.key, self.tenant, doc_id(signer)?, email, until.timestamp());
        let url = format!(
            "{}/sign/s/{}/{token}?e={session}{}",
            self.settings.public_url.as_deref().unwrap_or_default(),
            self.slug,
            if embed { "&embed=1" } else { "" }
        );
        Ok((url, until))
    }

    /// Emailed links must be absolute.
    pub fn require_url(&self) -> Result<()> {
        if self.mail && self.settings.public_url.is_none() {
            return Err(Error::invalid("set the Public URL in Sign Settings so emailed signing links work"));
        }
        Ok(())
    }

    /// Emails a signer their link, an invitation or a reminder, when an email can go out; returns whether it was
    /// emailed (otherwise the caller hands out the link).
    pub async fn invite(&self, k: &Kernel, ctx: &Ctx, tx: &mut Tx, request: &Doc, signer: &Doc, reminder: bool) -> Result<bool> {
        if !self.mail || linked(signer) {
            return Ok(false);
        }
        self.require_url()?;
        let mut values = self.values(tx, ctx.tenant, request).await?;
        values.insert("name".into(), json!(s(signer, "name").unwrap_or_default()));
        values.insert("link".into(), json!(self.link(request, signer)?));
        values.insert("until".into(), json!(time(request, "expires_at").map(human).unwrap_or_default()));
        values.insert("access_code".into(), json!(s(signer, "access_code_hash").is_some()));
        let template = if reminder { REMINDER } else { INVITATION };
        email(k, ctx, tx, template, request, signer, values, &[], &["link"]).await?;
        Ok(true)
    }

    /// Tells signers who had the link that the request was withdrawn.
    pub async fn withdrawn(&self, k: &Kernel, ctx: &Ctx, tx: &mut Tx, request: &Doc, signer: &Doc) -> Result<()> {
        if !self.mail || s(signer, "email").is_none() {
            return Ok(());
        }
        let values = self.values(tx, ctx.tenant, request).await?;
        email(k, ctx, tx, VOIDED, request, signer, values, &[], &[]).await
    }

    /// Sends a signer the sealed PDF, only where it can travel with the email.
    pub async fn completed(&self, k: &Kernel, ctx: &Ctx, tx: &mut Tx, request: &Doc, signer: &Doc, file: Uuid) -> Result<()> {
        if !self.mail || !self.attach || s(signer, "email").is_none() {
            return Ok(());
        }
        let values = self.values(tx, ctx.tenant, request).await?;
        email(k, ctx, tx, COMPLETED, request, signer, values, &[file], &[]).await
    }

    /// What every message of a request reads besides the request: the organization and the sender.
    async fn values(&self, tx: &mut Tx, tenant: Uuid, request: &Doc) -> Result<Map<String, Value>> {
        let mut v = Map::new();
        v.insert("organization".into(), json!(self.settings.sender_name));
        v.insert("sender".into(), json!(sender_line(tx, tenant, request, &self.settings).await?));
        Ok(v)
    }
}

/// The package's message templates (`packages/sign/package.yaml`).
const INVITATION: &str = "sign.invitation";
const REMINDER: &str = "sign.reminder";
const VOIDED: &str = "sign.voided";
const COMPLETED: &str = "sign.completed";

/// `Asha Rao (asha@acme.test) of Acme Corp`; the template escapes it.
async fn sender_line(tx: &mut Tx, tenant: Uuid, request: &Doc, settings: &Settings) -> Result<String> {
    Ok(match sender_of(tx, tenant, request).await? {
        Some((name, email)) => format!("{name} ({email}) of {}", settings.sender_name),
        None => settings.sender_name.clone(),
    })
}

/// The name and email of the user who created the request.
pub async fn sender_of(tx: &mut Tx, tenant: Uuid, request: &Doc) -> Result<Option<(String, String)>> {
    let Some(user) = id(request, "created_by") else { return Ok(None) };
    Ok(sqlx::query_as("select full_name, email::text from app_user where tenant_id = $1 and id = $2").bind(tenant).bind(user).fetch_optional(&mut **tx).await?)
}

/// One email from a template, about the request, in the caller's transaction (it goes once that commits).
#[allow(clippy::too_many_arguments)]
async fn email(
    k: &Kernel,
    ctx: &Ctx,
    tx: &mut Tx,
    template: &str,
    request: &Doc,
    to: &Doc,
    values: Map<String, Value>,
    attachments: &[Uuid],
    secret: &[&str],
) -> Result<()> {
    let address = s(to, "email").ok_or_else(|| Error::internal("signer without email"))?;
    let m = Message {
        template: template.into(),
        entity: Some("SignRequest".into()),
        record: Some(doc_id(request)?),
        to: vec![Recipient::address(address)],
        channels: vec!["email".into()],
        values,
        attachments: attachments.to_vec(),
        secret: secret.iter().map(|v| v.to_string()).collect(),
        ..Default::default()
    };
    ferrum_kernel::messages::send(k, tx, ctx, &m).await.map(|_| ())
}

/// An in-app notification to the request's sender; for a request no person made (a configured step's, the system's),
/// to every active Sign Manager, so what needs someone is never told to nobody.
pub async fn notify_sender(tx: &mut Tx, tenant: Uuid, request: &Doc, title: &str, body: Option<&str>) -> Result<()> {
    let users = match id(request, "created_by").filter(|u| !u.is_nil()) {
        Some(user) => vec![user],
        None => {
            sqlx::query_scalar(
                "select ur.user_id from user_role ur join app_user u on u.tenant_id = ur.tenant_id and u.id = ur.user_id \
             where ur.tenant_id = $1 and ur.role = 'Sign Manager' and u.is_active and (ur.expires_at is null or ur.expires_at > now())",
            )
            .bind(tenant)
            .fetch_all(&mut **tx)
            .await?
        }
    };
    for user in users {
        ferrum_kernel::notify::send(tx, tenant, user, title, body, Some("SignRequest"), Some(doc_id(request)?)).await?;
    }
    Ok(())
}

/// Links people copy when there is no Mail: in the action's answer, and in a notification to the
/// user who asked (the generic app shows notifications, not action answers).
pub async fn links_to(tx: &mut Tx, ctx: &Ctx, request: &Doc, links: &[Value]) -> Result<()> {
    let lines: Vec<String> = links
        .iter()
        .filter_map(|l| {
            Some(format!("{} — {} <{}>: {}", l["role"].as_str()?, l["name"].as_str().unwrap_or_default(), l["email"].as_str()?, l["url"].as_str()?))
        })
        .collect();
    if lines.is_empty() {
        return Ok(());
    }
    let title = format!("Signing links for {}", s(request, "number").unwrap_or("your request"));
    let body = format!("Send each signer their own link:\n{}", lines.join("\n"));
    let user = if ctx.user.is_nil() { id(request, "created_by") } else { Some(ctx.user) };
    if let Some(user) = user {
        ferrum_kernel::notify::send(tx, ctx.tenant, user, &title, Some(&body), Some("SignRequest"), Some(doc_id(request)?)).await?;
    }
    Ok(())
}

/// `{role, name, email, emailed, url?}`: the URL only when it was not emailed, and never for a signer whose page the
/// requesting app opens (their link alone opens nothing: `signing_link` gives a session).
pub fn link_row(out: &Outbox, request: &Doc, signer: &Doc, emailed: bool) -> Result<Value> {
    let mut row = json!({"role": s(signer, "role"), "name": s(signer, "name"), "email": s(signer, "email"), "emailed": emailed});
    if linked(signer) {
        row["delivery"] = json!("Link");
    } else if !emailed {
        row["url"] = json!(out.link(request, signer)?);
    }
    Ok(row)
}

/// A signer whose link is not mailed: the app that asked opens their page (`signing_link`).
pub fn linked(signer: &Doc) -> bool {
    s(signer, "delivery").is_some_and(|d| d != "Email")
}

/// What the trail says of a link given out: mailed, handed to the sender, or left to the requesting app.
pub fn given(signer: &Doc, emailed: bool, again: bool) -> &'static str {
    match (linked(signer), emailed, again) {
        (true, _, _) => "Signing link issued by the requesting app when it opens the page",
        (false, true, false) => "Emailed the signing link",
        (false, true, true) => "Emailed the signing link again",
        (false, false, false) => "Signing link given to the sender",
        (false, false, true) => "Signing link given to the sender again",
    }
}
