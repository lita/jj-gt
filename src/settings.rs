//! UserSettings construction. jj-lib ships a defaults layer (misc.toml) that
//! makes UserSettings::from_config succeed, but user.name/user.email default
//! to "" — we fill them from git config so commits are authored properly.

use anyhow::{Context, Result};
use jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig};
use jj_lib::settings::UserSettings;

fn git_config(key: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["config", "--get", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let val = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!val.is_empty()).then_some(val)
}

pub fn user_settings() -> Result<UserSettings> {
    let mut config = StackedConfig::with_defaults();
    let mut layer = ConfigLayer::empty(ConfigSource::User);
    let name = git_config("user.name").unwrap_or_else(|| "gt demo".to_string());
    let email = git_config("user.email").unwrap_or_else(|| "gt@example.invalid".to_string());
    let username = std::env::var("USER").unwrap_or_else(|_| "gt".to_string());
    let hostname = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".to_string());
    layer.set_value("user.name", name).map_err(anyhow::Error::from)?;
    layer.set_value("user.email", email).map_err(anyhow::Error::from)?;
    layer
        .set_value("operation.username", username)
        .map_err(anyhow::Error::from)?;
    layer
        .set_value("operation.hostname", hostname)
        .map_err(anyhow::Error::from)?;
    config.add_layer(layer);
    UserSettings::from_config(config).context("failed to build jj UserSettings")
}
