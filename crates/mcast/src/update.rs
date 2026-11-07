//! `mcast update`: rebuild and reinstall this binary with cargo, either from
//! the GitHub repository or from a local clone.

use anyhow::{bail, Context, Result};
use clap::Args;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEFAULT_REPO: &str = "https://github.com/nathanbent/mcast-tools";

/// Commit this binary was built from, when it was built from a git checkout.
pub const BUILD_SHA: Option<&str> = option_env!("MCAST_GIT_SHA");

#[derive(Args)]
pub struct UpdateArgs {
    /// Only report whether a newer commit is available (or whether this build
    /// is ahead of the branch); don't install
    #[arg(long)]
    check: bool,
    /// Branch to install from
    #[arg(short, long, default_value = "main")]
    branch: String,
    /// Git repository to install from
    #[arg(long, default_value = DEFAULT_REPO)]
    repo: String,
    /// Update from a local clone instead: `git pull` it, then install from it
    #[arg(long, value_name = "DIR", conflicts_with = "repo")]
    path: Option<PathBuf>,
    /// Install even if this build is already the latest, or ahead of the branch
    #[arg(short, long)]
    force: bool,
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

/// Where the installed commit stands relative to the branch head.
#[derive(Debug, PartialEq, Eq)]
enum Relation {
    Same,
    /// Commits the installed build has that the branch doesn't, and vice versa.
    Counts {
        ahead: u32,
        behind: u32,
    },
    /// Couldn't tell (unknown build commit, commit not pushed, no network...).
    Unknown,
}

pub fn run(a: UpdateArgs) -> Result<()> {
    let latest = match &a.path {
        Some(dir) => local_head(dir, &a.branch, a.check)?,
        None => remote_head(&a.repo, &a.branch),
    };

    println!(
        "Installed: {}",
        BUILD_SHA.map_or("unknown commit".into(), |s| short(s).to_string())
    );
    match &latest {
        Some(sha) => println!("Latest:    {} ({})", short(sha), a.branch),
        None => println!("Latest:    unknown (couldn't ask git)"),
    }

    let relation = match (BUILD_SHA, &latest) {
        (Some(b), Some(l)) if b == l => Relation::Same,
        (Some(b), Some(l)) => match &a.path {
            Some(dir) => compare_local(dir, b, l),
            None => compare_github(&a.repo, b, l),
        },
        _ => Relation::Unknown,
    };
    let branch = &a.branch;
    // Whether installing would be an update rather than a downgrade.
    let wanted = match relation {
        Relation::Same => {
            println!("Up to date.");
            false
        }
        Relation::Counts { ahead: 0, behind } => {
            println!("{behind} new commit{} on {branch}.", plural(behind));
            true
        }
        Relation::Counts { ahead, behind: 0 } => {
            println!(
                "This build is {ahead} commit{} ahead of {branch} (a development build); \
                 there's nothing newer to install.",
                plural(ahead)
            );
            false
        }
        Relation::Counts { ahead, behind } => {
            let dropped = if ahead == 1 {
                "that commit".to_string()
            } else {
                format!("those {ahead}")
            };
            println!(
                "This build has {ahead} commit{} that {branch} doesn't, and {branch} has \
                 {behind} new one{}. Updating would replace it with {branch}, dropping {dropped}.",
                plural(ahead),
                plural(behind)
            );
            false
        }
        Relation::Unknown if BUILD_SHA.is_some() && latest.is_some() => {
            println!(
                "The installed commit differs from {branch}; couldn't tell whether it's older or newer."
            );
            true
        }
        Relation::Unknown => {
            println!("Can't compare commits.");
            true
        }
    };

    if a.check {
        match (wanted, &relation) {
            (true, Relation::Counts { .. }) => println!("Run `mcast update` to install them."),
            (true, _) => println!("Run `mcast update` to rebuild from {branch} anyway."),
            (false, Relation::Same) => {}
            (false, _) => println!("`mcast update --force` would install {branch} anyway."),
        }
        return Ok(());
    }
    if !wanted && !a.force {
        match relation {
            Relation::Same => println!("Use --force to rebuild anyway."),
            _ => println!("Not updating; use --force to install {branch} anyway."),
        }
        return Ok(());
    }

    let cargo = find_cargo()?;
    let mut cmd = Command::new(&cargo);
    cmd.args(["install", "--locked"]);
    match &a.path {
        Some(dir) => {
            cmd.arg("--path").arg(dir.join("crates/mcast"));
        }
        None => {
            cmd.args(["--git", &a.repo, "--branch", &a.branch, "mcast"]);
        }
    }
    // We've already decided an update is wanted. --force also lets a git
    // install replace one made with `cargo install --path` (the README's
    // instructions), which cargo otherwise refuses.
    cmd.arg("--force");

    let in_cargo_bin = check_location();
    println!("Running: {}", describe(&cmd));
    let guard = MoveAside::new(in_cargo_bin)?;
    let status = cmd
        .status()
        .with_context(|| format!("couldn't run {}", cargo.display()))?;
    if !status.success() {
        guard.restore();
        bail!("cargo install failed ({status})");
    }
    println!("Updated. Check with `mcast --version`.");
    Ok(())
}

/// `git ls-remote` the branch; None if git is missing or the network fails.
fn remote_head(repo: &str, branch: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["ls-remote", repo, &format!("refs/heads/{branch}")])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()?
        .split_whitespace()
        .next()
        .map(str::to_string)
}

fn plural(n: u32) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Ask a local clone how `installed` and `latest` relate. Unknown when the
/// clone doesn't have the installed commit (built from elsewhere).
fn compare_local(dir: &Path, installed: &str, latest: &str) -> Relation {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-list", "--left-right", "--count"])
        .arg(format!("{installed}...{latest}"))
        .output();
    match out {
        Ok(o) if o.status.success() => parse_left_right(&String::from_utf8_lossy(&o.stdout)),
        _ => Relation::Unknown,
    }
}

/// `git rev-list --left-right --count A...B` prints "<only in A>\t<only in B>".
fn parse_left_right(s: &str) -> Relation {
    let mut it = s.split_whitespace().map(str::parse::<u32>);
    match (it.next(), it.next()) {
        (Some(Ok(ahead)), Some(Ok(behind))) => counts(ahead, behind),
        _ => Relation::Unknown,
    }
}

fn counts(ahead: u32, behind: u32) -> Relation {
    if ahead == 0 && behind == 0 {
        Relation::Same
    } else {
        Relation::Counts { ahead, behind }
    }
}

/// Use GitHub's compare API (through curl) for a github.com repository.
/// Unknown for other hosts, without curl, offline, when rate-limited, or when
/// the installed commit was never pushed.
fn compare_github(repo: &str, installed: &str, latest: &str) -> Relation {
    let Some(path) = github_path(repo) else {
        return Relation::Unknown;
    };
    let url = format!("https://api.github.com/repos/{path}/compare/{latest}...{installed}");
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "15"])
        .args(["-H", "Accept: application/vnd.github+json"])
        .args(["-H", "User-Agent: mcast-update"])
        .arg(&url)
        .output();
    match out {
        Ok(o) if o.status.success() => parse_compare(&String::from_utf8_lossy(&o.stdout)),
        _ => Relation::Unknown,
    }
}

/// "owner/name" from a github.com URL (https, ssh or scp-style).
fn github_path(repo: &str) -> Option<String> {
    let rest = repo
        .strip_prefix("https://github.com/")
        .or_else(|| repo.strip_prefix("http://github.com/"))
        .or_else(|| repo.strip_prefix("ssh://git@github.com/"))
        .or_else(|| repo.strip_prefix("git@github.com:"))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(o), Some(n), None) if !o.is_empty() && !n.is_empty() => Some(format!("{o}/{n}")),
        _ => None,
    }
}

/// Compare response for base=latest, head=installed: `ahead_by` counts
/// commits only the installed build has, `behind_by` those only the branch has.
fn parse_compare(json: &str) -> Relation {
    match (json_u32(json, "ahead_by"), json_u32(json, "behind_by")) {
        (Some(ahead), Some(behind)) => counts(ahead, behind),
        _ => Relation::Unknown,
    }
}

/// The first `"key": <number>` in a JSON text (keys used here are unique).
fn json_u32(json: &str, key: &str) -> Option<u32> {
    let pat = format!("\"{key}\"");
    let rest = &json[json.find(&pat)? + pat.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Pull the local clone (fetch only for --check) and return the commit that
/// would be installed.
fn local_head(dir: &Path, branch: &str, check_only: bool) -> Result<Option<String>> {
    if !dir.join("crates/mcast/Cargo.toml").exists() {
        bail!("{} doesn't look like an mcast-tools clone", dir.display());
    }
    let git = |args: &[&str]| -> Result<std::process::Output> {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .context("couldn't run git")
    };
    if check_only {
        let out = git(&["fetch", "--quiet", "origin", branch])?;
        if !out.status.success() {
            bail!(
                "git fetch failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let out = git(&["rev-parse", "FETCH_HEAD"])?;
        return Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ));
    }
    let current = git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
    let current = String::from_utf8_lossy(&current.stdout).trim().to_string();
    if current != branch {
        bail!(
            "{} is on branch '{current}', not '{branch}'; switch it or pass --branch {current}",
            dir.display()
        );
    }
    println!("Pulling {} ...", dir.display());
    let out = git(&["pull", "--ff-only", "origin", branch])?;
    if !out.status.success() {
        bail!(
            "git pull failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let out = git(&["rev-parse", "HEAD"])?;
    Ok(Some(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

fn cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".cargo")))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// `cargo` from PATH, else from cargo's home (rustup puts it there even
/// when the shell's PATH doesn't include it).
fn find_cargo() -> Result<PathBuf> {
    let exe = format!("cargo{}", std::env::consts::EXE_SUFFIX);
    if Command::new(&exe).arg("--version").output().is_ok() {
        return Ok(PathBuf::from(exe));
    }
    if let Some(p) = cargo_home().map(|h| h.join("bin").join(&exe)) {
        if p.exists() {
            return Ok(p);
        }
    }
    bail!("cargo not found; install Rust from https://rustup.rs to update mcast")
}

/// cargo installs into $CARGO_HOME/bin; say so if this copy lives elsewhere.
/// Returns whether it lives there.
fn check_location() -> bool {
    let (Ok(exe), Some(home)) = (std::env::current_exe(), cargo_home()) else {
        return false;
    };
    let bin = home.join("bin");
    let same = exe
        .parent()
        .and_then(|d| d.canonicalize().ok())
        .zip(bin.canonicalize().ok())
        .is_some_and(|(a, b)| a == b);
    if !same {
        eprintln!(
            "Note: this mcast is {}; the update installs to {}.",
            exe.display(),
            bin.display()
        );
    }
    same
}

fn describe(cmd: &Command) -> String {
    let mut s = cmd.get_program().to_string_lossy().into_owned();
    for a in cmd.get_args() {
        s.push(' ');
        s.push_str(&a.to_string_lossy());
    }
    s
}

/// Windows can't overwrite a running .exe, but it can rename it. Move it
/// aside so cargo can write the new one, and put it back if cargo fails.
/// Elsewhere the old file is simply replaced while it runs.
struct MoveAside {
    #[cfg_attr(not(windows), allow(dead_code))]
    moved: Option<(PathBuf, PathBuf)>,
}

impl MoveAside {
    #[cfg(windows)]
    fn new(in_cargo_bin: bool) -> Result<Self> {
        if !in_cargo_bin {
            return Ok(Self { moved: None });
        }
        let exe = std::env::current_exe()?;
        let old = exe.with_extension("exe.old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&exe, &old)
            .with_context(|| format!("couldn't move {} aside", exe.display()))?;
        Ok(Self {
            moved: Some((exe, old)),
        })
    }

    #[cfg(not(windows))]
    fn new(_in_cargo_bin: bool) -> Result<Self> {
        Ok(Self { moved: None })
    }

    fn restore(self) {
        if let Some((exe, old)) = &self.moved {
            if !exe.exists() {
                let _ = std::fs::rename(old, exe);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_right_counts() {
        assert_eq!(parse_left_right("0\t0\n"), Relation::Same);
        assert_eq!(
            parse_left_right("0\t3\n"),
            Relation::Counts {
                ahead: 0,
                behind: 3
            }
        );
        assert_eq!(
            parse_left_right("2\t5"),
            Relation::Counts {
                ahead: 2,
                behind: 5
            }
        );
        assert_eq!(parse_left_right(""), Relation::Unknown);
    }

    #[test]
    fn compare_json() {
        let j = r#"{"url": "x", "status": "ahead", "ahead_by": 2, "behind_by": 0,
                    "files": [{"status": "modified"}]}"#;
        assert_eq!(
            parse_compare(j),
            Relation::Counts {
                ahead: 2,
                behind: 0
            }
        );
        assert_eq!(
            parse_compare(r#"{"ahead_by":0,"behind_by":0}"#),
            Relation::Same
        );
        assert_eq!(
            parse_compare(r#"{"message":"Not Found"}"#),
            Relation::Unknown
        );
    }

    #[test]
    fn github_paths() {
        let want = Some("nathanbent/mcast-tools".to_string());
        assert_eq!(github_path(DEFAULT_REPO), want);
        assert_eq!(
            github_path("https://github.com/nathanbent/mcast-tools.git"),
            want
        );
        assert_eq!(
            github_path("git@github.com:nathanbent/mcast-tools.git"),
            want
        );
        assert_eq!(github_path("https://gitlab.com/a/b"), None);
        assert_eq!(github_path("https://github.com/a"), None);
    }
}
