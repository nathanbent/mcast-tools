//! Access tokens: named keys to the panel, each with a [`Role`], kept in
//! `tokens.json` beside `intercom.json` (`docs/intercom-tokens.md`).
//!
//! Only each token's SHA-256 is stored: the secret is shown once, when the
//! token is made. The file is separate from the config so that a copy of the
//! config never carries them, and it's written with owner-only permissions.
//!
//! [`TokenStore`] is the running panel's view: it re-reads the file when it
//! changes (so `mcast intercom token ...` works while the panel runs), and
//! counts uses in memory, writing them out now and then rather than on every
//! request.

use crate::model::Config;
use crate::store::stamp;
use anyhow::{bail, Context, Result};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// What a token may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Everything, tokens included.
    Admin,
    /// Use the panel: play, stop, push to talk, history; change nothing.
    Operator,
    /// Other systems: play announcements (maybe only some) and stop them.
    Trigger,
    /// Sound files: list, upload, download, delete, folders.
    Media,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::Admin, Role::Operator, Role::Trigger, Role::Media];

    pub fn name(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Operator => "operator",
            Role::Trigger => "trigger",
            Role::Media => "media",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for Role {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Role::ALL
            .into_iter()
            .find(|r| r.name().eq_ignore_ascii_case(s.trim()))
            .with_context(|| format!("no role \"{s}\": use admin, operator, trigger or media"))
    }
}

/// One token, as stored. The secret itself isn't kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Token {
    pub id: String,
    pub name: String,
    pub role: Role,
    /// Trigger tokens: the announcement ids they may play. Empty: any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub announcements: Vec<String>,
    /// Operator tokens: the categories (ids) whose announcements they see
    /// and play. Empty: all of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    /// `sha256:` and the hex SHA-256 of the secret.
    pub hash: String,
    pub created_ms: u64,
    /// The last day it works, as given (`2027-01-31`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
    /// When it stops working: the end of that day in the panel's time zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_ms: Option<u64>,
    #[serde(default)]
    pub uses: u64,
}

impl Token {
    pub fn expired(&self, now_ms: u64) -> bool {
        self.expires_ms.is_some_and(|at| now_ms >= at)
    }

    /// Whether it may play (and stop) announcement `id`.
    pub fn may_play(&self, id: &str) -> bool {
        self.role != Role::Trigger
            || self.announcements.is_empty()
            || self.announcements.iter().any(|a| a == id)
    }
}

/// `tokens.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenFile {
    #[serde(default)]
    pub tokens: Vec<Token>,
}

/// What [`TokenFile::add`] needs.
#[derive(Debug, Clone)]
pub struct NewToken {
    pub name: String,
    pub role: Role,
    /// Announcement ids or names; trigger tokens only.
    pub announcements: Vec<String>,
    /// Category ids or names; operator tokens only.
    pub categories: Vec<String>,
    /// The last day it works (`2027-01-31`).
    pub expires: Option<String>,
}

impl TokenFile {
    /// A token by id, or by name (any case).
    pub fn find(&self, which: &str) -> Option<usize> {
        let which = which.trim();
        self.tokens.iter().position(|t| t.id == which).or_else(|| {
            self.tokens
                .iter()
                .position(|t| t.name.eq_ignore_ascii_case(which))
        })
    }

    fn check_name(&self, name: &str, except: Option<&str>) -> Result<String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
            bail!("a token's name is one line of 1 to 64 characters");
        }
        if self
            .tokens
            .iter()
            .any(|t| Some(t.id.as_str()) != except && t.name.eq_ignore_ascii_case(name))
        {
            bail!("there's already a token called \"{name}\"");
        }
        Ok(name.to_string())
    }

    /// Make a token. Returns it and its secret, which isn't kept anywhere.
    pub fn add(&mut self, new: NewToken, cfg: &Config, now_ms: u64) -> Result<(Token, String)> {
        let name = self.check_name(&new.name, None)?;
        let announcements = announcement_ids(new.role, &new.announcements, cfg)?;
        let categories = category_ids(new.role, &new.categories, cfg)?;
        let (expires, expires_ms) = expiry(new.expires.as_deref(), cfg, now_ms)?;
        let secret = new_secret()?;
        let id = loop {
            let id = format!("t_{}", hex(&random::<3>()?));
            if !self.tokens.iter().any(|t| t.id == id) {
                break id;
            }
        };
        let token = Token {
            id,
            name,
            role: new.role,
            announcements,
            categories,
            hash: hash(&secret),
            created_ms: now_ms,
            expires,
            expires_ms,
            last_used_ms: None,
            uses: 0,
        };
        self.tokens.push(token.clone());
        Ok((token, secret))
    }

    /// Rename a token, or change its announcements or expiry. `None` leaves
    /// that alone; `Some(None)` clears the expiry.
    #[allow(clippy::too_many_arguments)]
    pub fn change(
        &mut self,
        which: &str,
        name: Option<&str>,
        announcements: Option<&[String]>,
        categories: Option<&[String]>,
        expires: Option<Option<&str>>,
        cfg: &Config,
        now_ms: u64,
    ) -> Result<Token> {
        let i = self
            .find(which)
            .with_context(|| format!("no token \"{which}\""))?;
        let id = self.tokens[i].id.clone();
        let name = name.map(|n| self.check_name(n, Some(&id))).transpose()?;
        let role = self.tokens[i].role;
        let announcements = announcements
            .map(|a| announcement_ids(role, a, cfg))
            .transpose()?;
        let categories = categories.map(|c| category_ids(role, c, cfg)).transpose()?;
        let expires = expires.map(|e| expiry(e, cfg, now_ms)).transpose()?;
        let t = &mut self.tokens[i];
        if let Some(n) = name {
            t.name = n;
        }
        if let Some(a) = announcements {
            t.announcements = a;
        }
        if let Some(c) = categories {
            t.categories = c;
        }
        if let Some((e, ms)) = expires {
            t.expires = e;
            t.expires_ms = ms;
        }
        Ok(t.clone())
    }

    /// Revoke a token, by id or name.
    pub fn remove(&mut self, which: &str) -> Result<Token> {
        let i = self
            .find(which)
            .with_context(|| format!("no token \"{which}\""))?;
        Ok(self.tokens.remove(i))
    }

    /// The token this secret belongs to.
    pub fn lookup(&self, secret: &str) -> Option<&Token> {
        let h = hash(secret);
        self.tokens.iter().find(|t| t.hash == h)
    }

    /// Whether some admin token works now.
    pub fn has_admin(&self, now_ms: u64) -> bool {
        self.tokens
            .iter()
            .any(|t| t.role == Role::Admin && !t.expired(now_ms))
    }

    fn validate(&self) -> Result<()> {
        let mut ids = std::collections::HashSet::new();
        for t in &self.tokens {
            if !ids.insert(t.id.as_str()) {
                bail!("two tokens have the id \"{}\"", t.id);
            }
            if !t.hash.starts_with("sha256:") {
                bail!("token \"{}\" has no sha256 hash", t.name);
            }
        }
        Ok(())
    }
}

/// Resolve announcement ids or names; only trigger tokens have them.
fn announcement_ids(role: Role, given: &[String], cfg: &Config) -> Result<Vec<String>> {
    if given.is_empty() {
        return Ok(Vec::new());
    }
    if role != Role::Trigger {
        bail!("only trigger tokens are limited to announcements; a {role} token isn't");
    }
    let mut ids: Vec<String> = Vec::new();
    for which in given {
        let a = cfg
            .announcement(which.trim())
            .with_context(|| format!("no announcement \"{which}\""))?;
        if !ids.contains(&a.id) {
            ids.push(a.id.clone());
        }
    }
    Ok(ids)
}

/// Resolve category ids or names; only operator tokens have them.
fn category_ids(role: Role, given: &[String], cfg: &Config) -> Result<Vec<String>> {
    if given.is_empty() {
        return Ok(Vec::new());
    }
    if role != Role::Operator {
        bail!("only operator tokens are limited to categories; a {role} token isn't");
    }
    let mut ids: Vec<String> = Vec::new();
    for which in given {
        let w = which.trim();
        let c = cfg
            .categories
            .iter()
            .find(|c| c.id == w)
            .or_else(|| {
                cfg.categories
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(w))
            })
            .with_context(|| format!("no category \"{which}\""))?;
        if !ids.contains(&c.id) {
            ids.push(c.id.clone());
        }
    }
    Ok(ids)
}

/// `2027-01-31` → (the date, the end of that day in the panel's time zone).
fn expiry(date: Option<&str>, cfg: &Config, now_ms: u64) -> Result<(Option<String>, Option<u64>)> {
    let Some(date) = date.map(str::trim).filter(|d| !d.is_empty()) else {
        return Ok((None, None));
    };
    let day: jiff::civil::Date = date
        .parse()
        .map_err(|_| anyhow::anyhow!("\"{date}\" isn't a date like 2027-01-31"))?;
    let tz = cfg.tz()?;
    let end = day
        .tomorrow()
        .and_then(|d| d.to_zoned(tz))
        .map_err(|e| anyhow::anyhow!("\"{date}\": {e}"))?;
    let ms = end.timestamp().as_millisecond().max(0) as u64;
    if ms <= now_ms {
        bail!("{date} has already gone");
    }
    Ok((Some(day.to_string()), Some(ms)))
}

/// `N` random bytes from the system.
pub(crate) fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    SystemRandom::new()
        .fill(&mut b)
        .map_err(|_| anyhow::anyhow!("no random numbers from the system"))?;
    Ok(b)
}

/// `mci_` and 32 random bytes, base64url.
pub fn new_secret() -> Result<String> {
    Ok(format!("mci_{}", base64url(&random::<32>()?)))
}

/// What's stored for a secret.
pub fn hash(secret: &str) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, secret.as_bytes());
    format!("sha256:{}", hex(d.as_ref()))
}

/// Bytes as lowercase hex.
pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn base64url(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for chunk in b.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, x)| n | (*x as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// `tokens.json` beside the config file.
pub fn default_path(config: &Path) -> PathBuf {
    config.with_file_name("tokens.json")
}

/// Read the file. A missing file has no tokens.
pub fn load(path: &Path) -> Result<TokenFile> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(TokenFile::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let f: TokenFile =
        serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    f.validate()
        .with_context(|| format!("in {}", path.display()))?;
    Ok(f)
}

/// Write the file in one step, readable by its owner only.
pub fn save(path: &Path, f: &TokenFile) -> Result<()> {
    f.validate()?;
    crate::store::save_private(path, f)
}

/// What a secret turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    Valid(Token),
    /// A real token, past its expiry.
    Expired(Token),
    Unknown,
}

/// The running panel's tokens.
pub struct TokenStore {
    path: PathBuf,
    state: Mutex<StoreState>,
}

#[derive(Default)]
struct StoreState {
    file: TokenFile,
    /// The file as last read: modified time and length.
    stamp: Option<(SystemTime, u64)>,
    /// Uses not yet written: id → (last used, how many).
    usage: HashMap<String, (u64, u64)>,
}

impl TokenStore {
    /// Read the file; a broken one is an error now (later, the last good copy is kept).
    pub fn open(path: PathBuf) -> Result<Self> {
        let file = load(&path)?;
        let state = StoreState {
            file,
            stamp: stamp(&path),
            usage: HashMap::new(),
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StoreState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Re-read the file if it changed on disk.
    fn refresh(&self, st: &mut StoreState) {
        let now = stamp(&self.path);
        if now == st.stamp {
            return;
        }
        match load(&self.path) {
            Ok(f) => {
                st.file = f;
                st.stamp = now;
            }
            Err(e) => {
                // Keep the last good copy; say so once per change.
                st.stamp = now;
                eprintln!("mcast intercom: {e:#}; keeping the tokens read before");
            }
        }
    }

    /// Who a secret belongs to, counting the use.
    pub fn authenticate(&self, secret: &str, now_ms: u64) -> Lookup {
        let mut st = self.lock();
        self.refresh(&mut st);
        let Some(t) = st.file.lookup(secret).cloned() else {
            return Lookup::Unknown;
        };
        if t.expired(now_ms) {
            return Lookup::Expired(t);
        }
        let u = st.usage.entry(t.id.clone()).or_default();
        u.0 = u.0.max(now_ms);
        u.1 += 1;
        Lookup::Valid(t)
    }

    /// Every token, with uses so far.
    pub fn list(&self) -> Vec<Token> {
        let mut st = self.lock();
        self.refresh(&mut st);
        let mut f = st.file.clone();
        apply_usage(&mut f, &st.usage);
        f.tokens
    }

    pub fn has_admin(&self, now_ms: u64) -> bool {
        let mut st = self.lock();
        self.refresh(&mut st);
        st.file.has_admin(now_ms)
    }

    /// Change the file: read it afresh, add the uses counted so far, change
    /// it, write it. Blocks on the disk.
    pub fn modify<R>(&self, f: impl FnOnce(&mut TokenFile) -> Result<R>) -> Result<R> {
        let mut st = self.lock();
        let mut file = load(&self.path)?;
        apply_usage(&mut file, &st.usage);
        let r = f(&mut file)?;
        save(&self.path, &file)?;
        st.usage.clear();
        st.file = file;
        st.stamp = stamp(&self.path);
        Ok(r)
    }

    /// Write out the uses counted so far, if any. Blocks on the disk.
    pub fn flush(&self) -> Result<()> {
        if self.lock().usage.is_empty() {
            return Ok(());
        }
        self.modify(|_| Ok(()))
    }
}

fn apply_usage(f: &mut TokenFile, usage: &HashMap<String, (u64, u64)>) {
    for t in &mut f.tokens {
        if let Some((last, n)) = usage.get(&t.id) {
            t.last_used_ms = Some(t.last_used_ms.unwrap_or(0).max(*last));
            t.uses += n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        std::env::temp_dir().join(format!(
            "mcast-intercom-tokens-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    fn cfg() -> Config {
        serde_json::from_value(serde_json::json!({
            "time_zone": "Europe/London",
            "zones": [{"id": "hall", "name": "Hall", "targets": [{"group": "zone-1"}]}],
            "announcements": [{"id": "bell", "name": "Bell", "audio": "chime_only", "zones": ["hall"]}],
        }))
        .unwrap()
    }

    fn new(name: &str, role: Role) -> NewToken {
        NewToken {
            name: name.into(),
            role,
            announcements: vec![],
            categories: vec![],
            expires: None,
        }
    }

    #[test]
    fn secrets_look_right_and_only_their_hash_is_kept() {
        let s = new_secret().unwrap();
        assert!(s.starts_with("mci_") && s.len() == 47, "{s}");
        assert!(s[4..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(s, new_secret().unwrap());
        assert_eq!(base64url(b"\xfb\xff"), "-_8");
        assert_eq!(base64url(b"Man"), "TWFu");

        let mut f = TokenFile::default();
        let (t, secret) = f.add(new("Desk", Role::Operator), &cfg(), 1).unwrap();
        assert_eq!(f.lookup(&secret), Some(&t));
        assert_eq!(f.lookup("mci_nope"), None);
        let text = serde_json::to_string(&f).unwrap();
        assert!(!text.contains(&secret));
    }

    #[test]
    fn names_roles_announcements_and_expiry() {
        let c = cfg();
        let mut f = TokenFile::default();
        f.add(new("Desk", Role::Operator), &c, 1).unwrap();
        let e = f.add(new("desk", Role::Admin), &c, 1).unwrap_err();
        assert!(e.to_string().contains("already"), "{e}");
        assert!(f.add(new(" ", Role::Admin), &c, 1).is_err());

        let mut doorbell = new("Doorbell", Role::Trigger);
        doorbell.announcements = vec!["BELL".into()];
        let (t, _) = f.add(doorbell, &c, 1).unwrap();
        assert_eq!(t.announcements, ["bell"]);
        assert!(t.may_play("bell") && !t.may_play("other"));

        let mut wrong = new("Uploader", Role::Media);
        wrong.announcements = vec!["bell".into()];
        assert!(f.add(wrong, &c, 1).is_err());
        let mut missing = new("Other", Role::Trigger);
        missing.announcements = vec!["nope".into()];
        assert!(f.add(missing, &c, 1).is_err());

        // The end of the day in London: midnight UTC in winter.
        let mut temp = new("Contractor", Role::Admin);
        temp.expires = Some("2026-12-24".into());
        let (t, _) = f.add(temp, &c, 1).unwrap();
        let end = "2026-12-25T00:00:00Z".parse::<jiff::Timestamp>().unwrap();
        assert_eq!(t.expires_ms, Some(end.as_millisecond() as u64));
        assert!(!t.expired(end.as_millisecond() as u64 - 1));
        assert!(t.expired(end.as_millisecond() as u64));
        let mut gone = new("Late", Role::Admin);
        gone.expires = Some("2020-01-01".into());
        assert!(f.add(gone, &c, end.as_millisecond() as u64).is_err());

        let t = f
            .change("contractor", Some("Builder"), None, None, Some(None), &c, 1)
            .unwrap();
        assert_eq!((t.name.as_str(), t.expires_ms), ("Builder", None));
        assert!(f.remove("builder").is_ok());
        assert!(f.remove("builder").is_err());
    }

    #[test]
    fn the_store_counts_uses_and_sees_changes_on_disk() {
        let d = dir();
        let path = d.join("tokens.json");
        let store = TokenStore::open(path.clone()).unwrap();
        assert!(store.list().is_empty());
        let secret = store
            .modify(|f| Ok(f.add(new("Desk", Role::Operator), &cfg(), 1)?.1))
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(matches!(store.authenticate(&secret, 10), Lookup::Valid(_)));
        assert!(matches!(store.authenticate(&secret, 20), Lookup::Valid(_)));
        assert_eq!(store.authenticate("mci_x", 20), Lookup::Unknown);
        let listed = &store.list()[0];
        assert_eq!((listed.uses, listed.last_used_ms), (2, Some(20)));
        // Not written yet...
        assert_eq!(load(&path).unwrap().tokens[0].uses, 0);

        // Someone else (the CLI) adds one meanwhile; flushing keeps both.
        let mut f = load(&path).unwrap();
        let (_, other) = f.add(new("Doorbell", Role::Trigger), &cfg(), 1).unwrap();
        // Make sure the change is seen even within the same clock tick.
        f.tokens[1].name = "Doorbell, front".into();
        save(&path, &f).unwrap();
        assert!(matches!(store.authenticate(&other, 30), Lookup::Valid(_)));
        store.flush().unwrap();
        let on_disk = load(&path).unwrap();
        assert_eq!(on_disk.tokens.len(), 2);
        assert_eq!(on_disk.tokens[0].uses, 2);
        assert_eq!(on_disk.tokens[1].uses, 1);

        // Revoked on disk: refused at once.
        let mut f = load(&path).unwrap();
        f.remove("Desk").unwrap();
        save(&path, &f).unwrap();
        assert_eq!(store.authenticate(&secret, 40), Lookup::Unknown);
        let _ = std::fs::remove_dir_all(d);
    }
}
