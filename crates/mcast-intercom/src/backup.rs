//! Backups of the panel's config, alone (`intercom.json` as it is) or with
//! the sounds it can use, as a `.tar.gz`:
//!
//! ```text
//! mcast-intercom-backup.json   {"format": 1, "created_ms", "version", "sounds"}
//! intercom.json
//! media/bells/lunch.wav        each sound, by its path on the mcast server
//! ```
//!
//! Never in a backup: `tokens.json` and `webhooks.json` (they hold secrets)
//! and the play history. Restoring replaces the whole config; [`plan`] says
//! first what would change.

use crate::backend::{Backend, BackendError, MediaFile};
use crate::model::{valid_media_name, Audio, Config, PlaylistItem};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};

/// The archive's description of itself.
pub const MANIFEST: &str = "mcast-intercom-backup.json";
const CONFIG: &str = "intercom.json";
const MEDIA: &str = "media/";
/// The archive format this writes and reads.
const FORMAT: u64 = 1;
/// Most a backup may hold, unpacked, and its most files.
pub const MAX_BYTES: u64 = 1 << 30;
const MAX_FILES: usize = 10_000;
/// The largest config file read.
const MAX_CONFIG: u64 = 16 << 20;

/// What a backup holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Backup {
    pub config: Config,
    /// Sounds, by path, with their bytes (none for a config-only backup).
    pub sounds: BTreeMap<String, Vec<u8>>,
    /// When it was made, if it says.
    pub created_ms: Option<u64>,
}

/// Every sound file a config uses: announcements' audio and chimes,
/// the defaults' chimes, and playlists.
pub fn media_used(cfg: &Config) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let chime = |c: &Option<Value>, out: &mut BTreeSet<String>| {
        if let Some(f) = c.as_ref().and_then(|c| c["file"].as_str()) {
            out.insert(f.to_string());
        }
    };
    chime(&cfg.defaults.chime, &mut out);
    chime(&cfg.defaults.end_chime, &mut out);
    for a in &cfg.announcements {
        for (audio, _) in a.sections() {
            if let Audio::Media(m) = audio {
                out.insert(m.clone());
            }
        }
        chime(&a.chime, &mut out);
        chime(&a.end_chime, &mut out);
    }
    for p in &cfg.playlists {
        out.extend(
            p.items
                .iter()
                .filter_map(PlaylistItem::media)
                .map(String::from),
        );
    }
    out
}

/// Write a `.tar.gz` backup of `cfg` with `sounds` (path, bytes).
pub fn write_archive<'a>(
    cfg: &Config,
    sounds: impl IntoIterator<Item = (&'a str, &'a [u8])>,
    created_ms: u64,
    out: impl Write,
) -> Result<()> {
    let gz = flate2::write::GzEncoder::new(out, flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let add = |tar: &mut tar::Builder<_>, name: &str, data: &[u8]| -> Result<()> {
        let mut h = tar::Header::new_ustar();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(created_ms / 1000);
        h.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut h, name, data)
            .with_context(|| format!("adding {name} to the backup"))
    };
    let sounds: Vec<(&str, &[u8])> = sounds.into_iter().collect();
    let manifest = json!({
        "format": FORMAT,
        "created_ms": created_ms,
        "version": env!("CARGO_PKG_VERSION"),
        "sounds": sounds.len(),
    });
    add(&mut tar, MANIFEST, &serde_json::to_vec_pretty(&manifest)?)?;
    add(&mut tar, CONFIG, &serde_json::to_vec_pretty(cfg)?)?;
    for (name, data) in sounds {
        if !valid_media_name(name) {
            bail!("can't back up a sound called \"{name}\"");
        }
        add(&mut tar, &format!("{MEDIA}{name}"), data)?;
    }
    tar.into_inner()?.finish()?.flush()?;
    Ok(())
}

/// Read a backup: a `.tar.gz` made by [`write_archive`], or a config file
/// on its own. The config is checked as `intercom.json` would be.
pub fn read(bytes: &[u8]) -> Result<Backup> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return read_archive(bytes);
    }
    let config = parse_config(bytes)?;
    Ok(Backup {
        config,
        sounds: BTreeMap::new(),
        created_ms: None,
    })
}

fn parse_config(bytes: &[u8]) -> Result<Config> {
    let cfg: Config = serde_json::from_slice(bytes)
        .context("this isn't an intercom backup: not a .tar.gz, and not an intercom.json")?;
    cfg.validate()
        .context("the backup's config has a problem")?;
    Ok(cfg)
}

fn read_archive(bytes: &[u8]) -> Result<Backup> {
    // A small archive can unpack to a lot: count what comes out.
    let gz = flate2::read::GzDecoder::new(bytes).take(MAX_BYTES + 1);
    let mut tar = tar::Archive::new(gz);
    let (mut manifest, mut config) = (None, None);
    let mut sounds = BTreeMap::new();
    let mut total: u64 = 0;
    for (n, entry) in tar.entries().context("reading the backup")?.enumerate() {
        if n >= MAX_FILES {
            bail!("the backup has more than {MAX_FILES} files");
        }
        let mut entry = entry.context("reading the backup")?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        let name = entry
            .path()
            .context("reading the backup")?
            .to_str()
            .map(|s| s.replace('\\', "/"))
            .context("a file in the backup has a name that isn't text")?;
        if !kind.is_file() {
            bail!("the backup holds \"{name}\", which isn't a plain file");
        }
        let size = entry.header().size()?;
        total += size;
        if total > MAX_BYTES {
            bail!("the backup holds more than {} MiB", MAX_BYTES >> 20);
        }
        let mut data = Vec::with_capacity(size.min(MAX_BYTES) as usize);
        entry
            .read_to_end(&mut data)
            .with_context(|| format!("reading {name} from the backup"))?;
        match name.as_str() {
            MANIFEST => manifest = Some(data),
            CONFIG => {
                if size > MAX_CONFIG {
                    bail!("the backup's intercom.json is too big");
                }
                config = Some(data);
            }
            _ => {
                let Some(sound) = name.strip_prefix(MEDIA).filter(|s| valid_media_name(s)) else {
                    bail!("the backup holds \"{name}\", which an intercom backup doesn't");
                };
                sounds.insert(sound.to_string(), data);
            }
        }
    }
    let manifest: Value = manifest
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()
        .context("reading the backup's description")?
        .context("this .tar.gz isn't an intercom backup (it has no mcast-intercom-backup.json)")?;
    let format = manifest["format"].as_u64().unwrap_or(0);
    if format != FORMAT {
        bail!("this backup is format {format}; this panel reads format {FORMAT}");
    }
    let config = parse_config(&config.context("the backup has no intercom.json")?)?;
    Ok(Backup {
        config,
        sounds,
        created_ms: manifest["created_ms"].as_u64(),
    })
}

/// The lists in a config whose items have ids, with what to call them.
const LISTS: [(&str, &str); 7] = [
    ("zones", "zone"),
    ("announcements", "announcement"),
    ("schedules", "schedule"),
    ("playlists", "playlist"),
    ("categories", "category"),
    ("sites", "site"),
    ("schedule_groups", "schedule group"),
];

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ListChanges {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
}

impl ListChanges {
    fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }
}

/// What restoring a backup would do.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Plan {
    /// For each list that changes (`zones`, `announcements`, ...): the
    /// names of what's added, changed and removed.
    pub lists: BTreeMap<String, ListChanges>,
    /// Other settings that change: `time_zone`, `defaults`, `days_off`,
    /// `panel`, ...
    pub settings: Vec<String>,
    /// Sounds in the backup.
    pub sounds_in_backup: usize,
    /// Sounds to upload: not on the mcast server, or a different size there.
    pub upload: Vec<String>,
    /// Sounds the restored config uses that are neither on the mcast server
    /// nor in the backup: those would fail to play.
    pub missing: Vec<String>,
    /// Nothing would change.
    pub same: bool,
}

/// Compare the config now with a backup's, and the sounds on the server
/// (`on_server`; `None` if it couldn't be asked) with the backup's.
pub fn plan(now: &Config, backup: &Backup, on_server: Option<&[MediaFile]>) -> Plan {
    let (old, new) = (
        serde_json::to_value(now).unwrap_or_default(),
        serde_json::to_value(&backup.config).unwrap_or_default(),
    );
    let mut lists = BTreeMap::new();
    for (key, _) in LISTS {
        let items = |v: &Value| -> Vec<Value> { v[key].as_array().cloned().unwrap_or_default() };
        let (before, after) = (items(&old), items(&new));
        let by_id = |list: &[Value], id: &Value| list.iter().find(|i| i["id"] == *id).cloned();
        let label = |i: &Value| {
            i["name"]
                .as_str()
                .or(i["id"].as_str())
                .unwrap_or("?")
                .to_string()
        };
        let mut c = ListChanges::default();
        for a in &after {
            match by_id(&before, &a["id"]) {
                None => c.added.push(label(a)),
                Some(b) if b != *a => c.changed.push(label(a)),
                Some(_) => {}
            }
        }
        for b in &before {
            if by_id(&after, &b["id"]).is_none() {
                c.removed.push(label(b));
            }
        }
        if !c.is_empty() {
            lists.insert(key.to_string(), c);
        }
    }
    // Everything else, key by key (a key left out is its default).
    let mut keys: BTreeSet<&str> = BTreeSet::new();
    for v in [&old, &new] {
        keys.extend(
            v.as_object()
                .into_iter()
                .flat_map(|o| o.keys().map(String::as_str)),
        );
    }
    let settings: Vec<String> = keys
        .into_iter()
        .filter(|k| !LISTS.iter().any(|(l, _)| l == k))
        .filter(|k| old.get(*k) != new.get(*k))
        .map(String::from)
        .collect();

    let size_on_server = |name: &str| {
        on_server.and_then(|files| files.iter().find(|f| f.name == name).map(|f| f.bytes))
    };
    let upload: Vec<String> = backup
        .sounds
        .iter()
        .filter(|(name, data)| size_on_server(name) != Some(data.len() as u64))
        .map(|(name, _)| name.clone())
        .collect();
    let missing = match on_server {
        // Without the server's list, only the backup can be asked.
        None => Vec::new(),
        Some(_) => media_used(&backup.config)
            .into_iter()
            .filter(|m| !backup.sounds.contains_key(m) && size_on_server(m).is_none())
            .collect(),
    };
    let same = lists.is_empty() && settings.is_empty() && upload.is_empty();
    Plan {
        lists,
        settings,
        sounds_in_backup: backup.sounds.len(),
        upload,
        missing,
        same,
    }
}

impl Plan {
    /// For people, a line each: "zones: added Yard; changed Hall".
    pub fn describe(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (list, c) in &self.lists {
            let mut parts = Vec::new();
            for (what, names) in [
                ("added", &c.added),
                ("changed", &c.changed),
                ("removed", &c.removed),
            ] {
                if !names.is_empty() {
                    parts.push(format!("{what} {}", names.join(", ")));
                }
            }
            out.push(format!("{}: {}", list.replace('_', " "), parts.join("; ")));
        }
        if !self.settings.is_empty() {
            out.push(format!(
                "settings: {}",
                self.settings.join(", ").replace('_', " ")
            ));
        }
        if self.sounds_in_backup > 0 {
            out.push(format!(
                "sounds: {} in the backup, {} to upload{}",
                self.sounds_in_backup,
                self.upload.len(),
                if self.upload.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", self.upload.join(", "))
                }
            ));
        }
        if !self.missing.is_empty() {
            out.push(format!(
                "missing: {} (used, but neither on the mcast server nor in the backup)",
                self.missing.join(", ")
            ));
        }
        if self.same {
            out.push("nothing would change".into());
        }
        out
    }
}

/// What [`apply`] did.
#[derive(Debug, Clone, Default)]
pub struct Applied {
    pub uploaded: Vec<String>,
    /// Where the config it replaced was kept (`intercom.json.bak`).
    pub kept: Option<std::path::PathBuf>,
}

#[derive(Debug)]
pub enum ApplyError {
    /// A sound couldn't be uploaded; the config wasn't changed.
    Upload {
        name: String,
        error: BackendError,
        /// Sounds uploaded before it.
        done: Vec<String>,
    },
    /// Keeping the old config or writing the new one.
    Write(anyhow::Error),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Upload { name, error, done } => {
                write!(f, "uploading {name}: {error}; the config wasn't changed")?;
                if !done.is_empty() {
                    write!(f, " (uploaded before it: {})", done.join(", "))?;
                }
                Ok(())
            }
            ApplyError::Write(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for ApplyError {}

/// Restore `b` as [`plan`] said: upload the sounds in `plan.upload`, then
/// keep the config at `path` as `PATH.bak` and write the backup's there.
/// The caller holds whatever lock edits take.
pub fn apply(
    path: &std::path::Path,
    b: &Backup,
    plan: &Plan,
    backend: &dyn Backend,
) -> Result<Applied, ApplyError> {
    let mut uploaded = Vec::new();
    for name in &plan.upload {
        let Some(data) = b.sounds.get(name) else {
            continue;
        };
        if let Err(error) = backend.upload_media(name, data) {
            return Err(ApplyError::Upload {
                name: name.clone(),
                error,
                done: uploaded,
            });
        }
        uploaded.push(name.clone());
    }
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    let bak = std::path::PathBuf::from(bak);
    let kept = if path.exists() {
        std::fs::copy(path, &bak)
            .with_context(|| format!("keeping {}", bak.display()))
            .map_err(ApplyError::Write)?;
        Some(bak)
    } else {
        None
    };
    crate::store::save(path, &b.config).map_err(ApplyError::Write)?;
    Ok(Applied { uploaded, kept })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Announcement, Target, Zone};

    fn cfg() -> Config {
        let mut c = Config::default();
        c.zones.push(Zone {
            id: "hall".into(),
            name: "Hall".into(),
            targets: vec![Target {
                group: "239.1.1.1:5000".into(),
                ..Default::default()
            }],
            site: None,
            listeners: Vec::new(),
        });
        let a: Announcement = serde_json::from_value(json!({
            "id": "lunch", "name": "Lunch", "audio": {"media": "bells/lunch.wav"},
            "chime": {"file": "chimes/up.wav"}, "zones": ["hall"]
        }))
        .unwrap();
        c.announcements.push(a);
        c.time_zone = Some("Europe/London".into());
        c
    }

    fn archive(c: &Config, sounds: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        write_archive(c, sounds.iter().copied(), 1_800_000_000_000, &mut out).unwrap();
        out
    }

    fn file(name: &str, bytes: u64) -> MediaFile {
        serde_json::from_value(json!({"name": name, "bytes": bytes})).unwrap()
    }

    #[test]
    fn an_archive_round_trips() {
        let c = cfg();
        let bytes = archive(
            &c,
            &[("bells/lunch.wav", b"RIFF1234"), ("chimes/up.wav", b"RIFF")],
        );
        assert!(bytes.starts_with(&[0x1f, 0x8b]));
        let b = read(&bytes).unwrap();
        assert_eq!(b.config, c);
        assert_eq!(b.created_ms, Some(1_800_000_000_000));
        assert_eq!(b.sounds.len(), 2);
        assert_eq!(b.sounds["bells/lunch.wav"], b"RIFF1234");
        assert!(write_archive(&c, [("../x.wav", &b""[..])], 0, Vec::new()).is_err());
    }

    #[test]
    fn a_config_on_its_own_is_a_backup() {
        let c = cfg();
        let b = read(&serde_json::to_vec(&c).unwrap()).unwrap();
        assert_eq!(b.config, c);
        assert!(b.sounds.is_empty() && b.created_ms.is_none());
        let err = read(b"hello").unwrap_err().to_string();
        assert!(err.contains("isn't an intercom backup"), "{err}");
        // Checked like the file itself.
        let mut bad = serde_json::to_value(&c).unwrap();
        bad["announcements"][0]["zones"] = json!(["nowhere"]);
        assert!(read(&serde_json::to_vec(&bad).unwrap()).is_err());
    }

    /// A .tar.gz with these (name, bytes) entries, as anything might make it.
    fn raw_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut t = tar::Builder::new(gz);
        for (name, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            // Unchecked, so a path like ../ can be written.
            h.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
            h.set_cksum();
            t.append(&h, *data).unwrap();
        }
        t.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn refuses_archives_that_arent_backups_or_reach_outside() {
        let manifest = br#"{"format": 1}"#;
        let c = serde_json::to_vec(&cfg()).unwrap();
        let ok = raw_tar(&[(MANIFEST, manifest), (CONFIG, &c), ("media/a.wav", b"x")]);
        assert_eq!(read(&ok).unwrap().sounds.len(), 1);
        for (entries, says) in [
            (vec![(CONFIG, &c[..])], "no mcast-intercom-backup.json"),
            (vec![(MANIFEST, &manifest[..])], "no intercom.json"),
            (
                vec![(MANIFEST, &br#"{"format": 2}"#[..]), (CONFIG, &c[..])],
                "format 2",
            ),
            (
                vec![
                    (MANIFEST, &manifest[..]),
                    (CONFIG, &c[..]),
                    ("media/../../etc/x", b"x"),
                ],
                "doesn't",
            ),
            (
                vec![
                    (MANIFEST, &manifest[..]),
                    (CONFIG, &c[..]),
                    ("tokens.json", b"{}"),
                ],
                "doesn't",
            ),
            (
                vec![
                    (MANIFEST, &manifest[..]),
                    (CONFIG, &c[..]),
                    ("media/.hidden", b"x"),
                ],
                "doesn't",
            ),
        ] {
            let err = format!("{:#}", read(&raw_tar(&entries)).unwrap_err());
            assert!(err.contains(says), "{says}: {err}");
        }
    }

    #[test]
    fn lists_the_sounds_a_config_uses() {
        let mut c = cfg();
        c.playlists = serde_json::from_value(json!([
            {"id": "p", "name": "P", "items": [{"type": "sound", "media": "a.wav"}]}
        ]))
        .unwrap();
        c.defaults.end_chime = Some(json!({"file": "end.wav"}));
        let used: Vec<String> = media_used(&c).into_iter().collect();
        assert_eq!(
            used,
            ["a.wav", "bells/lunch.wav", "chimes/up.wav", "end.wav"]
        );
    }

    #[test]
    fn plans_what_a_restore_changes() {
        let now = cfg();
        let mut then = cfg();
        then.zones[0].name = "Main hall".into();
        then.zones.push(Zone {
            id: "yard".into(),
            name: "Yard".into(),
            targets: vec![Target {
                group: "239.1.1.2:5000".into(),
                ..Default::default()
            }],
            site: None,
            listeners: Vec::new(),
        });
        then.announcements.clear();
        then.time_zone = None;
        then.schedules_paused = true;
        let backup = Backup {
            config: then,
            sounds: BTreeMap::from([
                ("same.wav".into(), vec![0; 4]),
                ("new.wav".into(), vec![0; 2]),
            ]),
            created_ms: None,
        };
        let server = [file("same.wav", 4), file("bells/lunch.wav", 10)];
        let p = plan(&now, &backup, Some(&server));
        assert_eq!(p.lists["zones"].added, ["Yard"]);
        assert_eq!(p.lists["zones"].changed, ["Main hall"]);
        assert_eq!(p.lists["announcements"].removed, ["Lunch"]);
        assert!(!p.lists.contains_key("schedules"));
        assert_eq!(p.settings, ["schedules_paused", "time_zone"]);
        assert_eq!(p.upload, ["new.wav"]);
        assert_eq!(p.sounds_in_backup, 2);
        assert!(!p.same);

        // The same config: nothing to do, but sounds it uses are missing.
        let backup = Backup {
            config: cfg(),
            sounds: BTreeMap::new(),
            created_ms: None,
        };
        let p = plan(&now, &backup, Some(&server));
        assert!(p.same, "{p:?}");
        assert_eq!(p.missing, ["chimes/up.wav"]);
        // Without the server, missing sounds can't be known.
        assert!(plan(&now, &backup, None).missing.is_empty());
    }
}
