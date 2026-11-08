//! The Panel's groups, in one go: which groups (categories) there are, in
//! what order, and which buttons are in each, in what order.
//!
//! `GET /panel-layout` gives it as the Panel shows it, with a `revision`.
//! `PUT /panel-layout` saves it all at once: groups made, renamed, recolored
//! and deleted, buttons moved between them, and both orders. Given the
//! `revision` it started from, it's refused (409) if the Panel changed
//! meanwhile, so one admin's edit never quietly undoes another's.

use super::routes::{change, slug_id};
use super::{ApiError, ApiResult, AppState};
use crate::model::{Category, Config, Size, MAX_LAYOUT};
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// The layout tabs the Panel keeps its orders in.
const CATEGORY_ORDER: &str = "panel";
const BUTTON_ORDER: &str = "panel-buttons";
/// The id the page uses for "in no group"; a group can't have it.
const OTHER: &str = "other";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Group {
    /// None for a group to make (its id comes from its name).
    #[serde(default)]
    id: Option<String>,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<Size>,
    /// Its buttons (announcement ids), in order.
    #[serde(default)]
    announcements: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PanelLayout {
    /// What the layout was when the editing started (from GET).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
    groups: Vec<Group>,
    /// Buttons in no group, in order: under "Other", last.
    #[serde(default)]
    other: Vec<String>,
}

/// `ids` in the layout's order for `tab`, then the rest as they were.
fn ordered<'a>(c: &Config, tab: &str, ids: Vec<&'a str>) -> Vec<&'a str> {
    let saved = c.layout.get(tab).map(Vec::as_slice).unwrap_or_default();
    let mut out: Vec<&str> = saved
        .iter()
        .filter_map(|s| ids.iter().find(|i| **i == s.as_str()).copied())
        .collect();
    for i in ids {
        if !out.contains(&i) {
            out.push(i);
        }
    }
    out
}

/// The Panel's groups as every screen shows them.
pub(crate) fn current(c: &Config) -> PanelLayout {
    let buttons = ordered(
        c,
        BUTTON_ORDER,
        c.announcements.iter().map(|a| a.id.as_str()).collect(),
    );
    let category_of = |id: &str| {
        c.announcements
            .iter()
            .find(|a| a.id == id)
            .and_then(|a| a.category.as_deref())
    };
    let known = |cat: Option<&str>| cat.is_some_and(|x| c.categories.iter().any(|k| k.id == x));
    let groups = ordered(
        c,
        CATEGORY_ORDER,
        c.categories.iter().map(|k| k.id.as_str()).collect(),
    )
    .into_iter()
    .filter_map(|id| c.categories.iter().find(|k| k.id == id))
    .map(|k| Group {
        id: Some(k.id.clone()),
        name: k.name.clone(),
        color: k.color.clone(),
        size: k.size,
        announcements: buttons
            .iter()
            .filter(|b| category_of(b) == Some(&k.id))
            .map(|b| b.to_string())
            .collect(),
    })
    .collect();
    let other = buttons
        .iter()
        .filter(|b| !known(category_of(b)))
        .map(|b| b.to_string())
        .collect();
    let mut l = PanelLayout {
        revision: None,
        groups,
        other,
    };
    l.revision = Some(revision(&l));
    l
}

/// A short fingerprint of a layout (without its revision).
fn revision(l: &PanelLayout) -> String {
    let bytes = serde_json::to_vec(&(&l.groups, &l.other)).unwrap_or_default();
    let d = ring::digest::digest(&ring::digest::SHA256, &bytes);
    d.as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect()
}

fn stale(why: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        format!("{why}: the Panel changed while you were arranging it. Load it again and redo your changes"),
    )
}

/// Make `c`'s Panel look like `l`.
fn apply(c: &mut Config, l: PanelLayout) -> ApiResult<PanelLayout> {
    if let Some(r) = &l.revision {
        if *r != current(c).revision.unwrap_or_default() {
            return Err(stale("Not saved"));
        }
    }
    if l.groups.len() > MAX_LAYOUT {
        return Err(ApiError::bad(format!("at most {MAX_LAYOUT} groups")));
    }
    // Every button at most once, and each one the config has.
    let mut seen = HashSet::new();
    for id in l
        .groups
        .iter()
        .flat_map(|g| &g.announcements)
        .chain(&l.other)
    {
        if !seen.insert(id.as_str()) {
            return Err(ApiError::bad(format!("\"{id}\" is in two places")));
        }
        if !c.announcements.iter().any(|a| a.id == *id) {
            return Err(stale(format!("There's no announcement \"{id}\" any more")));
        }
    }
    // Names: given, and each group's its own.
    let mut names = HashSet::new();
    for g in &l.groups {
        let name = g.name.trim();
        if name.is_empty() {
            return Err(ApiError::bad("every group needs a name"));
        }
        if !names.insert(name.to_lowercase()) {
            return Err(ApiError::bad(format!("two groups are called \"{name}\"")));
        }
    }
    // Groups kept, and the ones to make.
    let mut ids = HashSet::new();
    for g in &l.groups {
        if let Some(id) = &g.id {
            if !ids.insert(id.as_str()) {
                return Err(ApiError::bad(format!("the group \"{id}\" is listed twice")));
            }
            if !c.categories.iter().any(|k| k.id == *id) {
                return Err(stale(format!("There's no group \"{id}\" any more")));
            }
        }
    }
    let mut taken: HashSet<String> = c
        .categories
        .iter()
        .map(|k| k.id.clone())
        .chain([OTHER.to_string()])
        .collect();
    let mut categories = Vec::with_capacity(l.groups.len());
    let mut assign: HashMap<String, Option<String>> = HashMap::new();
    let mut order = Vec::new();
    for g in l.groups {
        let mut k = match &g.id {
            Some(id) => c
                .categories
                .iter()
                .find(|k| k.id == *id)
                .cloned()
                .expect("checked"),
            None => {
                let id = slug_id(&g.name, "group", |i| taken.contains(i));
                taken.insert(id.clone());
                Category {
                    id,
                    name: String::new(),
                    color: None,
                    size: None,
                }
            }
        };
        k.name = g.name.trim().to_string();
        k.color = g.color;
        k.size = g.size;
        for a in g.announcements {
            assign.insert(a.clone(), Some(k.id.clone()));
            order.push(a);
        }
        categories.push(k);
    }
    for a in l.other {
        assign.insert(a.clone(), None);
        order.push(a);
    }
    // A button left out stays where it is, unless its group is going.
    let kept: HashSet<&str> = categories.iter().map(|k| k.id.as_str()).collect();
    for a in &c.announcements {
        if assign.contains_key(&a.id) {
            continue;
        }
        if let Some(cat) = &a.category {
            if !kept.contains(cat.as_str()) {
                return Err(stale(format!(
                    "\"{}\" is still in the group \"{cat}\"",
                    a.name
                )));
            }
        }
    }
    // The buttons' order: each group's, then Other's, as given; any left
    // out after, as they were.
    let mut buttons: Vec<String> = order;
    for id in ordered(
        c,
        BUTTON_ORDER,
        c.announcements.iter().map(|a| a.id.as_str()).collect(),
    ) {
        if !assign.contains_key(id) {
            buttons.push(id.to_string());
        }
    }
    for a in &mut c.announcements {
        if let Some(cat) = assign.get(&a.id) {
            a.category = cat.clone();
        }
    }
    c.layout.insert(
        CATEGORY_ORDER.into(),
        categories.iter().map(|k| k.id.clone()).collect(),
    );
    buttons.truncate(MAX_LAYOUT);
    c.layout.insert(BUTTON_ORDER.into(), buttons);
    c.categories = categories;
    Ok(current(c))
}

pub(super) async fn get_layout(State(s): State<AppState>) -> ApiResult<Json<PanelLayout>> {
    Ok(Json(current(&s.load().await?)))
}

pub(super) async fn put_layout(
    State(s): State<AppState>,
    body: Result<Json<PanelLayout>, JsonRejection>,
) -> ApiResult<Json<PanelLayout>> {
    let Json(l) = body?;
    change(&s, move |c| apply(c, l)).await.map(Json)
}
