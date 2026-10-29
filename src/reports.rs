//! Sign reports: requests by status, time to complete, and signatures waiting per signer. Every
//! query carries the caller's record scope on Sign Request, so a sender sees their own numbers.
//! Dates are the workspace's local dates.

use crate::child_table;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use ferrum_domain::reports::{Params, ReportCompany, ReportSource, col, param_def, period, read_tx};
use ferrum_domain::util::money;
use ferrum_kernel::{Action, Ctx, Error, Kernel, Result, Tx, row_scope};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder};
use std::collections::HashMap;
use uuid::Uuid;

pub struct SignReports;

const STATUSES: [&str; 7] = ["Draft", "Sent", "In Progress", "Completed", "Declined", "Expired", "Voided"];
const GROUPS: [(&str, &str); 3] = [
    ("Month", "to_char(a.completed_at at time zone $2, 'YYYY-MM')"),
    ("Sender", "coalesce(a.sender_email, 'Unknown')"),
    ("Template", "coalesce(t.template_name, 'No template')"),
];

/// Requests are not per company: periods follow the workspace's fiscal year.
fn span(ctx: &Ctx, p: &Params<'_>) -> Result<(NaiveDate, NaiveDate)> {
    let co = ReportCompany { id: Uuid::nil(), name: String::new(), currency: String::new(), fy_month: ctx.fy_start_month, precision: 2 };
    period(p, &co)
}

/// `select <columns> from` the caller's visible requests `a` (and their template `t`); `$1` is
/// the tenant and `$2` the workspace time zone.
fn requests(ctx: &Ctx, columns: &str) -> Result<QueryBuilder<Postgres>> {
    let ent = ctx.schema.entity("SignRequest").ok_or_else(|| Error::NotFound("SignRequest".into()))?;
    let scope = row_scope(ctx, ent, Action::Read)?;
    let mut qb = QueryBuilder::new(format!(
        "select {columns} from e_sign_request a left join e_sign_template t on t.tenant_id = a.tenant_id and t.id = a.template \
         where a.tenant_id = "
    ));
    qb.push_bind(ctx.tenant);
    // Keeps `$2` the time zone for the expressions in `columns`.
    qb.push(" and ").push_bind(ctx.schema.timezone.clone()).push("::text is not null");
    scope.push_sql(&mut qb, "a");
    Ok(qb)
}

fn local_date(qb: &mut QueryBuilder<Postgres>, column: &str, (from, to): (NaiveDate, NaiveDate)) {
    qb.push(format!(" and (a.{column} at time zone $2)::date between ")).push_bind(from).push(" and ").push_bind(to);
}

fn pct(part: i64, whole: i64) -> Value {
    if whole == 0 { Value::Null } else { money((Decimal::from(part) * Decimal::ONE_HUNDRED / Decimal::from(whole)).round_dp(1)) }
}

fn dec(v: Option<Decimal>) -> Value {
    v.map_or(Value::Null, money)
}

async fn by_status(ctx: &Ctx, p: &Params<'_>, tx: &mut Tx) -> Result<Value> {
    let period = span(ctx, p)?;
    let mut qb = requests(ctx, "a.status, count(*)")?;
    local_date(&mut qb, "created_at", period);
    qb.push(" group by 1");
    let counts: HashMap<String, i64> = qb.build_query_as::<(String, i64)>().fetch_all(&mut **tx).await?.into_iter().collect();
    let total: i64 = counts.values().sum();
    let rows: Vec<Value> = STATUSES
        .iter()
        .map(|st| {
            let n = counts.get(*st).copied().unwrap_or(0);
            json!({"status": st, "requests": n, "share": pct(n, total)})
        })
        .collect();
    Ok(json!({
        "columns": [col("status", "Status", "data"), col("requests", "Requests", "int"), col("share", "Share %", "decimal")],
        "rows": rows,
        "totals": {"requests": total},
        "meta": {"from_date": period.0, "to_date": period.1},
    }))
}

type TimeRow = (String, i64, Option<Decimal>, Option<Decimal>, Option<Decimal>, Option<Decimal>);

async fn completion(ctx: &Ctx, p: &Params<'_>, tx: &mut Tx) -> Result<Value> {
    let period = span(ctx, p)?;
    let want = p.str("group_by").unwrap_or("Month");
    let (label, expr) = GROUPS
        .iter()
        .copied()
        .find(|(l, _)| l.eq_ignore_ascii_case(want))
        .ok_or_else(|| Error::field("group_by", format!("use one of: {}", GROUPS.map(|g| g.0).join(", "))))?;
    let hours = "extract(epoch from (a.completed_at - a.sent_at)) / 3600.0";
    let columns = format!(
        "count(*), round(avg({hours})::numeric, 2), round((percentile_cont(0.5) within group (order by {hours}))::numeric, 2), \
         round(min({hours})::numeric, 2), round(max({hours})::numeric, 2)"
    );
    let filter = |qb: &mut QueryBuilder<Postgres>| {
        qb.push(" and a.status = 'Completed' and a.sent_at is not null");
        local_date(qb, "completed_at", period);
    };
    let mut grouped = requests(ctx, &format!("{expr}, {columns}"))?;
    filter(&mut grouped);
    grouped.push(" group by 1 order by 1");
    let data: Vec<TimeRow> = grouped.build_query_as().fetch_all(&mut **tx).await?;
    let mut all = requests(ctx, &format!("'All'::text, {columns}"))?;
    filter(&mut all);
    let total: TimeRow = all.build_query_as().fetch_one(&mut **tx).await?;
    let row = |(group, n, avg, median, min, max): TimeRow| json!({"group": group, "completed": n, "average_hours": dec(avg), "median_hours": dec(median), "fastest_hours": dec(min), "slowest_hours": dec(max)});
    let mut totals = row(total);
    if let Value::Object(m) = &mut totals {
        m.remove("group");
    }
    Ok(json!({
        "columns": [col("group", label, "data"), col("completed", "Completed", "int"), col("average_hours", "Average (hours)", "decimal"),
            col("median_hours", "Median (hours)", "decimal"), col("fastest_hours", "Fastest (hours)", "decimal"), col("slowest_hours", "Slowest (hours)", "decimal")],
        "rows": data.into_iter().map(row).collect::<Vec<_>>(),
        "totals": totals,
        "meta": {"from_date": period.0, "to_date": period.1, "group_by": label},
    }))
}

type PendingRow = (String, Option<String>, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>);

async fn pending(ctx: &Ctx, tx: &mut Tx) -> Result<Value> {
    let signers = child_table(ctx, "SignRequest", "signers")?;
    let ent = ctx.schema.entity("SignRequest").ok_or_else(|| Error::NotFound("SignRequest".into()))?;
    let scope = row_scope(ctx, ent, Action::Read)?;
    let mut qb = QueryBuilder::new(format!(
        "select lower(sg.email), max(sg.name), count(*), min(sg.sent_at), min(a.expires_at) from e_sign_request a \
         join {signers} sg on sg.tenant_id = a.tenant_id and sg.parent_id = a.id where a.tenant_id = "
    ));
    qb.push_bind(ctx.tenant);
    scope.push_sql(&mut qb, "a");
    qb.push(" and a.status in ('Sent', 'In Progress') and a.expires_at > now() and sg.status in ('Sent', 'Viewed') group by 1 order by 3 desc, 1");
    let data: Vec<PendingRow> = qb.build_query_as().fetch_all(&mut **tx).await?;
    let now = Utc::now();
    let total: i64 = data.iter().map(|r| r.2).sum();
    let rows: Vec<Value> = data
        .into_iter()
        .map(|(email, name, n, since, next)| {
            let days = since.map(|t| Decimal::from((now - t).num_minutes().max(0)) / Decimal::from(1440)).map(|d| d.round_dp(1));
            json!({"signer": email, "name": name, "pending": n, "waiting_since": since, "days_waiting": dec(days), "next_expiry": next})
        })
        .collect();
    Ok(json!({
        "columns": [col("signer", "Signer", "data"), col("name", "Name", "data"), col("pending", "Waiting Requests", "int"),
            col("waiting_since", "Waiting Since", "datetime"), col("days_waiting", "Days Waiting", "decimal"), col("next_expiry", "Next Expiry", "datetime")],
        "rows": rows,
        "totals": {"pending": total},
    }))
}

#[async_trait]
impl ReportSource for SignReports {
    fn catalog(&self) -> Vec<Value> {
        let entry =
            |name: &str, label: &str, params: Vec<Value>| json!({"name": name, "label": label, "module": "sign", "entity": "SignRequest", "params": params});
        vec![
            entry("sign_requests_by_status", "Requests by Status", vec![param_def("from_date"), param_def("to_date")]),
            entry(
                "sign_completion_time",
                "Time to Complete",
                vec![
                    param_def("from_date"),
                    param_def("to_date"),
                    json!({"name": "group_by", "label": "Group By", "type": "select", "options": GROUPS.map(|g| g.0), "default": "Month"}),
                ],
            ),
            entry("sign_pending_signatures", "Pending Signatures by Signer", vec![]),
        ]
    }

    async fn run(&self, k: &Kernel, ctx: &Ctx, name: &str, params: &HashMap<String, String>) -> Result<Value> {
        let p = Params { raw: params };
        let mut tx = read_tx(k, ctx).await?;
        let out = match name {
            "sign_requests_by_status" => by_status(ctx, &p, &mut tx).await,
            "sign_completion_time" => completion(ctx, &p, &mut tx).await,
            "sign_pending_signatures" => pending(ctx, &mut tx).await,
            _ => Err(Error::NotFound(format!("report {name}"))),
        };
        tx.rollback().await?;
        out
    }
}
