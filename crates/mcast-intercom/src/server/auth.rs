//! Who's calling, and what they may do (`docs/intercom-tokens.md`).
//!
//! [`authenticate`] runs on every API request: it finds the caller from the
//! startup token or `tokens.json` (401 if neither matches), and leaves a
//! [`Caller`] on the request. [`authorize`] runs once the route is known and
//! checks the caller's role against [`access`] (403 if it falls short).
//! Handlers that play or stop runs also check a trigger token's
//! announcements.
//!
//! Every route needs a line in [`access`]: one without is refused for
//! everyone, so a new route can't be left open by mistake.

use super::{ApiError, ApiResult, AppState, Event, API_PREFIX};
use crate::now_ms;
use crate::runner::Trigger;
use crate::tokens::{Lookup, NewToken, Role, Token};
use axum::extract::rejection::JsonRejection;
use axum::extract::{MatchedPath, Path, Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::{json, Value};

/// The name runs record for the startup token (`--panel-token`).
pub const STARTUP_TOKEN: &str = "startup token";

/// The caller of a request, left on it by [`authenticate`].
#[derive(Debug, Clone)]
pub struct Caller {
    /// The token's name; `None` without one (an open panel on loopback).
    pub name: Option<String>,
    pub role: Role,
    /// A trigger token's announcements; empty for any.
    pub announcements: Vec<String>,
    /// An operator token's categories; empty for all.
    pub categories: Vec<String>,
    /// The token came in the URL (`?access_token=`), not a header.
    pub in_url: bool,
}

impl Caller {
    fn open() -> Self {
        Self {
            name: None,
            role: Role::Admin,
            announcements: Vec::new(),
            categories: Vec::new(),
            in_url: false,
        }
    }

    fn token(t: &Token, in_url: bool) -> Self {
        Self {
            name: Some(t.name.clone()),
            role: t.role,
            announcements: t.announcements.clone(),
            categories: t.categories.clone(),
            in_url,
        }
    }

    /// Whether it may play, see and stop runs of announcement `id`: a
    /// trigger token's announcements, an operator token's categories (an
    /// announcement in none, or gone, isn't in them).
    pub fn may_play(&self, cfg: &crate::model::Config, id: &str) -> bool {
        match self.role {
            Role::Trigger => {
                self.announcements.is_empty() || self.announcements.iter().any(|a| a == id)
            }
            Role::Operator if !self.categories.is_empty() => cfg
                .announcements
                .iter()
                .find(|a| a.id == id)
                .and_then(|a| a.category.as_ref())
                .is_some_and(|c| self.categories.contains(c)),
            _ => true,
        }
    }

    /// Whether it sees only some announcements' runs (so filtering them
    /// needs the config).
    pub fn is_limited(&self) -> bool {
        match self.role {
            Role::Trigger => !self.announcements.is_empty(),
            Role::Operator => !self.categories.is_empty(),
            _ => false,
        }
    }

    /// What runs it starts record.
    pub fn trigger(&self) -> Trigger {
        match (&self.name, self.role) {
            (Some(n), Role::Trigger) => Trigger::Api {
                by: Some(n.clone()),
            },
            (by, _) => Trigger::Manual { by: by.clone() },
        }
    }

    /// What runs started through the trigger API record.
    pub fn api_trigger(&self) -> Trigger {
        Trigger::Api {
            by: self.name.clone(),
        }
    }

    /// "this token (Doorbell, trigger)", for messages.
    pub fn describe(&self) -> String {
        match &self.name {
            Some(n) => format!("this token ({n}, {})", self.role),
            None => "this caller".into(),
        }
    }

    pub fn cant(&self, what: &str) -> ApiError {
        ApiError::new(
            StatusCode::FORBIDDEN,
            format!("{} can't {what}", self.describe()),
        )
    }
}

/// How the panel checks callers.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// The startup token (`--panel-token`): admin, and not in `tokens.json`.
    pub token: Option<String>,
    /// `tokens.json`.
    pub tokens: std::path::PathBuf,
    /// Requests without a token are let in as admin (no startup token, on
    /// loopback). Requests with a known token still get its role.
    pub open: bool,
}

/// Compare without leaking how much of the token matched.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn unauthorized(message: &str) -> Response {
    let mut r = ApiError::new(StatusCode::UNAUTHORIZED, message).into_response();
    r.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        "Bearer".parse().expect("valid header"),
    );
    r
}

pub(super) async fn authenticate(
    State(s): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    // Browsers' EventSource, <audio> and WebSocket can't set headers, so the
    // token may come in the URL; `authorize` says where that's allowed.
    let query = req
        .uri()
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("access_token=")))
        .filter(|t| !t.is_empty())
        .map(url_decode);
    let in_url = bearer.is_none() && query.is_some();
    let caller = match bearer.or(query) {
        None if s.0.auth.open => Caller::open(),
        None => return unauthorized("missing or wrong token"),
        Some(t) => {
            if s.0
                .auth
                .token
                .as_deref()
                .is_some_and(|k| same(t.as_bytes(), k.as_bytes()))
            {
                Caller {
                    name: Some(STARTUP_TOKEN.into()),
                    ..Caller::open()
                }
                .with_url(in_url)
            } else {
                let store = s.0.tokens.clone();
                let now = now_ms();
                let found = tokio::task::spawn_blocking(move || store.authenticate(&t, now))
                    .await
                    .unwrap_or(Lookup::Unknown);
                match found {
                    Lookup::Valid(tok) => Caller::token(&tok, in_url),
                    // An open panel ignores tokens it doesn't know, as before.
                    _ if s.0.auth.open => Caller::open(),
                    Lookup::Expired(tok) => {
                        return unauthorized(&format!(
                            "the token \"{}\" expired after {}",
                            tok.name,
                            tok.expires.as_deref().unwrap_or("its last day")
                        ))
                    }
                    Lookup::Unknown => return unauthorized("missing or wrong token"),
                }
            }
        }
    };
    req.extensions_mut().insert(caller);
    next.run(req).await
}

impl Caller {
    fn with_url(mut self, in_url: bool) -> Self {
        self.in_url = in_url;
        self
    }
}

/// A query value decoded: `%XX`, and `+` as a space.
pub(super) fn url_decode(v: &str) -> String {
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

/// What a route needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Any valid token.
    Anyone,
    /// Look at the setup: admin, operator.
    Read,
    /// Hear what happens (`/events`): admin, operator, media.
    Events,
    /// Play, see and stop runs: admin, operator, trigger (its announcements).
    Play,
    /// The panel's controls beyond playing (all stop, push to talk): admin, operator.
    Operate,
    /// See sound files: admin, operator, media.
    MediaRead,
    /// Change sound files: admin, media.
    MediaWrite,
    /// Change the setup, tokens included: admin.
    Admin,
}

impl Access {
    pub fn allows(self, role: Role) -> bool {
        use Role::*;
        match self {
            Access::Anyone => true,
            Access::Read | Access::Operate => matches!(role, Admin | Operator),
            Access::Events | Access::MediaRead => matches!(role, Admin | Operator | Media),
            Access::Play => matches!(role, Admin | Operator | Trigger),
            Access::MediaWrite => matches!(role, Admin | Media),
            Access::Admin => role == Admin,
        }
    }

    /// For "this token can't ...".
    fn what(self) -> &'static str {
        match self {
            Access::Anyone => "do that",
            Access::Read => "see the panel's setup",
            Access::Events => "follow the panel's events",
            Access::Play => "play announcements",
            Access::Operate => "use the panel's controls",
            Access::MediaRead => "see sound files",
            Access::MediaWrite => "change sound files",
            Access::Admin => "change the panel's setup",
        }
    }
}

/// What `method` on route `path` (as routed: `/zones/{id}`) needs; `None`
/// for a route with no rule, which nobody may use.
pub fn access(method: &Method, path: &str) -> Option<Access> {
    use Access::*;
    let get = *method == Method::GET;
    Some(match path {
        "/health" if get => Anyone,
        "/backend"
        | "/config"
        | "/upcoming"
        | "/phonebook"
        | "/listeners"
        | "/speakers"
        | "/history"
        | "/history.csv"
        | "/stats"
        | "/monitor"
        | "/chimes/{name}/preview"
            if get =>
        {
            Read
        }
        "/zones"
        | "/zones/{id}"
        | "/announcements"
        | "/announcements/{id}"
        | "/schedules"
        | "/schedules/{id}"
        | "/sites"
        | "/sites/{id}"
        | "/schedule-groups"
        | "/schedule-groups/{id}"
        | "/settings"
        | "/chimes"
        | "/playlists"
        | "/playlists/{id}"
        | "/categories"
        | "/categories/{id}" => {
            if get {
                Read
            } else {
                Admin
            }
        }
        "/events" if get => Events,
        "/announcements/{id}/play" if *method == Method::POST => Play,
        "/runs/{id}" => Play,
        "/runs" if get => Play,
        "/runs" if *method == Method::DELETE => Operate,
        "/live/{id}" if get => Operate,
        "/media" | "/library" if get => MediaRead,
        "/media/{*name}" => {
            if get {
                MediaRead
            } else {
                MediaWrite
            }
        }
        "/media-folders"
        | "/media-folders/{*name}"
        | "/media-move"
        | "/media-hidden"
        | "/media-tags" => MediaWrite,
        "/tokens" | "/tokens/{id}" | "/layout/{tab}" => Admin,
        "/backup" if get => Admin,
        "/restore" if *method == Method::POST => Admin,
        // Any method: the handlers take GET and POST, and refuse the rest.
        "/trigger/{which}" | "/trigger/{which}/stop" | "/stop-all" => Play,
        "/webhooks" | "/webhooks/{id}" | "/webhooks/{id}/test" => Admin,
        _ => return None,
    })
}

/// Routes where a token may come in the URL. For the page, where browsers
/// can't send a header: events, sound files and chime previews for
/// `<audio>`, and push to talk's WebSocket. For trigger tokens, the trigger
/// API's GET form, for devices that can only fetch a URL; nothing else.
pub fn url_token_ok(method: &Method, path: &str, role: Role) -> bool {
    // HEAD reads as GET would (and the trigger API refuses it outright).
    (*method == Method::GET || *method == Method::HEAD)
        && if role == Role::Trigger {
            matches!(
                path,
                "/trigger/{which}" | "/trigger/{which}/stop" | "/stop-all"
            )
        } else {
            matches!(
                path,
                "/events" | "/media/{*name}" | "/chimes/{name}/preview" | "/live/{id}"
            )
        }
}

pub(super) async fn authorize(req: Request, next: Next) -> Response {
    let Some(caller) = req.extensions().get::<Caller>().cloned() else {
        return ApiError::internal("no caller on the request").into_response();
    };
    let path = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str())
        .unwrap_or("");
    let path = path.strip_prefix(API_PREFIX).unwrap_or(path);
    let method = req.method();
    let Some(need) = access(method, path) else {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            format!("{method} {path} has no access rule"),
        )
        .into_response();
    };
    // A token in the URL only where it can't be sent in a header.
    if caller.in_url && !url_token_ok(method, path, caller.role) {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "send the token in an Authorization: Bearer header here, not in the URL",
        )
        .into_response();
    }
    if !need.allows(caller.role) {
        return caller.cant(need.what()).into_response();
    }
    next.run(req).await
}

// ---- /tokens ----

/// A token for the API: never its hash.
fn view(t: &Token) -> Value {
    let mut v = serde_json::to_value(t).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.remove("hash");
        o.insert("expired".into(), t.expired(now_ms()).into());
        o.entry("announcements").or_insert(json!([]));
        o.entry("categories").or_insert(json!([]));
        o.entry("expires").or_insert(Value::Null);
        o.entry("last_used_ms").or_insert(Value::Null);
    }
    v
}

fn bad(e: anyhow::Error) -> ApiError {
    let m = format!("{e:#}");
    if m.starts_with("no token") {
        ApiError::new(StatusCode::NOT_FOUND, m)
    } else if m.contains("already") {
        ApiError::new(StatusCode::CONFLICT, m)
    } else {
        ApiError::bad(m)
    }
}

pub(super) async fn list_tokens(State(s): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let store = s.0.tokens.clone();
    let list = tokio::task::spawn_blocking(move || store.list()).await?;
    Ok(Json(list.iter().map(view).collect()))
}

fn strings(v: &Value, key: &str) -> ApiResult<Option<Vec<String>>> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| ApiError::bad(format!("{key}: expected a list of strings")))
            })
            .collect::<ApiResult<Vec<_>>>()
            .map(Some),
        Some(_) => Err(ApiError::bad(format!("{key}: expected a list of strings"))),
    }
}

fn string(v: &Value, key: &str) -> ApiResult<Option<String>> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(ApiError::bad(format!("{key}: expected a string"))),
    }
}

/// `{name, role, announcements?, expires?}` → `{token, secret}`; the only
/// time the secret is shown.
pub(super) async fn create_token(
    State(s): State<AppState>,
    body: Result<Json<Value>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let Json(v) = body?;
    let new = NewToken {
        name: string(&v, "name")?.ok_or_else(|| ApiError::bad("a token needs a name"))?,
        role: string(&v, "role")?
            .ok_or_else(|| {
                ApiError::bad("a token needs a role: admin, operator, trigger or media")
            })?
            .parse()
            .map_err(bad)?,
        announcements: strings(&v, "announcements")?.unwrap_or_default(),
        categories: strings(&v, "categories")?.unwrap_or_default(),
        expires: string(&v, "expires")?,
    };
    let cfg = s.load().await?;
    let store = s.0.tokens.clone();
    let (token, secret) =
        tokio::task::spawn_blocking(move || store.modify(|f| f.add(new, &cfg, now_ms())))
            .await?
            .map_err(bad)?;
    let _ = s.0.events.send(Event::ConfigChanged);
    Ok((
        StatusCode::CREATED,
        Json(json!({ "token": view(&token), "secret": secret })),
    ))
}

/// `{name?, announcements?, expires?}`: `expires: null` clears it. The role
/// stays: make a new token for another.
pub(super) async fn put_token(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> ApiResult<Json<Value>> {
    let Json(v) = body?;
    if !v.is_object() {
        return Err(ApiError::bad("expected a JSON object"));
    }
    let name = string(&v, "name")?;
    let announcements = strings(&v, "announcements")?;
    let categories = strings(&v, "categories")?;
    let expires: Option<Option<String>> = match v.get("expires") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(d)) => Some(Some(d.clone())),
        Some(_) => {
            return Err(ApiError::bad(
                "expires: expected a date like 2027-01-31, or null",
            ))
        }
    };
    let role = string(&v, "role")?;
    let cfg = s.load().await?;
    let store = s.0.tokens.clone();
    let token = tokio::task::spawn_blocking(move || {
        store.modify(|f| {
            let i = f.find(&id).filter(|i| f.tokens[*i].id == id);
            let Some(i) = i else {
                anyhow::bail!("no token \"{id}\"");
            };
            if let Some(r) = role {
                let r: Role = r.parse()?;
                if r != f.tokens[i].role {
                    anyhow::bail!("a token's role can't change: make a new token instead");
                }
            }
            f.change(
                &id,
                name.as_deref(),
                announcements.as_deref(),
                categories.as_deref(),
                expires.as_ref().map(|e| e.as_deref()),
                &cfg,
                now_ms(),
            )
        })
    })
    .await?
    .map_err(bad)?;
    let _ = s.0.events.send(Event::ConfigChanged);
    Ok(Json(view(&token)))
}

/// Revoke a token; returns it.
pub(super) async fn delete_token(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let store = s.0.tokens.clone();
    let token = tokio::task::spawn_blocking(move || {
        store.modify(|f| match f.tokens.iter().position(|t| t.id == id) {
            Some(i) => Ok(f.tokens.remove(i)),
            None => anyhow::bail!("no token \"{id}\""),
        })
    })
    .await?
    .map_err(bad)?;
    let _ = s.0.events.send(Event::ConfigChanged);
    Ok(Json(view(&token)))
}

/// `/health`, with who's asking.
pub(super) async fn health(Extension(caller): Extension<Caller>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "name": caller.name,
        "role": caller.role,
        "announcements": caller.announcements,
        "categories": caller.categories,
    }))
}
