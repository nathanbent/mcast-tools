//! The built-in chimes as clients list them, and which ones are hidden.
//!
//! There are a lot of built-in chimes; most sites want a handful. Hiding one
//! takes it out of the lists that the web GUI, `mcast tui` and the intercom
//! panel offer, but a request that names it still plays it, so nothing that
//! already uses it breaks. Which are hidden is kept in a small JSON file,
//! `chimes.json` in the mcast config directory (or `$MCAST_CHIMES`):
//!
//! ```json
//! { "hidden": ["klaxon", "cowbell"] }
//! ```

use anyhow::{Context, Result};
use mcast_audio::chime::BuiltIn;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Where the chime settings are kept: `$MCAST_CHIMES`, else `chimes.json` in
/// the mcast config directory (beside the phonebook).
pub fn default_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("MCAST_CHIMES").filter(|v| !v.is_empty()) {
        return Ok(p.into());
    }
    let dir = crate::phonebook::config_dir()
        .context("can't find a config directory; set MCAST_CHIMES to a file path")?;
    Ok(dir.join("chimes.json"))
}

/// One built-in chime, as `GET /chimes` lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChimeInfo {
    pub name: &'static str,
    /// "chime", "bell", "tone" or "alert".
    pub category: &'static str,
    pub description: &'static str,
    pub hidden: bool,
}

/// Which built-in chimes are hidden from lists.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChimeSettings {
    /// Names of hidden chimes. Names this version doesn't know are kept, so
    /// an older server doesn't forget a newer one's settings.
    #[serde(default)]
    pub hidden: BTreeSet<String>,
}

impl ChimeSettings {
    /// Load from `path`; a missing file hides nothing.
    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        std::fs::write(&tmp, text)
            .and_then(|_| std::fs::rename(&tmp, path))
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn is_hidden(&self, b: BuiltIn) -> bool {
        self.hidden.contains(b.name())
    }

    /// Hide or show a chime. Returns whether that changed anything.
    pub fn set_hidden(&mut self, b: BuiltIn, hidden: bool) -> bool {
        if hidden {
            self.hidden.insert(b.name().to_string())
        } else {
            self.hidden.remove(b.name())
        }
    }

    /// Every built-in chime, grouped by category, with whether it's hidden.
    pub fn list(&self) -> Vec<ChimeInfo> {
        BuiltIn::ALL
            .iter()
            .map(|&b| ChimeInfo {
                name: b.name(),
                category: b.category().name(),
                description: b.description(),
                hidden: self.is_hidden(b),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_keeps_unknown_names() {
        let dir = std::env::temp_dir().join(format!("mcast-chimes-{}", std::process::id()));
        let path = dir.join("chimes.json");
        let mut s = ChimeSettings::load_from(&path).unwrap();
        assert!(s.hidden.is_empty());
        assert!(s.set_hidden(BuiltIn::Cowbell, true));
        assert!(!s.set_hidden(BuiltIn::Cowbell, true));
        s.hidden.insert("from-the-future".into());
        s.save_to(&path).unwrap();
        let back = ChimeSettings::load_from(&path).unwrap();
        assert_eq!(back, s);
        let list = back.list();
        assert_eq!(list.len(), BuiltIn::ALL.len());
        assert!(list.iter().find(|c| c.name == "cowbell").unwrap().hidden);
        assert!(!list.iter().find(|c| c.name == "rising").unwrap().hidden);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_typos() {
        assert!(serde_json::from_str::<ChimeSettings>(r#"{"hiden": []}"#).is_err());
    }
}
