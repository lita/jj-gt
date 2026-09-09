//! Minimal GitHub REST client for the stacked-PR half of `gt submit`/`gt sync`.
//! jj-lib is not involved here — pushing the branches is jj-lib's job, turning
//! them into a PR stack is plain HTTPS.

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

pub struct GitHub {
    token: String,
    pub owner: String,
    pub repo: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Pr {
    pub number: u64,
    pub html_url: String,
    pub state: String,
    #[serde(default)]
    pub merged_at: Option<String>,
    pub base: PrRef,
    pub head: PrRef,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrRef {
    #[serde(rename = "ref")]
    pub r#ref: String,
}

/// Parse "git@github.com:owner/repo.git" or "https://github.com/owner/repo(.git)".
pub fn parse_github_remote(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.splitn(2, '/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.trim_end_matches('/').to_string();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner, repo))
}

/// GITHUB_TOKEN, or whatever `gh` is logged in with.
pub fn discover_token() -> Result<String> {
    if let Ok(tok) = std::env::var("GITHUB_TOKEN")
        && !tok.trim().is_empty()
    {
        return Ok(tok.trim().to_string());
    }
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .context("failed to run `gh auth token` (set GITHUB_TOKEN or install gh)")?;
    if !out.status.success() {
        bail!(
            "`gh auth token` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let tok = String::from_utf8(out.stdout)?.trim().to_string();
    if tok.is_empty() {
        bail!("`gh auth token` returned an empty token");
    }
    Ok(tok)
}

impl GitHub {
    pub fn new(owner: String, repo: String) -> Result<Self> {
        Ok(GitHub {
            token: discover_token()?,
            owner,
            repo,
        })
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let url = format!("https://api.github.com{path}");
        let req = ureq::request(method, &url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/vnd.github+json")
            .set("X-GitHub-Api-Version", "2022-11-28")
            .set("User-Agent", "gt-jjcon-demo");
        let resp = match body {
            Some(json) => req.send_json(json),
            None => req.call(),
        };
        match resp {
            Ok(r) => Ok(r.into_json()?),
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                Err(anyhow!("GitHub API {method} {path} failed ({code}): {text}"))
            }
            Err(e) => Err(anyhow!("GitHub API {method} {path} failed: {e}")),
        }
    }

    pub fn default_branch(&self) -> Result<String> {
        let repo = self.request("GET", &format!("/repos/{}/{}", self.owner, self.repo), None)?;
        repo["default_branch"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("repo response had no default_branch"))
    }

    /// Open PR whose head is `branch`, if any.
    pub fn find_open_pr(&self, branch: &str) -> Result<Option<Pr>> {
        let prs: Vec<Pr> = serde_json::from_value(self.request(
            "GET",
            &format!(
                "/repos/{}/{}/pulls?state=open&head={}:{}",
                self.owner, self.repo, self.owner, branch
            ),
            None,
        )?)?;
        Ok(prs.into_iter().next())
    }

    pub fn get_pr(&self, number: u64) -> Result<Pr> {
        Ok(serde_json::from_value(self.request(
            "GET",
            &format!("/repos/{}/{}/pulls/{number}", self.owner, self.repo),
            None,
        )?)?)
    }

    pub fn create_pr(&self, head: &str, base: &str, title: &str, body: &str) -> Result<Pr> {
        Ok(serde_json::from_value(self.request(
            "POST",
            &format!("/repos/{}/{}/pulls", self.owner, self.repo),
            Some(serde_json::json!({
                "title": title,
                "head": head,
                "base": base,
                "body": body,
            })),
        )?)?)
    }

    pub fn update_pr(
        &self,
        number: u64,
        base: Option<&str>,
        body: Option<&str>,
    ) -> Result<Pr> {
        let mut patch = serde_json::Map::new();
        if let Some(base) = base {
            patch.insert("base".into(), base.into());
        }
        if let Some(body) = body {
            patch.insert("body".into(), body.into());
        }
        Ok(serde_json::from_value(self.request(
            "PATCH",
            &format!("/repos/{}/{}/pulls/{number}", self.owner, self.repo),
            Some(serde_json::Value::Object(patch)),
        )?)?)
    }
}
