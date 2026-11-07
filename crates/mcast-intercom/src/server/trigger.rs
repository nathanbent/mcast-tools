//! The trigger API: other systems play and stop announcements by name.
//!
//! `GET` or `POST /trigger/{id or name}` plays one, `.../stop` stops it, and
//! `/stop-all` stops everything the caller may stop. `GET` is for devices
//! that can only fetch a URL (with `?access_token=` for trigger tokens);
//! `POST` can take a JSON body. Either can narrow the play to some of the
//! announcement's zones (`zones`) or change how often it repeats (`repeat`).

use super::{ApiError, ApiResult, AppState, Caller};
use crate::model::{Audio, Config, MAX_REPEAT};
use crate::runner::{PlayError, Run};
use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{Method, StatusCode};
use axum::{Extension, Json};
use serde::Deserialize;

/// What a trigger may change for this one play.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Overrides {
    /// Only these of the announcement's zones (ids or names).
    zones: Option<Vec<String>>,
    repeat: Option<u32>,
}

/// GET and POST only: a HEAD (a link preview, say) must not page anyone.
fn get_or_post(m: &Method) -> ApiResult<()> {
    if *m == Method::GET || *m == Method::POST {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "use GET or POST to trigger",
        ))
    }
}

/// `zones=a,b&repeat=2` from the URL, `{"zones": [...], "repeat": 2}` from a
/// POST body. Both may be given: the body wins.
fn overrides(query: Option<&str>, body: &[u8]) -> ApiResult<Overrides> {
    let mut o = Overrides::default();
    for kv in query.unwrap_or("").split('&') {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        let v = decode(v);
        match k {
            "zones" | "zone" => {
                let list = o.zones.get_or_insert_with(Vec::new);
                list.extend(
                    v.split(',')
                        .map(str::trim)
                        .filter(|z| !z.is_empty())
                        .map(String::from),
                );
            }
            "repeat" => {
                o.repeat = Some(
                    v.trim()
                        .parse()
                        .map_err(|_| ApiError::bad(format!("repeat=\"{v}\" isn't a number")))?,
                )
            }
            _ => {}
        }
    }
    if body.iter().any(|b| !b.is_ascii_whitespace()) {
        let b: Overrides =
            serde_json::from_slice(body).map_err(|e| ApiError::bad(format!("the body: {e}")))?;
        if b.zones.is_some() {
            o.zones = b.zones;
        }
        if b.repeat.is_some() {
            o.repeat = b.repeat;
        }
    }
    Ok(o)
}

/// `+` and `%XX` in a query value.
fn decode(v: &str) -> String {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (
            b[i],
            b.get(i + 1).copied().and_then(hex),
            b.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (b'+', _, _) => {
                out.push(b' ');
                i += 1;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The announcement `which` (id or name) if the caller may play it.
fn find<'a>(
    cfg: &'a Config,
    caller: &Caller,
    which: &str,
) -> ApiResult<&'a crate::model::Announcement> {
    let a = cfg.announcement(which.trim()).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            format!("no announcement \"{which}\""),
        )
    })?;
    if !caller.may_play(&a.id) {
        return Err(caller.cant(&format!("play \"{}\"", a.name)));
    }
    Ok(a)
}

/// Play an announcement now; the run (`201`).
pub(super) async fn trigger(
    State(s): State<AppState>,
    Extension(caller): Extension<Caller>,
    method: Method,
    Path(which): Path<String>,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Run>)> {
    get_or_post(&method)?;
    let o = overrides(query.as_deref(), &body)?;
    let mut cfg = s.load().await?;
    let a = find(&cfg, &caller, &which)?.clone();
    if a.audio == Audio::Live {
        return Err(ApiError::bad(format!(
            "\"{}\" is live: talk on it from the panel's page",
            a.name
        )));
    }
    let mut changed = a.clone();
    if let Some(zones) = &o.zones {
        if zones.is_empty() {
            return Err(ApiError::bad("zones: give at least one"));
        }
        // Any of the zones it plays to, its own or its parts'.
        let all: Vec<String> = a
            .sections()
            .iter()
            .flat_map(|(_, zs)| zs.iter().cloned())
            .collect();
        let mut ids = Vec::new();
        for z in zones {
            let zone = cfg
                .zones
                .iter()
                .find(|x| x.id == *z)
                .or_else(|| cfg.zones.iter().find(|x| x.name.eq_ignore_ascii_case(z)));
            match zone {
                Some(zone) if all.contains(&zone.id) => ids.push(zone.id.clone()),
                _ => {
                    return Err(ApiError::bad(format!(
                        "zone \"{z}\" isn't one of \"{}\"'s zones ({})",
                        a.name,
                        all.join(", ")
                    )))
                }
            }
        }
        // Each section keeps only the zones asked for; parts left with none go.
        changed.zones.retain(|z| ids.contains(z));
        for p in &mut changed.parts {
            p.zones.retain(|z| ids.contains(z));
        }
        changed.parts.retain(|p| !p.zones.is_empty());
    }
    if let Some(n) = o.repeat {
        if !(1..=MAX_REPEAT).contains(&n) {
            return Err(ApiError::bad(format!("repeat must be 1 to {MAX_REPEAT}")));
        }
        changed.repeat = n;
    }
    // Play this one with the changes, without saving them.
    if let Some(slot) = cfg.announcements.iter_mut().find(|x| x.id == a.id) {
        *slot = changed;
    }
    let runner = s.0.runner.clone();
    let trigger = caller.api_trigger();
    let id = a.id.clone();
    let run = tokio::task::spawn_blocking(move || runner.play(&cfg, &id, trigger))
        .await?
        .map_err(|e| match e {
            PlayError::NoAnnouncement(_) => ApiError::new(StatusCode::NOT_FOUND, e.to_string()),
            PlayError::Server(m) => ApiError::new(StatusCode::BAD_GATEWAY, m),
        })?;
    Ok((StatusCode::CREATED, Json(run)))
}

/// Stop runs that `pick` chooses and the caller may stop; returns them.
async fn stop_where(
    s: &AppState,
    caller: &Caller,
    pick: impl Fn(&Run) -> bool,
) -> ApiResult<Json<Vec<Run>>> {
    let ids: Vec<u64> =
        s.0.runner
            .list()
            .into_iter()
            .filter(|r| !r.state.is_over() && caller.may_play(&r.announcement) && pick(r))
            .map(|r| r.id)
            .collect();
    let runner = s.0.runner.clone();
    let stopped = tokio::task::spawn_blocking(move || {
        ids.into_iter()
            .filter_map(|id| runner.stop(id))
            .collect::<Vec<_>>()
    })
    .await?;
    Ok(Json(stopped))
}

/// Stop an announcement wherever it's playing; the runs stopped.
pub(super) async fn trigger_stop(
    State(s): State<AppState>,
    Extension(caller): Extension<Caller>,
    method: Method,
    Path(which): Path<String>,
) -> ApiResult<Json<Vec<Run>>> {
    get_or_post(&method)?;
    let cfg = s.load().await?;
    let id = find(&cfg, &caller, &which)?.id.clone();
    stop_where(&s, &caller, |r| r.announcement == id).await
}

/// Stop everything the caller may stop: all of it, or a trigger token's
/// announcements.
pub(super) async fn stop_all(
    State(s): State<AppState>,
    Extension(caller): Extension<Caller>,
    method: Method,
) -> ApiResult<Json<Vec<Run>>> {
    get_or_post(&method)?;
    stop_where(&s, &caller, |_| true).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_from_the_url_and_the_body() {
        let o = overrides(
            Some("access_token=x&zones=hall,Front+yard&zone=gym&repeat=2"),
            b"",
        )
        .unwrap();
        assert_eq!(o.zones.unwrap(), ["hall", "Front yard", "gym"]);
        assert_eq!(o.repeat, Some(2));
        let o = overrides(Some("repeat=2"), br#"{"repeat": 3, "zones": ["a"]}"#).unwrap();
        assert_eq!(
            (o.repeat, o.zones.unwrap()),
            (Some(3), vec!["a".to_string()])
        );
        assert!(overrides(Some("repeat=lots"), b"").is_err());
        assert!(overrides(None, br#"{"volume": 3}"#).is_err());
        assert_eq!(decode("a%2Cb%20c"), "a,b c");
    }
}
