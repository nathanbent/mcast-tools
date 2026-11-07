//! Embeds the git commit the binary was built from, so `mcast update --check`
//! and `mcast --version` can tell which revision is installed.

use std::path::PathBuf;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=MCAST_GIT_SHA");

    let sha = std::env::var("MCAST_GIT_SHA")
        .ok()
        .or_else(|| git(&["rev-parse", "HEAD"]));
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    match sha {
        Some(sha) => {
            println!("cargo:rustc-env=MCAST_GIT_SHA={sha}");
            println!(
                "cargo:rustc-env=MCAST_VERSION={version} ({})",
                &sha[..sha.len().min(10)]
            );
        }
        None => println!("cargo:rustc-env=MCAST_VERSION={version}"),
    }

    // Rebuild when HEAD moves (checkout, commit, pull).
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        let dir = PathBuf::from(dir);
        for f in ["HEAD", "packed-refs"] {
            let p = dir.join(f);
            if p.exists() {
                println!("cargo:rerun-if-changed={}", p.display());
            }
        }
        if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
            let p = dir.join(head_ref);
            if p.exists() {
                println!("cargo:rerun-if-changed={}", p.display());
            }
        }
    }
}
