//! Where the panel's config lives: a JSON file, `intercom.json` in the mcast
//! config directory, or `$MCAST_INTERCOM`.

use crate::model::Config;
use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

/// `$MCAST_INTERCOM`, else `intercom.json` in the mcast config directory
/// (`$XDG_CONFIG_HOME/mcast`, `~/.config/mcast`, or `%APPDATA%\mcast`).
pub fn default_path() -> Result<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(p) = var("MCAST_INTERCOM") {
        return Ok(p);
    }
    let dir = if cfg!(windows) {
        var("APPDATA")
    } else {
        var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|h| h.join(".config")))
    };
    let dir = dir.context("can't find a config directory; set MCAST_INTERCOM to a file path")?;
    Ok(dir.join("mcast").join("intercom.json"))
}

/// Read and check the config. A missing file is an empty config.
pub fn load(path: &Path) -> Result<Config> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let cfg: Config =
        serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    cfg.validate()
        .with_context(|| format!("in {}", path.display()))?;
    Ok(cfg)
}

/// Check and write the config, replacing the file in one step so a crash
/// can't leave half of it.
pub fn save(path: &Path, cfg: &Config) -> Result<()> {
    cfg.validate()?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        serde_json::to_writer_pretty(&mut f, cfg)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().with_context(|| format!("writing {}", path.display()))
}

/// Write a JSON file in one step, readable by its owner only (it holds
/// secrets, or their hashes).
pub fn save_private<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let write = || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&tmp)?;
        serde_json::to_writer_pretty(&mut f, value)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().with_context(|| format!("writing {}", path.display()))
}

/// The modified time and length of a file, to notice when it changes.
pub(crate) fn stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}
