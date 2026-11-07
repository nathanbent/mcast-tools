//! The phonebook: named groups, and resolving names in requests.

use crate::{ApiError, ApiResult, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mcast_control::phonebook::{Entry, Phonebook};
use mcast_control::Event;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::net::SocketAddr;

impl AppState {
    fn load_phonebook(&self) -> ApiResult<Phonebook> {
        Phonebook::load_from(&self.0.phonebook).map_err(|e| ApiError::internal(format!("{e:#}")))
    }

    /// Read a request whose `group` may be a phonebook name instead of an
    /// address, as everywhere else a group is asked for.
    pub(crate) fn named<T: DeserializeOwned>(
        &self,
        body: Result<Json<serde_json::Value>, JsonRejection>,
    ) -> ApiResult<T> {
        let Json(mut v) = body?;
        if let Some(g) = v.get_mut("group") {
            if let Some(s) = g.as_str().filter(|s| s.parse::<SocketAddr>().is_err()) {
                let addr = self
                    .load_phonebook()?
                    .resolve(s)
                    .map_err(|e| ApiError::bad(format!("group: {e:#}")))?;
                *g = addr.to_string().into();
            }
        }
        serde_json::from_value(v).map_err(|e| {
            ApiError::bad(format!(
                "Failed to deserialize the JSON body into the target type: {e}"
            ))
        })
    }
}

pub(crate) async fn list_phonebook(State(s): State<AppState>) -> ApiResult<Json<Vec<Entry>>> {
    Ok(Json(s.load_phonebook()?.entries))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PutEntry {
    group: SocketAddr,
    #[serde(default)]
    description: String,
    /// Rename the entry from the name in the path to this.
    #[serde(default)]
    name: Option<String>,
}

/// Add or replace an entry (renaming it, with `name` in the body).
pub(crate) async fn put_phonebook(
    State(s): State<AppState>,
    Path(name): Path<String>,
    body: Result<Json<PutEntry>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<Entry>)> {
    let Json(req) = body?;
    let _write = s.0.phonebook_lock.lock().await;
    let mut book = s.load_phonebook()?;
    let mut name = name.trim().to_string();
    if let Some(to) = req.name.as_deref().map(str::trim).filter(|to| *to != name) {
        if book.get(&name).is_some() {
            book.rename(&name, to)
                .map_err(|e| ApiError::bad(e.to_string()))?;
        } else if book.get(to).is_some() {
            return Err(ApiError::bad(format!(
                "\"{to}\" is already in the phonebook"
            )));
        }
        name = to.to_string();
    }
    let entry = Entry {
        name,
        group: req.group,
        description: req.description.trim().to_string(),
    };
    let new = book
        .put(entry.clone())
        .map_err(|e| ApiError::bad(e.to_string()))?;
    book.save()
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    let _ = s.0.events.send(Event::PhonebookChanged);
    let status = if new {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(entry)))
}

pub(crate) async fn delete_phonebook(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<Entry>> {
    let _write = s.0.phonebook_lock.lock().await;
    let mut book = s.load_phonebook()?;
    let entry = book
        .remove(&name)
        .map_err(|e| ApiError::new(StatusCode::NOT_FOUND, e.to_string()))?;
    book.save()
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    let _ = s.0.events.send(Event::PhonebookChanged);
    Ok(Json(entry))
}
