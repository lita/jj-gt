//! jj-gt submit: push every stack branch (force-with-lease) and create/update the
//! stacked PRs through Graphite or directly on GitHub.
//!
//! The push itself is a jj-lib operation: push_refs mutates the view's
//! remote-tracking bookmarks, so even a push ends with a transaction commit —
//! an op-log write most people don't expect.

use anyhow::{Context, Result, bail};
use jj_lib::git;
use jj_lib::object_id::ObjectId as _;
use jj_lib::refs::{LocalAndRemoteRef, RefPushAction, classify_ref_push_action};
use owo_colors::OwoColorize;

use crate::engine::Gt;
use crate::github::{GitHub, parse_github_remote};
use crate::gitnet::{Sideband, subprocess_options};
use crate::graphite::{Graphite, PrState, Submission, SubmissionResult};
use crate::stack::{branch_entries, current_stack};
use crate::util::git_output;

pub fn github_for(gt: &Gt) -> Result<GitHub> {
    let (owner, repo) = github_repo(gt)?;
    GitHub::new(owner, repo)
}

pub fn github_repo(gt: &Gt) -> Result<(String, String)> {
    let url = git_output(&gt.root, &["remote", "get-url", &gt.state.remote])?;
    parse_github_remote(&url)
        .with_context(|| format!("remote {} ({url}) is not a GitHub repo", gt.state.remote))
}

pub fn run(
    gt: &mut Gt,
    askpass: Option<&std::path::Path>,
    graphite: Option<&Graphite>,
) -> Result<()> {
    gt.snapshot()?;
    let entries = current_stack(gt)?;
    let branches = branch_entries(&entries)?;
    if branches.is_empty() {
        bail!("nothing to submit — the stack is empty (use `jj-gt create` first)");
    }
    // jj git push refuses these too: conflicted trees would push materialized
    // .jjconflict-* directories to GitHub.
    for (entry, name) in &branches {
        if entry.commit.has_conflict() {
            bail!(
                "{} has unresolved conflicts — resolve them before submitting",
                name.as_str()
            );
        }
        if entry.commit.description().trim().is_empty() {
            bail!(
                "{} has no description — describe it before submitting",
                name.as_str()
            );
        }
    }

    // Select the PR backend before pushing. If Graphite lacks repository
    // access, verify that GitHub credentials are available for the fallback.
    let mut github_fallback = None;
    let graphite_repo = if let Some(api) = graphite {
        let (owner, repo) = github_repo(gt)?;
        gt.explain.net("Graphite: check repository access");
        let auth = api.check_auth(Some((&owner, &repo)))?;
        if auth.can_submit_prs != Some(true) {
            eprintln!("Graphite cannot submit PRs to {owner}/{repo}; falling back to GitHub.");
            github_fallback = Some(GitHub::new(owner, repo).context(
                "GitHub fallback requires GitHub credentials; run `gh auth login` or set GITHUB_TOKEN",
            )?);
            None
        } else {
            Some((api, owner, repo))
        }
    } else {
        None
    };

    let graphite_submission = if let Some((api, owner, repo)) = graphite_repo {
        gt.explain.net("Graphite: look up stack PRs");
        let names = branches
            .iter()
            .map(|(_, name)| name.as_str().to_string())
            .collect::<Vec<_>>();
        let existing = api.pull_requests(&owner, &repo, &gt.state.trunk, &names)?;
        let trunk_id = gt.trunk_id()?;
        let mut submissions = Vec::new();
        for (entry, name) in &branches {
            let parent_id = entry
                .commit
                .parent_ids()
                .first()
                .context("stack commit has no parent")?;
            let base = if *parent_id == trunk_id {
                gt.state.trunk.clone()
            } else {
                branches
                    .iter()
                    .find(|(parent, _)| parent.commit.id() == parent_id)
                    .map(|(_, name)| name.as_str().to_string())
                    .context("stack commit's parent has no branch")?
            };
            let pr_number = existing
                .get(name.as_str())
                .filter(|pr| pr.state == PrState::Open)
                .map(|pr| pr.pr_number);
            submissions.push(Submission {
                action: if pr_number.is_some() {
                    "update"
                } else {
                    "create"
                },
                head: name.as_str().into(),
                head_sha: entry.commit.id().hex(),
                base,
                base_sha: parent_id.hex(),
                pr_number,
                title: pr_number.is_none().then(|| {
                    entry
                        .commit
                        .description()
                        .lines()
                        .next()
                        .unwrap_or(name.as_str())
                        .to_owned()
                }),
                body: pr_number.is_none().then(|| {
                    entry
                        .commit
                        .description()
                        .split_once('\n')
                        .map(|(_, body)| body.trim().to_owned())
                        .unwrap_or_default()
                }),
            });
        }
        Some((owner, repo, submissions))
    } else {
        None
    };

    // ── jj side: one lease-protected push for the whole stack ──
    let ex = gt.explain;
    let remote = gt.remote().to_owned();
    let mut bookmarks = Vec::new();
    {
        let view = gt.repo.view();
        for (_, name) in &branches {
            let local_target = view.get_local_bookmark(name);
            let remote_ref = view.get_remote_bookmark(name.to_remote_symbol(&remote));
            match classify_ref_push_action(LocalAndRemoteRef {
                local_target,
                remote_ref,
            }) {
                RefPushAction::Update(diff) => bookmarks.push((name.clone(), diff)),
                RefPushAction::AlreadyMatches => {
                    ex.note(&format!(
                        "{} already up to date on {}",
                        name.as_str(),
                        remote.as_str()
                    ));
                }
                RefPushAction::RemoteUntracked => bail!(
                    "{} exists on the remote but is untracked — `jj bookmark track {}@{}`",
                    name.as_str(),
                    name.as_str(),
                    remote.as_str()
                ),
                RefPushAction::LocalConflicted | RefPushAction::RemoteConflicted => {
                    bail!(
                        "bookmark {} is conflicted — resolve before submitting",
                        name.as_str()
                    )
                }
            }
        }
    }
    if !bookmarks.is_empty() {
        let n = bookmarks.len();
        let mut tx = gt.start_tx();
        let options = subprocess_options(&gt.settings, askpass)?;
        ex.net(&format!(
            "git push --force-with-lease {} branch(es) → {}",
            n,
            remote.as_str()
        ));
        let stats = git::push_refs(
            tx.repo_mut(),
            options,
            &remote,
            &git::GitPushRefTargets {
                bookmarks,
                tags: vec![],
            },
            &mut Sideband { explain: ex },
            &git::GitPushOptions::default(),
        )?;
        for (ref_name, reason) in stats.rejected.iter().chain(&stats.remote_rejected) {
            eprintln!(
                "{} push rejected: {} ({})",
                "✗".red(),
                ref_name.as_str(),
                reason.clone().unwrap_or_default()
            );
        }
        if !stats.all_ok() {
            bail!("push failed — the jj view was left untouched; re-run after `jj-gt sync`");
        }
        for name in &stats.pushed {
            println!("{} pushed {}", "✓".green(), name.as_str());
        }
        // push_refs already updated the view's remote-tracking bookmarks —
        // committing the transaction is what makes that an operation.
        gt.finish_tx(tx, "jj-gt submit: push stack")?;
    } else {
        println!("All branches already up to date on {}", remote.as_str());
    }

    if let Some((owner, repo, submissions)) = graphite_submission {
        let api = graphite.expect("Graphite submission requires a client");
        ex.net("Graphite: submit stack PRs");
        let results = api.submit(&owner, &repo, &gt.state.trunk, &submissions)?;
        let mut seen = std::collections::HashSet::new();
        let mut errors = Vec::new();
        for result in results {
            let head = match &result {
                SubmissionResult::Created { head, .. } | SubmissionResult::Error { head, .. } => {
                    head
                }
            };
            if !submissions.iter().any(|pr| pr.head == *head) || !seen.insert(head.clone()) {
                errors.push(format!(
                    "unexpected or duplicate branch in Graphite response: {head}"
                ));
                continue;
            }
            match result {
                SubmissionResult::Created {
                    head,
                    pr_number,
                    pr_url,
                    warnings,
                } => {
                    gt.state.prs.insert(head.clone(), pr_number);
                    // Persist each success even if another PR in the batch fails.
                    gt.state.save(&gt.root)?;
                    println!(
                        "  {} {}  {}",
                        format!("#{pr_number}").cyan().bold(),
                        head.magenta(),
                        pr_url.dimmed()
                    );
                    for warning in warnings {
                        eprintln!("Graphite: {warning}");
                    }
                }
                SubmissionResult::Error { head, error } => errors.push(format!("{head}: {error}")),
            }
        }
        for pr in &submissions {
            if !seen.contains(&pr.head) {
                errors.push(format!("no Graphite result for {}", pr.head));
            }
        }
        if !errors.is_empty() {
            bail!(
                "Graphite submission incomplete (successful PRs were saved):\n{}",
                errors.join("\n")
            );
        }
        return Ok(());
    }

    // ── GitHub side: stacked PRs, base = the parent commit's branch ──
    let gh = match github_fallback {
        Some(gh) => gh,
        None => github_for(gt)?,
    };
    let trunk_id = gt.trunk_id()?;
    let mut prs: Vec<(String, u64, String)> = Vec::new(); // (branch, number, url)
    for (entry, name) in &branches {
        let branch = name.as_str().to_string();
        let parent_id = entry.commit.parent_ids().first().cloned();
        let base = match &parent_id {
            Some(pid) if *pid != trunk_id => gt
                .repo
                .view()
                .local_bookmarks_for_commit(pid)
                .map(|(n, _)| n.to_owned())
                .find(|n| **n != *gt.trunk())
                .map(|n| n.as_str().to_string())
                .unwrap_or_else(|| gt.state.trunk.clone()),
            _ => gt.state.trunk.clone(),
        };
        let title = entry
            .commit
            .description()
            .lines()
            .next()
            .unwrap_or(&branch)
            .to_string();
        let existing = match gt.state.prs.get(&branch) {
            Some(number) => Some(gh.get_pr(*number)?),
            None => gh.find_open_pr(&branch)?,
        };
        let pr = match existing {
            Some(pr) if pr.state == "open" => {
                if pr.base.r#ref != base {
                    ex.net(&format!(
                        "PATCH pull #{}: base {} → {}",
                        pr.number, pr.base.r#ref, base
                    ));
                    gh.update_pr(pr.number, Some(&base), None)?
                } else {
                    pr
                }
            }
            _ => {
                ex.net(&format!("POST pulls: {branch} → {base}"));
                gh.create_pr(&branch, &base, &title, "")?
            }
        };
        gt.state.prs.insert(branch.clone(), pr.number);
        prs.push((branch.clone(), pr.number, pr.html_url.clone()));
    }
    gt.state.save(&gt.root)?;

    // Stack overview comment in every PR body, Graphite-style.
    for (branch, number, _) in &prs {
        let mut body = String::from("### Stack\n\n");
        for (other_branch, other_number, _) in prs.iter().rev() {
            let marker = if other_branch == branch {
                " ← this PR"
            } else {
                ""
            };
            body.push_str(&format!("- #{other_number}{marker}\n"));
        }
        body.push_str(&format!("- `{}` (trunk)\n", gt.state.trunk));
        body.push_str("\n*stacked with [jj-gt](https://github.com/jj-vcs/jj) on jj-lib*\n");
        gh.update_pr(*number, None, Some(&body))?;
    }

    println!();
    for (branch, number, url) in prs.iter().rev() {
        println!(
            "  {} {}  {}",
            format!("#{number}").cyan().bold(),
            branch.magenta(),
            url.dimmed()
        );
    }
    Ok(())
}
