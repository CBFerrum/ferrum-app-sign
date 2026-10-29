//! Maintenance, every minute: expire overdue requests, send due reminders, retry sealing that
//! failed. Each item is re-checked under its request's row lock and changed in one transaction
//! with its audit event and its email, so it happens exactly once however often this runs. A
//! failing item is logged and does not stop the others; the first error is returned.

use crate::event::{self, Entry};
use crate::notice::{self, Outbox};
use crate::request::{OPEN, obj, set_signer};
use crate::{PACKAGE, child_table, human, int, now, rows, s, seal, settings, time, ts};
use chrono::Duration;
use ferrum_kernel::{Ctx, Error, Kernel, Result, with_retry};
use serde_json::json;
use uuid::Uuid;

/// Items handled per tenant and step in one round.
const BATCH: i64 = 100;
/// Sealing is retried this long after the request last changed (the last signature seals at once).
const SEAL_RETRY_MINUTES: i64 = 2;

pub async fn run(k: &Kernel) -> Result<()> {
    let mut tx = k.db.system_tx().await?;
    let tenants: Vec<Uuid> = sqlx::query_scalar("select id from tenant where status = 'active' order by id").fetch_all(&mut *tx).await?;
    tx.commit().await?;
    let mut first_error = None;
    for tenant in tenants {
        let schema = match k.registry.schema(tenant).await {
            Ok(s) => s,
            Err(e) => {
                first_error.get_or_insert(e);
                continue;
            }
        };
        if !schema.packages.iter().any(|p| p == PACKAGE) {
            continue;
        }
        let ctx = Ctx::system(tenant, schema);
        let steps = [("expiry", expire(k, &ctx).await), ("reminders", remind(k, &ctx).await), ("sealing", reseal(k, &ctx).await)];
        for (step, outcome) in steps {
            match outcome {
                Ok(0) => {}
                Ok(n) => tracing::info!(%tenant, step, n, "sign maintenance"),
                Err(e) => {
                    tracing::warn!(%tenant, step, "sign maintenance failed: {e}");
                    first_error.get_or_insert(e);
                }
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Runs `item` for each id; failures are logged and the first is returned after the rest ran.
async fn each<F, Fut>(ids: Vec<Uuid>, step: &str, item: F) -> Result<usize>
where
    F: Fn(Uuid) -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let (mut done, mut first) = (0, None);
    for id in ids {
        match item(id).await {
            Ok(true) => done += 1,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(%id, step, "sign maintenance item failed: {e}");
                first.get_or_insert(e);
            }
        }
    }
    first.map_or(Ok(done), Err)
}

async fn expire(k: &Kernel, ctx: &Ctx) -> Result<usize> {
    let signers = child_table(ctx, "SignRequest", "signers")?;
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select r.id from e_sign_request r where r.tenant_id = $1 and r.status in ('Sent', 'In Progress') and r.expires_at <= now() \
         and exists (select 1 from {signers} s where s.tenant_id = r.tenant_id and s.parent_id = r.id and s.status is distinct from 'Signed') \
         order by r.expires_at limit {BATCH}"
    )))
    .bind(ctx.tenant)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    each(ids, "expiry", |id| {
        with_retry(move || async move {
            let mut tx = k.db.tenant_tx(ctx.tenant).await?;
            let doc = crate::lock_request(k, ctx, &mut tx, id).await?;
            let open = OPEN.contains(&s(&doc, "status").unwrap_or_default());
            let all_signed = rows(&doc, "signers").iter().all(|r| s(r, "status") == Some("Signed"));
            let Some(at) = time(&doc, "expires_at").filter(|t| *t <= now() && open && !all_signed) else {
                tx.rollback().await?;
                return Ok(false);
            };
            let done = k.update_tx(ctx, &mut tx, "SignRequest", id, obj(json!({"status": "Expired"})), None).await?;
            event::record(k, ctx, &mut tx, &done, Entry::new("Expired", "system").details(format!("Signing links stopped working at {}", human(at)))).await?;
            let title = format!("{} expired before everyone signed", s(&done, "number").unwrap_or_default());
            notice::notify_sender(&mut tx, ctx.tenant, &done, &title, s(&done, "title")).await?;
            tx.commit().await?;
            Ok(true)
        })
    })
    .await
}

async fn remind(k: &Kernel, ctx: &Ctx) -> Result<usize> {
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let settings = settings::load(k, ctx, &mut tx).await?;
    if settings.reminder_every_days == 0 || settings.max_reminders == 0 {
        tx.commit().await?;
        return Ok(0);
    }
    let signers = child_table(ctx, "SignRequest", "signers")?;
    let due: Vec<(Uuid, Uuid)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select s.parent_id, s.id from {signers} s join e_sign_request r on r.tenant_id = s.tenant_id and r.id = s.parent_id \
         where s.tenant_id = $1 and r.status in ('Sent', 'In Progress') and r.expires_at > now() and s.status in ('Sent', 'Viewed') \
         and coalesce(s.reminders_sent, 0) < $2 and greatest(s.sent_at, s.last_reminded_at) <= now() - make_interval(days => $3::int) \
         order by greatest(s.sent_at, s.last_reminded_at) limit {BATCH}"
    )))
    .bind(ctx.tenant)
    .bind(settings.max_reminders)
    .bind(settings.reminder_every_days as i32)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let (every, max) = (settings.reminder_every_days, settings.max_reminders);
    let pairs: Vec<Uuid> = due.iter().map(|(_, sid)| *sid).collect();
    each(pairs, "reminders", |sid| {
        let rid = due.iter().find(|(_, x)| *x == sid).map(|(r, _)| *r).unwrap_or_default();
        with_retry(move || async move {
            let mut tx = k.db.tenant_tx(ctx.tenant).await?;
            let doc = crate::lock_request(k, ctx, &mut tx, rid).await?;
            let at = now();
            let sg = rows(&doc, "signers").into_iter().find(|r| crate::id(r, "id") == Some(sid));
            let since = sg.as_ref().and_then(|r| time(r, "sent_at").into_iter().chain(time(r, "last_reminded_at")).max());
            let due_now = sg.as_ref().is_some_and(|r| matches!(s(r, "status"), Some("Sent" | "Viewed")) && int(r, "reminders_sent") < max)
                && OPEN.contains(&s(&doc, "status").unwrap_or_default())
                && time(&doc, "expires_at").is_some_and(|t| t > at)
                && since.is_some_and(|t| t + Duration::days(every) <= at);
            let (Some(sg), true) = (sg, due_now) else {
                tx.rollback().await?;
                return Ok(false);
            };
            let count = int(&sg, "reminders_sent") + 1;
            set_signer(k, ctx, &mut tx, sid, json!({"reminders_sent": count, "last_reminded_at": ts(at)})).await?;
            let out = Outbox::load(k, ctx, &mut tx).await?;
            let emailed = out.invite(k, ctx, &mut tx, &doc, &sg, true).await?;
            if !emailed {
                let title = format!("{} has not signed {} yet", s(&sg, "name").unwrap_or("A signer"), s(&doc, "number").unwrap_or_default());
                notice::notify_sender(&mut tx, ctx.tenant, &doc, &title, Some(&format!("Remind them with their link: {}", out.link(&doc, &sg)?))).await?;
            }
            let detail = format!("Automatic reminder {count} of {max}{}", if emailed { ", emailed" } else { ", sender notified" });
            event::record(k, ctx, &mut tx, &doc, Entry::new("Reminded", "system").signer(&sg).details(detail)).await?;
            tx.commit().await?;
            Ok(true)
        })
    })
    .await
}

async fn reseal(k: &Kernel, ctx: &Ctx) -> Result<usize> {
    let signers = child_table(ctx, "SignRequest", "signers")?;
    let mut tx = k.db.tenant_tx(ctx.tenant).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select r.id from e_sign_request r where r.tenant_id = $1 and r.status = 'In Progress' and r.sealed_document is null \
         and r.updated_at < now() - make_interval(mins => {SEAL_RETRY_MINUTES}) \
         and not exists (select 1 from {signers} s where s.tenant_id = r.tenant_id and s.parent_id = r.id and s.status is distinct from 'Signed') \
         order by r.updated_at limit 20"
    )))
    .bind(ctx.tenant)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    each(ids, "sealing", |id| seal::seal(k, ctx.tenant, id)).await.map_err(|e| match e {
        Error::Invalid(m) => Error::Invalid(format!("sealing failed: {m}")),
        other => other,
    })
}
