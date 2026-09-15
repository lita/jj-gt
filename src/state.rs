//! jj-gt's own tiny state file, stored inside .jj so the working-copy snapshot
//! never picks it up: .jj/gt.json

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GtState {
    /// Trunk bookmark name, e.g. "main".
    pub trunk: String,
    /// Git remote name, e.g. "origin".
    pub remote: String,
    /// bookmark name → PR number, recorded by `jj-gt submit`.
    #[serde(default)]
    pub prs: BTreeMap<String, u64>,
}

impl GtState {
    pub fn path(workspace_root: &Path) -> std::path::PathBuf {
        workspace_root.join(".jj").join("gt.json")
    }

    pub fn load(workspace_root: &Path) -> Result<Self> {
        let path = Self::path(workspace_root);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("no jj-gt state at {} — run `jj-gt init` first", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save(&self, workspace_root: &Path) -> Result<()> {
        let path = Self::path(workspace_root);
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}
