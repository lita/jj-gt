//! User credentials live outside the repository, separate from .jj/gt.json.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

pub fn path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        bail!("HOME must be an absolute path to store credentials outside the repository");
    }
    Ok(home.join(".jj-gt/config"))
}

fn read(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid config at {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(err) => Err(err).with_context(|| format!("could not read {}", path.display())),
    }
}

pub fn validate_token(token: &str) -> Result<&str> {
    let token = token.trim();
    if token.is_empty()
        || token.chars().any(char::is_whitespace)
        || token.chars().any(char::is_control)
    {
        bail!("Graphite token must be nonempty and contain no whitespace or control characters");
    }
    Ok(token)
}

pub fn graphite_token() -> Result<Option<String>> {
    if let Some(token) = std::env::var_os("GRAPHITE_AUTH_TOKEN") {
        let token = token
            .into_string()
            .map_err(|_| anyhow::anyhow!("GRAPHITE_AUTH_TOKEN is not UTF-8"))?;
        return Ok(Some(validate_token(&token)?.to_owned()));
    }
    let config = read(&path()?)?;
    match config.get("authToken") {
        None => Ok(None),
        Some(Value::String(token)) => Ok(Some(validate_token(token)?.to_owned())),
        Some(_) => bail!("authToken in ~/.jj-gt/config must be a string"),
    }
}

pub fn save_token(path: &Path, token: &str) -> Result<()> {
    let token = validate_token(token)?;
    let mut config = read(path)?;
    config.insert("authToken".into(), token.into());
    let parent = path.parent().context("config path has no parent")?;
    std::fs::create_dir_all(parent)?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer_pretty(&mut file, &config)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path)
        .context("could not save Graphite token")?;
    Ok(())
}
