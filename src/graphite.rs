//! Graphite CLI API. See docs/graphite-api.md for the upstream contract.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub struct Graphite {
    token: String,
    api_url: String,
    agent: ureq::Agent,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub github_login: Option<String>,
    pub can_submit_prs: Option<bool>,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrInfo {
    pub pr_number: u64,
    pub head_ref_name: String,
    pub state: PrState,
    #[serde(default)]
    pub versions: Vec<PrVersion>,
    pub merge_commit_sha: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrVersion {
    pub head_sha: String,
    pub created_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Submission {
    pub action: &'static str,
    pub head: String,
    pub head_sha: String,
    pub base: String,
    pub base_sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum SubmissionResult {
    #[serde(alias = "updated")]
    Created {
        head: String,
        #[serde(rename = "prNumber")]
        pr_number: u64,
        #[serde(rename = "prURL")]
        pr_url: String,
        #[serde(default)]
        warnings: Vec<String>,
    },
    Error {
        head: String,
        error: String,
    },
}

impl Graphite {
    pub fn new(token: String) -> Result<Self> {
        crate::config::validate_token(&token)?;
        let api_url = std::env::var("JJ_GT_GRAPHITE_API_URL")
            .unwrap_or_else(|_| "https://api.graphite.com/v1".into());
        let url = url::Url::parse(&api_url).context("invalid JJ_GT_GRAPHITE_API_URL")?;
        let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        if url.scheme() != "https" && !(url.scheme() == "http" && local) {
            bail!("Graphite API URL must use HTTPS (HTTP is allowed only for localhost tests)");
        }
        Ok(Self {
            token,
            api_url: api_url.trim_end_matches('/').into(),
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(60))
                .redirects(0)
                .build(),
        })
    }

    fn post<T: serde::de::DeserializeOwned>(&self, path: &str, body: Value) -> Result<T> {
        let response = self
            .agent
            .post(&format!("{}{path}", self.api_url))
            .set("Authorization", &format!("token {}", self.token))
            .set("User-Agent", concat!("jj-gt/", env!("CARGO_PKG_VERSION")))
            .set("Accept", "application/json")
            .send_json(body);
        match response {
            Ok(response) => response
                .into_json()
                .map_err(|_| anyhow::anyhow!("invalid Graphite API response for {path}")),
            Err(ureq::Error::Status(code, response)) => {
                if code == 401 || code == 403 {
                    bail!(
                        "Graphite API {path} failed ({code}); run `jj-gt auth --token <TOKEN>` and check repository access at https://app.graphite.com/settings"
                    );
                }
                let message = response
                    .into_string()
                    .unwrap_or_default()
                    .replace(&self.token, "[REDACTED]");
                bail!("Graphite API {path} failed ({code}): {message}")
            }
            Err(_) => {
                bail!("Graphite API {path} request failed; check your connection and API URL")
            }
        }
    }

    pub fn check_auth(&self, repo: Option<(&str, &str)>) -> Result<AuthStatus> {
        let body = match repo {
            Some((owner, name)) => json!({"repoOwner": owner, "repoName": name}),
            None => json!({}),
        };
        self.post("/graphite/check-auth", body)
    }

    pub fn pull_requests(
        &self,
        owner: &str,
        repo: &str,
        trunk: &str,
        branches: &[String],
    ) -> Result<HashMap<String, PrInfo>> {
        if branches.is_empty() {
            return Ok(HashMap::new());
        }
        #[derive(Deserialize)]
        struct Response {
            result: InfoResult,
        }
        #[derive(Deserialize)]
        #[serde(tag = "status", rename_all = "lowercase")]
        enum InfoResult {
            Ok { prs: Vec<PrInfo> },
            Error { message: String },
        }
        let response: Response = self.post(
            "/graphite/cli/pull-request-info",
            json!({
                "repoOwner": owner, "repoName": repo, "prNumbers": [],
                "prHeadRefNames": branches, "trunkBranchNames": [trunk],
                "consistent": true, "callsite": "jj-gt",
            }),
        )?;
        match response.result {
            InfoResult::Ok { prs } => {
                let mut result = HashMap::new();
                for pr in prs {
                    if !branches.contains(&pr.head_ref_name) {
                        continue;
                    }
                    // Reused branch names can have several historical PRs.
                    let replace = result.get(&pr.head_ref_name).is_none_or(|old: &PrInfo| {
                        (pr.state == PrState::Open, pr.pr_number)
                            > (old.state == PrState::Open, old.pr_number)
                    });
                    if replace {
                        result.insert(pr.head_ref_name.clone(), pr);
                    }
                }
                Ok(result)
            }
            InfoResult::Error { message } => bail!(
                "Graphite PR lookup failed: {}",
                message.replace(&self.token, "[REDACTED]")
            ),
        }
    }

    pub fn submit(
        &self,
        owner: &str,
        repo: &str,
        trunk: &str,
        prs: &[Submission],
    ) -> Result<Vec<SubmissionResult>> {
        #[derive(Deserialize)]
        struct Response {
            prs: Vec<SubmissionResult>,
        }
        let response: Response = self.post(
            "/graphite/submit/pull-requests",
            json!({
                "repoOwner": owner, "repoName": repo, "trunkBranchName": trunk,
                "useWebSubmit": false, "prs": prs,
            }),
        )?;
        Ok(response
            .prs
            .into_iter()
            .map(|result| match result {
                SubmissionResult::Error { head, error } => SubmissionResult::Error {
                    head,
                    error: error.replace(&self.token, "[REDACTED]"),
                },
                SubmissionResult::Created {
                    head,
                    pr_number,
                    pr_url,
                    warnings,
                } => SubmissionResult::Created {
                    head,
                    pr_number,
                    pr_url,
                    warnings: warnings
                        .into_iter()
                        .map(|warning| warning.replace(&self.token, "[REDACTED]"))
                        .collect(),
                },
            })
            .collect())
    }
}
