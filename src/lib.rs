//! Sign: electronic signatures with an audit trail.
//!
//! * `SignRequest` (numbered document): a PDF, its signers (sequential routing orders or
//!   parallel) and fields placed per signer role. Actions `send`, `remind`, `resend`, `void` and
//!   `verify`; `SignTemplate.create_request` makes one from a reusable template.
//! * Signers act through public, token-authenticated endpoints (`site.rs`): view the PDF with
//!   their own fields, sign (drawn PNG or typed), or decline. Links are HMAC tokens under a key
//!   in Sign Settings; access codes are Argon2id hashes.
//! * `SignEvent` is the append-only, hash-chained audit trail.
//! * When the last signer signs, `seal.rs` stamps every value onto the original pages, appends an
//!   audit certificate as an incremental update and stores the sealed PDF with its SHA-256.
//! * Mail (the `comms` app) delivers links and copies when installed; otherwise actions return
//!   copyable links and people get in-app notifications.

mod document;
mod event;
mod notice;
mod pdf;
mod reports;
mod request;
mod seal;
mod settings;
mod signature;
mod signing;
mod site;
mod template;
mod token;
mod upkeep;

use chrono::{DateTime, NaiveDate, NaiveTime, SecondsFormat, TimeZone, Utc};
use ferrum_domain::reports::ReportSource;
use ferrum_kernel::{Ctx, Doc, Error, HookRegistry, Kernel, Result};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

include!(concat!(env!("OUT_DIR"), "/packages.rs"));

/// This app's manifest (`app.yaml`).
pub const MANIFEST: &str = include_str!("../app.yaml");

/// This app's metadata packages, embedded at build time.
pub fn packages() -> std::result::Result<Vec<ferrum_meta::Package>, ferrum_meta::MetaError> {
    ferrum_meta::catalog::load_embedded(EMBEDDED)
}

/// Registers this app's document hooks and its public signing endpoints.
pub fn register(hooks: &mut HookRegistry) {
    hooks
        .register("SignSettings", Arc::new(settings::SettingsHook))
        .register("SignTemplate", Arc::new(template::TemplateHook))
        .register("SignRequest", Arc::new(request::RequestHook))
        .register("SignEvent", Arc::new(event::EventHook))
        .add_site("sign", Arc::new(site::SignSite));
}

/// This app's reports.
pub fn reports() -> Vec<Arc<dyn ReportSource>> {
    vec![Arc::new(reports::SignReports)]
}

/// Periodic upkeep: every minute, on one replica at a time (advisory lock); must be idempotent and cheap.
/// Expires overdue requests, sends due reminders (each once) and retries sealing that failed.
pub async fn maintenance(k: &Kernel) -> Result<()> {
    upkeep::run(k).await
}

// ───────────────────────────── shared helpers ─────────────────────────────

pub(crate) use ferrum_domain::util::{b, elevated, id, rows, s};

pub(crate) const PACKAGE: &str = "sign";

/// Now, to the millisecond: the precision timestamps are stored and hashed with.
pub(crate) fn now() -> DateTime<Utc> {
    let n = Utc::now();
    Utc.timestamp_millis_opt(n.timestamp_millis()).single().unwrap_or(n)
}

/// RFC 3339 UTC with milliseconds, the kernel's canonical date-time form.
pub(crate) fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(crate) fn time(doc: &Doc, key: &str) -> Option<DateTime<Utc>> {
    s(doc, key).and_then(|v| ferrum_kernel::value::parse_datetime(v).ok())
}

pub(crate) fn int(doc: &Doc, key: &str) -> i64 {
    doc.get(key).and_then(Value::as_i64).unwrap_or(0)
}

pub(crate) fn doc_id(doc: &Doc) -> Result<Uuid> {
    id(doc, "id").ok_or_else(|| Error::internal("record without id"))
}

/// A date-time as people read it on the certificate and in mail: `2026-10-29 14:03:12 UTC`.
pub(crate) fn human(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

/// The first instant after `day` ends in the IANA zone `tz` (local midnight, which a DST change
/// may skip: then the first hour that exists). Unknown zones count as UTC.
pub(crate) fn end_of_day(day: NaiveDate, tz: &str) -> DateTime<Utc> {
    let next = day.succ_opt().unwrap_or(day);
    let utc = next.and_time(NaiveTime::MIN).and_utc();
    match tz.parse::<chrono_tz::Tz>() {
        Ok(zone) => (0..=3)
            .filter_map(|h| next.and_hms_opt(h, 0, 0))
            .find_map(|local| zone.from_local_datetime(&local).earliest())
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or(utc),
        Err(_) => utc,
    }
}

/// Locks a request's row, then reads the request in a statement of its own. A locking read alone
/// returns the child rows (signers, fields) as they were when it started waiting for the lock —
/// before the transaction it waited for committed — so concurrent signers would not see each other.
pub(crate) async fn lock_request(k: &Kernel, ctx: &Ctx, tx: &mut ferrum_kernel::Tx, id: Uuid) -> Result<Doc> {
    let found: Option<Uuid> = sqlx::query_scalar("select id from e_sign_request where tenant_id = $1 and id = $2 for update")
        .bind(ctx.tenant)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    found.ok_or_else(|| Error::NotFound("Sign Request".into()))?;
    k.get_tx(ctx, tx, "SignRequest", id, false).await
}

/// Whether the workspace has the Mail app, which delivers this app's emails.
pub(crate) fn mail(ctx: &Ctx) -> bool {
    ctx.schema.packages.iter().any(|p| p == "comms") && ctx.schema.entity("Communication").is_some()
}

/// The physical table of a child table field.
pub(crate) fn child_table(ctx: &Ctx, entity: &str, field: &str) -> Result<String> {
    ctx.schema
        .entity(entity)
        .and_then(|e| e.children.get(field))
        .map(|c| ferrum_kernel::ddl::q(&c.table))
        .ok_or_else(|| Error::internal(format!("{entity}.{field} is not a table")))
}

/// Text cut to `max` characters, without control characters, trimmed.
pub(crate) fn clean(v: &str, max: usize) -> String {
    v.chars().filter(|c| !c.is_control()).take(max).collect::<String>().trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Duration};

    #[test]
    fn packages_are_valid() {
        let mut all = ferrum_domain::packages().expect("platform packages");
        all.extend(super::packages().expect("app packages"));
        let catalog = ferrum_meta::Catalog::from_packages(all).unwrap_or_else(|e| panic!("{e}"));
        let schema = ferrum_meta::effective::build(&catalog, &[PACKAGE.to_string()], &[], 1).unwrap_or_else(|e| panic!("{e}"));
        assert!(schema.warnings.is_empty(), "{:?}", schema.warnings);
        assert!(schema.entity("SignSettings").is_some_and(|e| e.secret_fields().count() == 1));
        for e in ["SignRequest", "SignTemplate", "SignEvent"] {
            assert!(schema.entity(e).is_some(), "{e}");
        }
        assert!(MANIFEST.contains("packages: [sign]"));
    }

    #[test]
    fn days_end_at_local_midnight_across_dst() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        // New York: 8 March 2026 has 23 hours, 1 November 2026 has 25.
        assert_eq!(end_of_day(d(2026, 3, 7), "America/New_York"), at("2026-03-08T05:00:00Z"));
        assert_eq!(end_of_day(d(2026, 3, 8), "America/New_York"), at("2026-03-09T04:00:00Z"));
        assert_eq!(end_of_day(d(2026, 10, 31), "America/New_York"), at("2026-11-01T04:00:00Z"));
        assert_eq!(end_of_day(d(2026, 11, 1), "America/New_York"), at("2026-11-02T05:00:00Z"));
        assert_eq!(end_of_day(d(2026, 10, 29), "Asia/Kolkata"), at("2026-10-29T18:30:00Z"));
        assert_eq!(end_of_day(d(2026, 10, 29), "Not/AZone"), at("2026-10-30T00:00:00Z"));
        // Every day of the year, in zones with DST at 02:00, at midnight (Santiago) and by half an
        // hour (Lord Howe): the end is the first instant of the next local day.
        for tz in ["America/New_York", "America/Santiago", "Australia/Lord_Howe", "Europe/London", "Asia/Kolkata"] {
            let zone: chrono_tz::Tz = tz.parse().unwrap();
            let mut day = d(2026, 1, 1);
            while day.year() == 2026 {
                let end = end_of_day(day, tz);
                assert_eq!(end.with_timezone(&zone).date_naive(), day.succ_opt().unwrap(), "{tz} {day}");
                assert_eq!((end - Duration::seconds(1)).with_timezone(&zone).date_naive(), day, "{tz} {day}");
                day = day.succ_opt().unwrap();
            }
        }
    }

    #[test]
    fn timestamps_keep_milliseconds() {
        let t = now();
        assert_eq!(t.timestamp_subsec_nanos() % 1_000_000, 0);
        assert_eq!(time(&serde_json::json!({"at": ts(t)}).as_object().cloned().unwrap(), "at"), Some(t));
        assert_eq!(clean(" a\u{0}b\tc ", 3), "ab");
    }
}
