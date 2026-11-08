//! `/history` and `/history.csv`: runs that ended, from the log on disk,
//! newest first. A token limited to categories sees only theirs.

use super::{ApiError, ApiResult, AppState, Caller};
use crate::history::{csv_row, Query, CSV_HEADER, MAX_LIMIT};
use axum::extract::{Query as Params, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};

/// `from` and `to` are dates in the panel's time zone (both days
/// included); `before` is a time (ms) for the next page.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HistoryParams {
    from: Option<String>,
    to: Option<String>,
    announcement: Option<String>,
    state: Option<String>,
    by: Option<String>,
    q: Option<String>,
    limit: Option<usize>,
    /// The `next` of the page before: `started_ms:id` (or just a time).
    before: Option<String>,
    /// Sent by browsers for event streams; ignored here.
    #[serde(default, rename = "access_token")]
    _token: Option<String>,
}

fn day_start(date: &str, tz: &jiff::tz::TimeZone) -> ApiResult<u64> {
    let d: jiff::civil::Date = date
        .trim()
        .parse()
        .map_err(|_| ApiError::bad(format!("\"{date}\" isn't a date like 2026-11-08")))?;
    let z = d
        .to_zoned(tz.clone())
        .map_err(|e| ApiError::bad(format!("\"{date}\": {e}")))?;
    Ok(z.timestamp().as_millisecond().max(0) as u64)
}

async fn run_query(
    s: &AppState,
    caller: &Caller,
    p: HistoryParams,
    default_limit: usize,
) -> ApiResult<(crate::model::Config, Vec<Value>, bool)> {
    let cfg = s.load().await?;
    let tz = cfg.tz().unwrap_or_else(|_| jiff::tz::TimeZone::system());
    let from_ms = p
        .from
        .as_deref()
        .filter(|d| !d.is_empty())
        .map(|d| day_start(d, &tz))
        .transpose()?;
    let to_ms = match p.to.as_deref().filter(|d| !d.is_empty()) {
        // To the end of that day: the start of the next.
        Some(d) => {
            let day: jiff::civil::Date = d
                .trim()
                .parse()
                .map_err(|_| ApiError::bad(format!("\"{d}\" isn't a date like 2026-11-08")))?;
            let next = day.tomorrow().map_err(|e| ApiError::bad(e.to_string()))?;
            Some(day_start(&next.to_string(), &tz)?)
        }
        None => None,
    };
    let before = match p.before.as_deref().filter(|b| !b.is_empty()) {
        None => None,
        Some(b) => {
            let (ms, id) = b.split_once(':').unwrap_or((b, "0"));
            let parse = |x: &str| x.trim().parse::<u64>().ok();
            Some((parse(ms).zip(parse(id))).ok_or_else(|| {
                ApiError::bad(format!("before=\"{b}\" isn't a page mark (started_ms:id)"))
            })?)
        }
    };
    let q = Query {
        from_ms,
        to_ms,
        announcement: p.announcement.filter(|x| !x.is_empty()),
        state: p.state.filter(|x| !x.is_empty()),
        by: p.by.filter(|x| !x.is_empty()),
        text: p.q.filter(|x| !x.is_empty()),
        before,
        limit: p.limit.unwrap_or(default_limit).min(MAX_LIMIT),
    };
    let history = s.0.history.clone();
    let caller = caller.clone();
    let c2 = cfg.clone();
    let (runs, more) = tokio::task::spawn_blocking(move || {
        history.query(&q, |r| {
            caller.may_play(&c2, r["announcement"].as_str().unwrap_or(""))
        })
    })
    .await?;
    Ok((cfg, runs, more))
}

/// `{"runs": [...], "more", "next"}`: for the next page, pass
/// `before` = `next` (the last run's `started_ms:id`).
pub(super) async fn list(
    State(s): State<AppState>,
    Extension(caller): Extension<Caller>,
    Params(p): Params<HistoryParams>,
) -> ApiResult<Json<Value>> {
    let (_, runs, more) = run_query(&s, &caller, p, 100).await?;
    let next = more.then(|| runs.last()).flatten().map(|r| {
        format!(
            "{}:{}",
            r["started_ms"].as_u64().unwrap_or(0),
            r["id"].as_u64().unwrap_or(0)
        )
    });
    Ok(Json(json!({ "runs": runs, "more": more, "next": next })))
}

/// The same, as a spreadsheet (times in the panel's time zone).
pub(super) async fn csv(
    State(s): State<AppState>,
    Extension(caller): Extension<Caller>,
    Params(p): Params<HistoryParams>,
) -> ApiResult<Response> {
    let (cfg, runs, _) = run_query(&s, &caller, p, MAX_LIMIT).await?;
    let tz = cfg.tz().unwrap_or_else(|_| jiff::tz::TimeZone::system());
    let mut out = String::from(CSV_HEADER);
    out.push_str("\r\n");
    for r in &runs {
        out.push_str(&csv_row(r, &tz));
        out.push_str("\r\n");
    }
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"intercom-history.csv\"",
            ),
        ],
        out,
    )
        .into_response())
}

/// The Panel's stats strip: plays today and over the last 7 days (in the
/// panel's time zone), and groups busy or failed in the last 24 hours:
/// `{"today": COUNTS, "week": COUNTS, "last_24h": {"busy_groups",
/// "failed_groups"}}`, counts being `{"plays", "played", "stopped",
/// "failed", "missed"}`. A token limited to categories counts only theirs.
pub(super) async fn stats(
    State(s): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<Value>> {
    let cfg = s.load().await?;
    let tz = cfg.tz().unwrap_or_else(|_| jiff::tz::TimeZone::system());
    let now = jiff::Timestamp::now();
    let today = now.to_zoned(tz.clone()).date();
    let start = |d: jiff::civil::Date| {
        d.to_zoned(tz.clone())
            .map(|z| z.timestamp().as_millisecond().max(0) as u64)
            .map_err(|e| ApiError::internal(e.to_string()))
    };
    let today_ms = start(today)?;
    let week_ms = start(
        today
            .checked_sub(jiff::Span::new().days(6))
            .map_err(|e| ApiError::internal(e.to_string()))?,
    )?;
    let day_ago_ms = (now.as_millisecond() as u64).saturating_sub(86_400_000);
    let history = s.0.history.clone();
    let v = tokio::task::spawn_blocking(move || {
        history.stats(today_ms, week_ms, day_ago_ms, |r| {
            caller.may_play(&cfg, r["announcement"].as_str().unwrap_or(""))
        })
    })
    .await?;
    Ok(Json(v))
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MonitorParams {
    /// How many days back, today included (1 to 366; default 7).
    days: Option<u32>,
}

/// The Monitor tab: what [`crate::history::summarize`] makes of the last
/// `days` days (in the panel's time zone), and how long the mcast server was
/// reachable (`connection`, see [`crate::history::uptime`]). A token limited
/// to categories sees only its announcements' plays.
pub(super) async fn monitor(
    State(s): State<AppState>,
    Extension(caller): Extension<Caller>,
    Params(p): Params<MonitorParams>,
) -> ApiResult<Json<Value>> {
    let n = p.days.unwrap_or(7);
    if !(1..=366).contains(&n) {
        return Err(ApiError::bad("days must be 1 to 366"));
    }
    let cfg = s.load().await?;
    let tz = cfg.tz().unwrap_or_else(|_| jiff::tz::TimeZone::system());
    let now = jiff::Timestamp::now();
    let today = now.to_zoned(tz.clone()).date();
    let start = |d: jiff::civil::Date| -> ApiResult<u64> {
        d.to_zoned(tz.clone())
            .map(|z| z.timestamp().as_millisecond().max(0) as u64)
            .map_err(|e| ApiError::internal(e.to_string()))
    };
    let mut days = Vec::new();
    for back in (0..n).rev() {
        let d = today
            .checked_sub(jiff::Span::new().days(back as i64))
            .map_err(|e| ApiError::internal(e.to_string()))?;
        let next = d
            .tomorrow()
            .map_err(|e| ApiError::internal(e.to_string()))?;
        days.push((d.to_string(), start(d)?, start(next)?));
    }
    let from_ms = days[0].1;
    let now_ms = now.as_millisecond().max(0) as u64;
    let history = s.0.history.clone();
    let v = tokio::task::spawn_blocking(move || {
        let mut runs = Vec::new();
        history.scan(from_ms, |r| {
            if caller.may_play(&cfg, r["announcement"].as_str().unwrap_or("")) {
                runs.push(r.clone());
            }
        });
        // The zones a group (as written in a zone) is in now.
        let zones_of = |group: &str| -> Vec<(String, String)> {
            cfg.zones
                .iter()
                .filter(|z| {
                    z.targets
                        .iter()
                        .any(|t| t.group.trim().eq_ignore_ascii_case(group.trim()))
                })
                .map(|z| (z.id.clone(), z.name.clone()))
                .collect()
        };
        let mut v = crate::history::summarize(&runs, &days, &zones_of);
        v["from_ms"] = from_ms.into();
        v["to_ms"] = now_ms.into();
        v["connection"] = crate::history::uptime(&history.connection_log(), from_ms, now_ms);
        v
    })
    .await?;
    Ok(Json(v))
}
