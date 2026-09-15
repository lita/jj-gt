//! Git HTTPS auth for jj-lib's fetch/push.
//!
//! jj-lib does no credential handling itself: it spawns the real `git` binary,
//! which inherits our environment. So we point GIT_ASKPASS at a tiny script
//! that answers "Username" with x-access-token and "Password" with the GitHub
//! token. If the user already has a working credential helper (e.g. from
//! `gh auth setup-git`), it wins and our askpass is never consulted.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;

use anyhow::Result;

pub struct GitAuth {
    // Keep the tempdir alive for the lifetime of the process.
    _dir: tempfile::TempDir,
}

const ASKPASS_SCRIPT: &str = r#"#!/bin/sh
case "$1" in
  Username*) echo "x-access-token" ;;
  *) echo "$GT_GIT_TOKEN" ;;
esac
"#;

/// Install a non-interactive askpass answering with `token`. Call once, before
/// any jj-lib fetch/push. Safe to call early in main (single-threaded).
pub fn install(token: &str) -> Result<GitAuth> {
    let dir = tempfile::TempDir::new()?;
    let script_path = dir.path().join("jj-gt-askpass.sh");
    let mut f = std::fs::File::create(&script_path)?;
    f.write_all(ASKPASS_SCRIPT.as_bytes())?;
    f.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    drop(f);
    // SAFETY: called from main before any threads are spawned.
    unsafe {
        std::env::set_var("GIT_ASKPASS", &script_path);
        std::env::set_var("GT_GIT_TOKEN", token);
        std::env::set_var("GIT_TERMINAL_PROMPT", "0");
    }
    Ok(GitAuth { _dir: dir })
}
