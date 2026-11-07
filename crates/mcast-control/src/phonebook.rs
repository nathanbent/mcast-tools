//! The phonebook: multicast groups saved under a name, with a description.
//!
//! Anywhere a group is asked for (`-g` on the command line, `group` in API
//! requests, the web GUI's group fields), a phonebook name can be given in
//! its place. Names match without regard to case.
//!
//! Entries live in a small text file (`~/.config/mcast/phonebook.conf`, or
//! `$MCAST_PHONEBOOK`) that can be edited by hand:
//!
//! ```text
//! [zone-1]
//! group = 239.192.2.1:10000
//! description = Ground floor paging speakers (G.711)
//! ```

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Longest name and description accepted.
pub const MAX_NAME: usize = 64;
pub const MAX_DESCRIPTION: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub group: SocketAddr,
    #[serde(default)]
    pub description: String,
}

/// Where the phonebook is kept: `$MCAST_PHONEBOOK`, else `phonebook.conf`
/// in the mcast config directory (`$XDG_CONFIG_HOME/mcast`,
/// `~/.config/mcast`, or `%APPDATA%\mcast` on Windows).
pub fn default_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("MCAST_PHONEBOOK").filter(|v| !v.is_empty()) {
        return Ok(p.into());
    }
    let dir = config_dir()
        .context("can't find a config directory; set MCAST_PHONEBOOK to a file path")?;
    Ok(dir.join("phonebook.conf"))
}

/// The mcast config directory: `$XDG_CONFIG_HOME/mcast`, `~/.config/mcast`,
/// or `%APPDATA%\mcast` on Windows.
pub(crate) fn config_dir() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let dir = if cfg!(windows) {
        var("APPDATA")
    } else {
        var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|h| h.join(".config")))
    };
    dir.map(|d| d.join("mcast"))
}

/// Names go in `[...]` headers and stand in for groups, so they can't look
/// like an address. Letters, digits, spaces and `-_.` are allowed.
pub fn check_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("a phonebook name can't be empty");
    }
    if name.chars().count() > MAX_NAME {
        bail!("phonebook name \"{name}\" is too long (at most {MAX_NAME} characters)");
    }
    let ok = name.trim() == name
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == ' ' || "-_.".contains(c));
    if !ok {
        bail!(
            "invalid phonebook name \"{name}\": use letters, digits, spaces, '-', '_' and '.', \
             not starting with '-' or a space"
        );
    }
    if name.parse::<SocketAddr>().is_ok() {
        bail!("invalid phonebook name \"{name}\": it looks like an address");
    }
    Ok(())
}

fn check_description(d: &str) -> Result<()> {
    if d.chars().count() > MAX_DESCRIPTION {
        bail!("description is too long (at most {MAX_DESCRIPTION} characters)");
    }
    if d.chars().any(char::is_control) {
        bail!("a description is one line, without control characters");
    }
    Ok(())
}

fn parse_file(text: &str) -> Result<Vec<Entry>> {
    struct Partial {
        name: String,
        group: Option<SocketAddr>,
        description: String,
        line: usize,
    }
    let mut out: Vec<Partial> = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let n = n + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let name = name.trim().to_string();
            check_name(&name).with_context(|| format!("line {n}"))?;
            if out.iter().any(|p| p.name.eq_ignore_ascii_case(&name)) {
                bail!("line {n}: \"{name}\" is in the phonebook twice");
            }
            out.push(Partial {
                name,
                group: None,
                description: String::new(),
                line: n,
            });
            continue;
        }
        let Some(p) = out.last_mut() else {
            bail!("line {n}: expected a [name] header before \"{line}\"");
        };
        let Some((key, value)) = line.split_once('=') else {
            bail!(
                "line {n}: expected \"group = ADDR:PORT\" or \"description = ...\", got \"{line}\""
            );
        };
        let value = value.trim();
        match key.trim() {
            "group" => {
                p.group = Some(value.parse().map_err(|_| {
                    anyhow::anyhow!(
                        "line {n}: \"{value}\" isn't a group and port, e.g. 239.69.0.1:5004"
                    )
                })?)
            }
            "description" => p.description = value.to_string(),
            k => bail!("line {n}: unknown setting \"{k}\" (expected group or description)"),
        }
    }
    out.into_iter()
        .map(|p| {
            let group = p.group.with_context(|| {
                format!(
                    "line {}: \"{}\" has no \"group = ADDR:PORT\"",
                    p.line, p.name
                )
            })?;
            Ok(Entry {
                name: p.name,
                group,
                description: p.description,
            })
        })
        .collect()
}

fn format_file(entries: &[Entry]) -> String {
    let mut s = String::from(
        "# mcast phonebook (see `mcast book --help`).\n\
         # Each [name] is a multicast group that can be used by name wherever a group goes.\n",
    );
    for e in entries {
        s += &format!("\n[{}]\ngroup = {}\n", e.name, e.group);
        if !e.description.is_empty() {
            s += &format!("description = {}\n", e.description);
        }
    }
    s
}

/// The phonebook file, loaded.
#[derive(Debug, Clone)]
pub struct Phonebook {
    pub path: PathBuf,
    /// Sorted by name.
    pub entries: Vec<Entry>,
}

impl Phonebook {
    /// Load the phonebook at the default path.
    pub fn load() -> Result<Phonebook> {
        Self::load_from(&default_path()?)
    }

    /// Load from `path`; a missing file is an empty phonebook.
    pub fn load_from(path: &Path) -> Result<Phonebook> {
        let mut entries = match std::fs::read_to_string(path) {
            Ok(text) => parse_file(&text).with_context(|| format!("reading {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        sort(&mut entries);
        Ok(Phonebook {
            path: path.to_path_buf(),
            entries,
        })
    }

    pub fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = self.path.with_extension("conf.tmp");
        std::fs::write(&tmp, format_file(&self.entries))
            .and_then(|_| std::fs::rename(&tmp, &self.path))
            .with_context(|| format!("writing {}", self.path.display()))
    }

    /// The entry called `name`, ignoring case.
    pub fn get(&self, name: &str) -> Option<&Entry> {
        let name = name.trim();
        self.entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
    }

    /// The name saved for `group`, if any (the first, if there are several).
    pub fn name_of(&self, group: SocketAddr) -> Option<&Entry> {
        self.entries.iter().find(|e| e.group == group)
    }

    /// A group given as `ADDR:PORT` or as a phonebook name.
    pub fn resolve(&self, s: &str) -> Result<SocketAddr> {
        let s = s.trim();
        if let Ok(a) = s.parse() {
            return Ok(a);
        }
        if let Some(e) = self.get(s) {
            return Ok(e.group);
        }
        if self.entries.is_empty() {
            bail!(
                "\"{s}\" isn't a group and port (e.g. 239.69.0.1:5004), and the phonebook is empty"
            );
        }
        bail!("\"{s}\" isn't a group and port (e.g. 239.69.0.1:5004) or a name in the phonebook");
    }

    /// Add an entry, or replace the one with that name (ignoring case).
    /// Returns true if it was new.
    pub fn put(&mut self, entry: Entry) -> Result<bool> {
        check_name(&entry.name)?;
        check_description(&entry.description)?;
        let new = match self
            .entries
            .iter_mut()
            .find(|e| e.name.eq_ignore_ascii_case(&entry.name))
        {
            Some(e) => {
                *e = entry;
                false
            }
            None => {
                self.entries.push(entry);
                true
            }
        };
        sort(&mut self.entries);
        Ok(new)
    }

    /// Rename an entry. Fails if `to` is taken by another entry.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<&Entry> {
        check_name(to)?;
        let i = self.index(from)?;
        if let Some(j) = self
            .entries
            .iter()
            .position(|e| e.name.eq_ignore_ascii_case(to))
        {
            if j != i {
                bail!("\"{to}\" is already in the phonebook");
            }
        }
        self.entries[i].name = to.to_string();
        sort(&mut self.entries);
        Ok(self.get(to).expect("just renamed"))
    }

    pub fn remove(&mut self, name: &str) -> Result<Entry> {
        let i = self.index(name)?;
        Ok(self.entries.remove(i))
    }

    fn index(&self, name: &str) -> Result<usize> {
        self.entries
            .iter()
            .position(|e| e.name.eq_ignore_ascii_case(name.trim()))
            .with_context(|| format!("\"{}\" isn't in the phonebook", name.trim()))
    }
}

fn sort(entries: &mut [Entry]) {
    entries.sort_by_key(|e| e.name.to_lowercase());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, group: &str, description: &str) -> Entry {
        Entry {
            name: name.into(),
            group: group.parse().unwrap(),
            description: description.into(),
        }
    }

    fn book() -> Phonebook {
        Phonebook {
            path: PathBuf::new(),
            entries: Vec::new(),
        }
    }

    #[test]
    fn file_round_trip() {
        let mut b = book();
        b.put(entry("Zone 2", "239.192.2.2:10000", "Upstairs = loud"))
            .unwrap();
        b.put(entry("zone-1", "239.192.2.1:10000", "")).unwrap();
        b.put(entry("v6", "[ff15::69]:5004", "IPv6 test")).unwrap();
        let text = format_file(&b.entries);
        let mut parsed = parse_file(&text).unwrap();
        sort(&mut parsed);
        assert_eq!(parsed, b.entries);
        assert_eq!(
            b.entries
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            ["v6", "Zone 2", "zone-1"]
        );
    }

    #[test]
    fn parses_hand_written_files() {
        let text =
            "# comment\n\n[ lobby ]\n  group=239.69.0.1:5004\ndescription =  Lobby music  \n";
        assert_eq!(
            parse_file(text).unwrap(),
            [entry("lobby", "239.69.0.1:5004", "Lobby music")]
        );
        assert!(parse_file("[a]\ndescription = x\n").is_err(), "no group");
        assert!(parse_file("group = 239.1.1.1:5\n").is_err(), "no header");
        assert!(parse_file("[a]\ngroup = nope\n").is_err());
        assert!(parse_file("[a]\ngroup = 239.1.1.1:5\nport = 3\n").is_err());
        assert!(parse_file("[a]\ngroup = 239.1.1.1:5\n[A]\ngroup = 239.1.1.1:6\n").is_err());
    }

    #[test]
    fn resolves_addresses_and_names() {
        let mut b = book();
        b.put(entry("Lobby", "239.69.0.1:5004", "")).unwrap();
        assert_eq!(
            b.resolve("239.1.2.3:4000").unwrap(),
            "239.1.2.3:4000".parse().unwrap()
        );
        assert_eq!(
            b.resolve(" lobby ").unwrap(),
            "239.69.0.1:5004".parse().unwrap()
        );
        assert!(b.resolve("kitchen").is_err());
        assert_eq!(
            b.name_of("239.69.0.1:5004".parse().unwrap()).unwrap().name,
            "Lobby"
        );
    }

    #[test]
    fn put_replaces_rename_and_remove() {
        let mut b = book();
        assert!(b.put(entry("Lobby", "239.69.0.1:5004", "a")).unwrap());
        assert!(!b.put(entry("LOBBY", "239.69.0.2:5004", "b")).unwrap());
        assert_eq!(b.entries, [entry("LOBBY", "239.69.0.2:5004", "b")]);
        b.put(entry("Hall", "239.69.0.3:5004", "")).unwrap();
        assert!(b.rename("lobby", "hall").is_err());
        assert_eq!(b.rename("lobby", "Lobby").unwrap().name, "Lobby");
        assert_eq!(b.rename("hall", "Main hall").unwrap().name, "Main hall");
        assert_eq!(b.remove("main HALL").unwrap().name, "Main hall");
        assert!(b.remove("main hall").is_err());
        assert_eq!(b.entries.len(), 1);
    }

    #[test]
    fn rejects_bad_names_and_descriptions() {
        for bad in [
            "",
            " x",
            "x ",
            "-x",
            "a/b",
            "[x]",
            "a=b",
            "239.1.1.1:5004",
            "1.2.3.4:5",
        ] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        for good in ["lobby", "Zone 2", "page_1.a", "Café", "2nd floor"] {
            check_name(good).unwrap();
        }
        let mut b = book();
        assert!(b.put(entry("x", "239.1.1.1:5", "two\nlines")).is_err());
        assert!(b
            .put(entry("x", "239.1.1.1:5", &"d".repeat(MAX_DESCRIPTION + 1)))
            .is_err());
    }
}
