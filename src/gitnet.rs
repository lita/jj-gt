//! Network glue for jj-lib's git subprocess: auth env injection + callback.

use std::collections::HashMap;

use anyhow::Result;
use jj_lib::git::{GitProgress, GitSidebandLineTerminator, GitSubprocessCallback, GitSubprocessOptions};
use jj_lib::settings::UserSettings;

use crate::explain::Explain;

/// Subprocess options with GIT_ASKPASS pointing at our token helper.
/// jj-lib applies `environment` on top of the inherited env, after its own
/// locale scrubbing — the documented injection point for auth.
pub fn subprocess_options(settings: &UserSettings, askpass: Option<&std::path::Path>) -> Result<GitSubprocessOptions> {
    let mut options = GitSubprocessOptions::from_settings(settings)?;
    let env: &mut HashMap<_, _> = &mut options.environment;
    if let Some(askpass) = askpass {
        env.insert("GIT_ASKPASS".into(), askpass.as_os_str().to_owned());
    }
    env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
    Ok(options)
}

/// Callback that forwards the remote's sideband messages (e.g. GitHub's
/// "Create a pull request for ..." hints) to stderr in explain mode.
pub struct Sideband {
    pub explain: Explain,
}

impl GitSubprocessCallback for Sideband {
    fn needs_progress(&self) -> bool {
        false
    }

    fn progress(&mut self, _progress: &GitProgress) -> std::io::Result<()> {
        Ok(())
    }

    fn local_sideband(
        &mut self,
        _message: &[u8],
        _term: Option<GitSidebandLineTerminator>,
    ) -> std::io::Result<()> {
        Ok(())
    }

    fn remote_sideband(
        &mut self,
        message: &[u8],
        _term: Option<GitSidebandLineTerminator>,
    ) -> std::io::Result<()> {
        if self.explain.on {
            let text = String::from_utf8_lossy(message);
            let text = text.trim();
            if !text.is_empty() {
                self.explain.net(&format!("remote: {text}"));
            }
        }
        Ok(())
    }
}
