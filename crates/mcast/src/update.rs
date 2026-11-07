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
    /// Only report whether a newer commit is available; don't install
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
    /// Reinstall even if cargo thinks this revision is already installed
    #[arg(short, long)]
    force: bool,
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
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
    let up_to_date = matches!((BUILD_SHA, &latest), (Some(b), Some(l)) if b == l);

    if a.check {
        match (up_to_date, &latest, BUILD_SHA) {
            (true, _, _) => println!("Up to date."),
            (false, Some(_), Some(_)) => println!("An update is available: run `mcast update`."),
            _ => println!("Can't compare; run `mcast update` to rebuild anyway."),
        }
        return Ok(());
    }
    if up_to_date && !a.force {
        println!("Up to date. Use --force to rebuild anyway.");
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
    // A local path is always rebuilt; a git source is skipped by cargo when the
    // same revision is installed, which can't happen here unless we don't know
    // our own commit. --force covers that and the explicit request.
    if a.force || BUILD_SHA.is_none() {
        cmd.arg("--force");
    }

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
