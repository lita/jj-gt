use std::path::Path;

use anyhow::{Result, bail};

/// Slugify a commit message into a branch name: "feat(api): Add users" →
/// "feat-api-add-users". Lowercased because jj bookmark names are effectively
/// case-insensitive on macOS while GitHub branch names are case-sensitive.
pub fn slugify(message: &str, max_len: usize) -> String {
    let first_line = message.lines().next().unwrap_or("");
    let mut slug = String::new();
    let mut last_dash = true; // suppress leading dash
    for ch in first_line.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= max_len {
            break;
        }
    }
    slug.trim_matches('-').to_string()
}

/// Branch prefix for generated branches, Graphite-style: "<user>/".
pub fn branch_user_prefix() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "dev".to_string())
}

/// Run `git <args>` in `dir` and return trimmed stdout.
pub fn git_output(dir: &Path, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()?;
    if !out.status.success() {
        bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}

pub fn short(hex: &str) -> String {
    hex[..8.min(hex.len())].to_string()
}
