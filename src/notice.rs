//! Getting word to people: signers' links, emails through the Mail app (a `Communication` record
//! per message, delivered and retried by Mail) when the workspace has it, in-app notifications
//! for users otherwise.

use crate::settings::{self, Settings};
use crate::{doc_id, human, id, s, time, token};
use ferrum_kernel::{Ctx, Doc, Error, Kernel, Result, Tx};
use serde_json::{Value, json};
use uuid::Uuid;

/// What a delivery needs: the settings, the workspace slug for links and the signing key.
pub struct Outbox {
    pub mail: bool,
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
        Ok(Outbox { mail: crate::mail(ctx), settings, tenant: ctx.tenant, slug, key })
    }

    /// The signer's link to the signing page (absolute when the public URL is known).
    pub fn link(&self, request: &Doc, signer: &Doc) -> Result<String> {
        let token = token::link_token(&self.key, self.tenant, doc_id(request)?, doc_id(signer)?, s(signer, "email").unwrap_or_default());
        Ok(format!("{}/api/site/{}/sign/s/{token}", self.settings.public_url.as_deref().unwrap_or_default(), self.slug))
    }

    /// Emailed links must be absolute.
    pub fn require_url(&self) -> Result<()> {
        if self.mail && self.settings.public_url.is_none() {
            return Err(Error::invalid("set the Public URL in Sign Settings (or in the Mail app's settings) so emailed signing links work"));
        }
        Ok(())
    }

    /// Emails a signer their link — an invitation or a reminder — when the workspace has Mail;
    /// returns whether it was emailed (otherwise the caller hands out the link).
    pub async fn invite(&self, k: &Kernel, ctx: &Ctx, tx: &mut Tx, request: &Doc, signer: &Doc, reminder: bool) -> Result<bool> {
        if !self.mail {
            return Ok(false);
        }
        self.require_url()?;
        let link = self.link(request, signer)?;
        let sender = sender_line(tx, ctx.tenant, request, &self.settings).await?;
        let title = s(request, "title").unwrap_or_default();
        let number = s(request, "number").unwrap_or_default();
        let until = time(request, "expires_at").map(human).unwrap_or_default();
        let code = s(signer, "access_code_hash").is_some();
        let subject = if reminder { format!("Reminder: please sign \"{title}\"") } else { format!("{}: please sign \"{title}\"", self.settings.sender_name) };
        let lead = if reminder {
            format!("{sender} is still waiting for your signature on <strong>{}</strong> ({}).", esc(title), esc(number))
        } else {
            format!("{sender} sent you <strong>{}</strong> ({}) to sign.", esc(title), esc(number))
        };
        let message = s(request, "message")
            .map(|m| {
                format!("<blockquote style=\"margin:12px 0;padding-left:12px;border-left:3px solid #d0d7de\">{}</blockquote>", esc(m).replace('\n', "<br>"))
            })
            .unwrap_or_default();
        let note = format!(
            "This link is personal: anyone who has it can sign as you, so do not forward it. It works until {until}.{}",
            if code { " You will also need the access code the sender gave you." } else { "" }
        );
        let html = format!(
            "<p>Hello {},</p><p>{lead}</p>{message}<p><a href=\"{}\" style=\"display:inline-block;padding:10px 18px;background:#1f6feb;color:#fff;border-radius:6px;text-decoration:none\">Review and sign</a></p><p style=\"color:#57606a;font-size:13px\">{}</p>",
            esc(s(signer, "name").unwrap_or("there")),
            esc(&link),
            esc(&note)
        );
        let text = format!(
            "Hello {},\n\n{}\n\n{}Review and sign: {link}\n\n{note}\n",
            s(signer, "name").unwrap_or("there"),
            strip_tags(&lead),
            s(request, "message").map(|m| format!("{m}\n\n")).unwrap_or_default()
        );
        email(k, ctx, tx, request, signer, &subject, &html, &text, json!([])).await?;
        Ok(true)
    }

    /// Tells signers who had the link that the request was withdrawn (Mail only).
    pub async fn withdrawn(&self, k: &Kernel, ctx: &Ctx, tx: &mut Tx, request: &Doc, signer: &Doc) -> Result<()> {
        if !self.mail || s(signer, "email").is_none() {
            return Ok(());
        }
        let sender = sender_line(tx, ctx.tenant, request, &self.settings).await?;
        let title = s(request, "title").unwrap_or_default();
        let reason = s(request, "void_reason").map(|r| format!(" Reason: {r}")).unwrap_or_default();
        let html = format!(
            "<p>{sender} withdrew <strong>{}</strong> ({}). It can no longer be signed and your link no longer works.{}</p>",
            esc(title),
            esc(s(request, "number").unwrap_or_default()),
            esc(&reason)
        );
        let text = format!("{} withdrew \"{title}\". It can no longer be signed.{reason}\n", strip_tags(&sender));
        email(k, ctx, tx, request, signer, &format!("Withdrawn: \"{title}\""), &html, &text, json!([])).await
    }

    /// Sends a signer the sealed PDF (Mail only).
    pub async fn completed(&self, k: &Kernel, ctx: &Ctx, tx: &mut Tx, request: &Doc, signer: &Doc, file: &Value) -> Result<()> {
        if !self.mail || s(signer, "email").is_none() {
            return Ok(());
        }
        let title = s(request, "title").unwrap_or_default();
        let sha = s(request, "sealed_sha256").unwrap_or_default();
        let html = format!(
            "<p>Everyone has signed <strong>{}</strong> ({}). The signed PDF with its audit certificate is attached.</p><p style=\"color:#57606a;font-size:13px\">SHA-256 of the attached file: <code>{sha}</code></p>",
            esc(title),
            esc(s(request, "number").unwrap_or_default())
        );
        let text = format!("Everyone has signed \"{title}\". The signed PDF with its audit certificate is attached.\nSHA-256: {sha}\n");
        email(k, ctx, tx, request, signer, &format!("Signed: \"{title}\""), &html, &text, json!([file])).await
    }
}

/// `Asha Rao (asha@acme.test) of Acme Corp`, HTML-escaped.
async fn sender_line(tx: &mut Tx, tenant: Uuid, request: &Doc, settings: &Settings) -> Result<String> {
    Ok(match sender_of(tx, tenant, request).await? {
        Some((name, email)) => format!("{} ({}) of {}", esc(&name), esc(&email), esc(&settings.sender_name)),
        None => esc(&settings.sender_name),
    })
}

/// The name and email of the user who created the request.
pub async fn sender_of(tx: &mut Tx, tenant: Uuid, request: &Doc) -> Result<Option<(String, String)>> {
    let Some(user) = id(request, "created_by") else { return Ok(None) };
    Ok(sqlx::query_as("select full_name, email::text from app_user where tenant_id = $1 and id = $2").bind(tenant).bind(user).fetch_optional(&mut **tx).await?)
}

/// One email through Mail, on the request's timeline.
#[allow(clippy::too_many_arguments)]
async fn email(k: &Kernel, ctx: &Ctx, tx: &mut Tx, request: &Doc, to: &Doc, subject: &str, html: &str, text: &str, attachments: Value) -> Result<()> {
    let address = s(to, "email").ok_or_else(|| Error::internal("signer without email"))?;
    let name: String = s(to, "name").unwrap_or_default().chars().filter(|c| !c.is_control() && !"\"\\<>".contains(*c)).collect();
    let recipient = if name.trim().is_empty() { address.to_string() } else { format!("\"{}\" <{address}>", name.trim()) };
    let mut doc = Doc::new();
    for (key, v) in [
        ("channel", json!("Email")),
        ("direction", json!("Sent")),
        ("recipients", json!(recipient)),
        ("subject", json!(subject.chars().take(990).collect::<String>())),
        ("content", json!(html)),
        ("text_content", json!(text)),
        ("reference_entity", json!("SignRequest")),
        ("reference_id", json!(doc_id(request)?)),
        ("attachments", attachments),
    ] {
        doc.insert(key.into(), v);
    }
    k.insert_tx(ctx, tx, "Communication", doc).await.map(|_| ())
}

/// An in-app notification to the request's sender.
pub async fn notify_sender(tx: &mut Tx, tenant: Uuid, request: &Doc, title: &str, body: Option<&str>) -> Result<()> {
    if let Some(user) = id(request, "created_by") {
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

/// `{role, name, email, emailed, url?}`: the URL only when it was not emailed.
pub fn link_row(out: &Outbox, request: &Doc, signer: &Doc, emailed: bool) -> Result<Value> {
    let mut row = json!({"role": s(signer, "role"), "name": s(signer, "name"), "email": s(signer, "email"), "emailed": emailed});
    if !emailed {
        row["url"] = json!(out.link(request, signer)?);
    }
    Ok(row)
}

pub fn esc(v: &str) -> String {
    v.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut inside = false;
    for c in html.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    out.replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping() {
        assert_eq!(esc("<a href=\"x\">'&"), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;");
        assert_eq!(strip_tags("Asha (a@x.test) sent you <strong>NDA &amp; terms</strong>."), "Asha (a@x.test) sent you NDA & terms.");
    }
}
