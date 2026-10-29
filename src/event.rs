//! The audit trail: one `SignEvent` per thing that happened to a request, numbered per request and
//! chained by SHA-256 (each event's hash covers its content and the previous event's hash).
//! Only the engine writes events; the hook refuses inserts by people, edits and deletes, a
//! database trigger refuses updates, and `verify` recomputes the chain, so a removed or altered
//! row shows.

use crate::{clean, doc_id, s, ts};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ferrum_kernel::{Ctx, Doc, DocHook, Error, HookCtx, Kernel, Result, Tx};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub struct EventHook;

#[async_trait]
impl DocHook for EventHook {
    async fn validate(&self, hc: &mut HookCtx<'_>, _doc: &Doc) -> Result<()> {
        if !hc.ctx.is_system {
            return Err(Error::Forbidden("sign events are written by the system only".into()));
        }
        Ok(())
    }

    async fn after_save(&self, _hc: &mut HookCtx<'_>, _doc: &Doc, old: Option<&Doc>) -> Result<()> {
        if old.is_some() {
            return Err(Error::Forbidden("sign events cannot be changed".into()));
        }
        Ok(())
    }

    async fn before_delete(&self, _hc: &mut HookCtx<'_>, _doc: &Doc) -> Result<()> {
        Err(Error::Forbidden("sign events cannot be deleted".into()))
    }
}

/// An event to append.
pub struct Entry<'a> {
    event: &'static str,
    signer: Option<&'a Doc>,
    actor: String,
    ip_hash: Option<String>,
    user_agent: Option<String>,
    details: Option<String>,
}

impl<'a> Entry<'a> {
    pub fn new(event: &'static str, actor: &str) -> Entry<'a> {
        Entry { event, signer: None, actor: actor.to_string(), ip_hash: None, user_agent: None, details: None }
    }
    pub fn signer(mut self, signer: &'a Doc) -> Self {
        self.signer = Some(signer);
        self
    }
    pub fn client(mut self, ip_hash: &str, user_agent: Option<&str>) -> Self {
        self.ip_hash = Some(ip_hash.to_string());
        self.user_agent = user_agent.map(str::to_string);
        self
    }
    pub fn details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }
}

/// One stored event, as the chain hashes it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Row {
    pub seq: i64,
    pub event: String,
    pub occurred_at: DateTime<Utc>,
    pub role: Option<String>,
    pub signer_name: Option<String>,
    pub signer_email: Option<String>,
    pub actor: Option<String>,
    pub ip_hash: Option<String>,
    pub user_agent: Option<String>,
    pub details: Option<String>,
    pub prev_hash: Option<String>,
    pub event_hash: Option<String>,
}

impl Row {
    fn content(&self, request: Uuid) -> String {
        json!([
            request.to_string(),
            self.seq,
            self.event,
            ts(self.occurred_at),
            self.role,
            self.signer_name,
            self.signer_email,
            self.actor,
            self.ip_hash,
            self.user_agent,
            self.details
        ])
        .to_string()
    }

    fn hash(&self, request: Uuid, prev: &str) -> String {
        let mut h = Sha256::new();
        h.update(prev.as_bytes());
        h.update(b"\n");
        h.update(self.content(request).as_bytes());
        hex::encode(h.finalize())
    }
}

/// Values as the kernel stores them (trimmed, no control characters, within the field's length),
/// so what is hashed now is what verification reads back.
fn tidy(v: Option<&str>, max: usize) -> Option<String> {
    v.map(|x| clean(x, max)).filter(|x| !x.is_empty())
}

/// Appends an event to a request's trail. The caller holds the request's row lock (sequence
/// numbers are also unique per request); `ctx` is a system or elevated context.
pub async fn record(k: &Kernel, ctx: &Ctx, tx: &mut Tx, request: &Doc, e: Entry<'_>) -> Result<Uuid> {
    let rid = doc_id(request)?;
    let last: Option<(i64, Option<String>)> =
        sqlx::query_as("select seq, event_hash from e_sign_event where tenant_id = $1 and request = $2 order by seq desc limit 1")
            .bind(ctx.tenant)
            .bind(rid)
            .fetch_optional(&mut **tx)
            .await?;
    let (seq, prev) = last.map(|(n, h)| (n + 1, h.unwrap_or_default())).unwrap_or((1, String::new()));
    let row = Row {
        seq,
        event: e.event.to_string(),
        occurred_at: crate::now(),
        role: tidy(e.signer.and_then(|x| s(x, "role")), 60),
        signer_name: tidy(e.signer.and_then(|x| s(x, "name")), 140),
        signer_email: tidy(e.signer.and_then(|x| s(x, "email")), 254).map(|x| x.to_lowercase()),
        actor: tidy(Some(&e.actor), 200),
        ip_hash: tidy(e.ip_hash.as_deref(), 64),
        user_agent: tidy(e.user_agent.as_deref(), 500),
        details: tidy(e.details.as_deref(), 2000),
        prev_hash: Some(prev.clone()).filter(|p| !p.is_empty()),
        event_hash: None,
    };
    let hash = row.hash(rid, &prev);
    let mut doc = Doc::new();
    doc.insert("request".into(), json!(rid));
    doc.insert("seq".into(), json!(seq));
    doc.insert("event".into(), json!(row.event));
    doc.insert("occurred_at".into(), json!(ts(row.occurred_at)));
    for (k, v) in [
        ("role", &row.role),
        ("signer_name", &row.signer_name),
        ("signer_email", &row.signer_email),
        ("actor", &row.actor),
        ("ip_hash", &row.ip_hash),
        ("user_agent", &row.user_agent),
        ("details", &row.details),
        ("prev_hash", &row.prev_hash),
    ] {
        doc.insert(k.into(), v.clone().map_or(Value::Null, Value::String));
    }
    doc.insert("sender".into(), request.get("sender_email").cloned().unwrap_or(Value::Null));
    doc.insert("event_hash".into(), json!(hash));
    let saved = k.insert_tx(ctx, tx, "SignEvent", doc).await?;
    doc_id(&saved)
}

/// A request's events in order.
pub async fn list(tx: &mut Tx, tenant: Uuid, request: Uuid) -> Result<Vec<Row>> {
    Ok(sqlx::query_as(
        "select seq, event, occurred_at, role, signer_name, signer_email, actor, ip_hash, user_agent, details, prev_hash, event_hash \
         from e_sign_event where tenant_id = $1 and request = $2 order by seq",
    )
    .bind(tenant)
    .bind(request)
    .fetch_all(&mut **tx)
    .await?)
}

/// The result of recomputing a trail.
#[derive(Debug, Clone)]
pub struct Check {
    pub events: usize,
    pub head: String,
    pub problem: Option<String>,
}

/// Recomputes the chain: numbers must run 1, 2, 3..., each event must name the previous hash and
/// hash to its stored value.
pub fn check(request: Uuid, rows: &[Row]) -> Check {
    let mut prev = String::new();
    for (i, r) in rows.iter().enumerate() {
        let problem = if r.seq != i as i64 + 1 {
            Some(format!("event {} is missing", i + 1))
        } else if r.prev_hash.clone().unwrap_or_default() != prev {
            Some(format!("event {} does not follow event {}", r.seq, i))
        } else if r.event_hash.as_deref() != Some(r.hash(request, &prev).as_str()) {
            Some(format!("event {} ({}) was altered", r.seq, r.event))
        } else {
            None
        };
        if problem.is_some() {
            return Check { events: rows.len(), head: prev, problem };
        }
        prev = r.event_hash.clone().unwrap_or_default();
    }
    Check { events: rows.len(), head: prev, problem: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(n: i64) -> (Uuid, Vec<Row>) {
        let request = Uuid::now_v7();
        let mut rows: Vec<Row> = Vec::new();
        for seq in 1..=n {
            let prev = rows.last().and_then(|r| r.event_hash.clone()).unwrap_or_default();
            let mut r = Row {
                seq,
                event: "Viewed".into(),
                occurred_at: crate::now(),
                role: Some("Client".into()),
                signer_name: Some("Jane".into()),
                signer_email: Some("jane@client.test".into()),
                actor: Some("jane@client.test".into()),
                ip_hash: Some("ab".repeat(16)),
                user_agent: None,
                details: None,
                prev_hash: Some(prev.clone()).filter(|p| !p.is_empty()),
                event_hash: None,
            };
            r.event_hash = Some(r.hash(request, &prev));
            rows.push(r);
        }
        (request, rows)
    }

    #[test]
    fn chains_detect_removed_reordered_and_altered_events() {
        let (request, rows) = chain(4);
        let ok = check(request, &rows);
        assert_eq!((ok.events, ok.problem.as_deref()), (4, None));
        assert_eq!(ok.head, rows[3].event_hash.clone().unwrap());
        assert!(check(Uuid::now_v7(), &rows).problem.is_some(), "the chain is bound to its request");

        let mut removed = rows.clone();
        removed.remove(1);
        assert_eq!(check(request, &removed).problem.as_deref(), Some("event 2 is missing"));
        let mut last_removed = rows.clone();
        last_removed.pop();
        assert!(check(request, &last_removed).problem.is_none(), "a truncated tail is only caught against the certificate's head");

        let mut altered = rows.clone();
        altered[2].ip_hash = Some("cd".repeat(16));
        assert_eq!(check(request, &altered).problem.as_deref(), Some("event 3 (Viewed) was altered"));

        let mut relinked = rows.clone();
        relinked[2].prev_hash = Some("00".repeat(32));
        assert!(check(request, &relinked).problem.unwrap().contains("does not follow"));
    }
}
