//! jj-gt submit: push every stack branch (force-with-lease) and create/update the
//! stacked PRs on GitHub.
//!
//! The push itself is a jj-lib operation: push_refs mutates the view's
//! remote-tracking bookmarks, so even a push ends with a transaction commit —
//! an op-log write most people don't expect.

use anyhow::{Context, Result, bail};
use jj_lib::git;
use jj_lib::refs::{LocalAndRemoteRef, RefPushAction, classify_ref_push_action};
use owo_colors::OwoColorize;

use crate::engine::Gt;
use crate::github::{GitHub, parse_github_remote};
use crate::gitnet::{Sideband, subprocess_options};
use crate::stack::{branch_entries, current_stack};
use crate::util::git_output;

pub fn github_for(gt: &Gt) -> Result<GitHub> {
    let url = git_output(&gt.root, &["remote", "get-url", &gt.state.remote])?;
    let (owner, repo) = parse_github_remote(&url)
        .with_context(|| format!("remote {} ({url}) is not a GitHub repo", gt.state.remote))?;
    GitHub::new(owner, repo)
}

pub fn run(gt: &mut Gt, askpass: Option<&std::path::Path>) -> Result<()> {
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
            bail!("{} has no description — describe it before submitting", name.as_str());
        }
    }

    // ── jj side: one lease-protected push for the whole stack ──
    let ex = gt.explain;
    let remote = gt.remote().to_owned();
    let mut bookmarks = Vec::new();
    {
        let view = gt.repo().view();
        for (_, name) in &branches {
            let local_target = view.get_local_bookmark(name);
            let remote_ref = view.get_remote_bookmark(name.to_remote_symbol(&remote));
            match classify_ref_push_action(LocalAndRemoteRef {
                local_target,
                remote_ref,
            }) {
                RefPushAction::Update(diff) => bookmarks.push((name.clone(), diff)),
                RefPushAction::AlreadyMatches => {
                    ex.note(&format!("{} already up to date on {}", name.as_str(), remote.as_str()));
                }
                RefPushAction::RemoteUntracked => bail!(
                    "{} exists on the remote but is untracked — `jj bookmark track {}@{}`",
                    name.as_str(),
                    name.as_str(),
                    remote.as_str()
                ),
                RefPushAction::LocalConflicted | RefPushAction::RemoteConflicted => {
                    bail!("bookmark {} is conflicted — resolve before submitting", name.as_str())
                }
            }
        }
    }
    if !bookmarks.is_empty() {
        let n = bookmarks.len();
        let mut tx = gt.start_tx();
        let options = subprocess_options(&gt.settings(), askpass)?;
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

    // ── GitHub side: stacked PRs, base = the parent commit's branch ──
    let gh = github_for(gt)?;
    let trunk_id = gt.trunk_id()?;
    let mut prs: Vec<(String, u64, String)> = Vec::new(); // (branch, number, url)
    for (entry, name) in &branches {
        let branch = name.as_str().to_string();
        let parent_id = entry.commit.parent_ids().first().cloned();
        let base = match &parent_id {
            Some(pid) if *pid != trunk_id => gt
                .repo()
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
                    ex.net(&format!("PATCH pull #{}: base {} → {}", pr.number, pr.base.r#ref, base));
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
            let marker = if other_branch == branch { " ← this PR" } else { "" };
            body.push_str(&format!("- #{other_number}{marker}\n"));
        }
        body.push_str(&format!("- `{}` (trunk)\n", gt.state.trunk));
        body.push_str("\n*stacked with [jj-gt](https://github.com/jj-vcs/jj) on jj-lib*\n");
        gh.update_pr(*number, None, Some(&body))?;
    }

    println!();
    for (branch, number, url) in prs.iter().rev() {
        println!("  {} {}  {}", format!("#{number}").cyan().bold(), branch.magenta(), url.dimmed());
    }
    Ok(())
}
