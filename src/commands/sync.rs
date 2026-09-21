//! jj-gt sync: fetch trunk, drop merged branches, rebase the stack — the command
//! where jj's rebase machinery shines.
//!
//! Graphite mode also checks confirmed merges of the exact submitted head.
//! Git-based detection handles:
//!  - merge/fast-forward landings: index.is_ancestor(commit, new trunk)
//!  - squash merges: the commit rebases to EMPTY on the new trunk → abandon

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use jj_lib::git;
use jj_lib::object_id::ObjectId as _;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::Repo as _;
use jj_lib::rewrite::rebase_commit;
use jj_lib::str_util::StringExpression;
use owo_colors::OwoColorize;
use pollster::FutureExt as _;

use crate::engine::Gt;
use crate::gitnet::{Sideband, subprocess_options};
use crate::graphite::{Graphite, PrState};
use crate::stack::current_stack;
use crate::util::short;

pub fn run(
    gt: &mut Gt,
    askpass: Option<&std::path::Path>,
    graphite: Option<&Graphite>,
) -> Result<()> {
    gt.snapshot()?;
    let ex = gt.explain;
    let remote = gt.remote().to_owned();
    // Compute the stack BEFORE fetching, against the old trunk.
    let entries = current_stack(gt)?;

    let graphite_prs = if let Some(api) = graphite {
        let (owner, repo) = crate::commands::submit::github_repo(gt)?;
        let names = entries
            .iter()
            .filter_map(|entry| entry.bookmark.as_ref().map(|name| name.as_str().to_owned()))
            .collect::<Vec<_>>();
        ex.net("Graphite: refresh stack PR status");
        api.pull_requests(&owner, &repo, &gt.state.trunk, &names)?
    } else {
        HashMap::new()
    };

    let mut tx = gt.start_tx();

    // ── fetch + import: the network write, then the view write ──
    let mut auto_track = HashMap::new();
    auto_track.insert(
        RemoteNameBuf::from(remote.as_str()),
        StringExpression::all().to_matcher(),
    );
    let import_options = git::GitImportOptions {
        // Commits whose remote branch disappeared (deleted after merge) get
        // abandoned during import.
        abandon_unreachable_commits: true,
        // A force-pushed jj rewrite (for example, a collaborator's `jj-gt
        // modify`) has the same change id as the commit it replaces. Preserve
        // that predecessor relationship so rebase_descendants() moves our
        // scratch working-copy commit onto the replacement instead of
        // abandoning it onto the old commit's parent (or the root commit).
        record_synthetic_predecessors: true,
        remote_auto_track_bookmarks: auto_track,
    };
    {
        let options = subprocess_options(&gt.settings, askpass)?;
        let mut fetch = git::GitFetch::new(tx.repo_mut(), options, &import_options)?;
        ex.net(&format!(
            "git fetch {} (updates .git/refs/remotes/* only)",
            remote.as_str()
        ));
        let refspecs = git::expand_fetch_refspecs(
            &remote,
            git::GitFetchRefExpression {
                bookmark: StringExpression::all(),
                tag: StringExpression::none(),
            },
        )?;
        fetch.fetch(&remote, refspecs, &mut Sideband { explain: ex }, None)?;
        let stats = fetch.import_refs().block_on()?;
        ex.note(&format!(
            "import_refs: {} bookmark(s) moved in the jj view, {} commit(s) abandoned",
            stats.changed_remote_bookmarks.len(),
            stats.abandoned_commits.len()
        ));
    }
    tx.repo_mut().rebase_descendants().block_on()?;

    // ── trunk: local trunk follows the freshly-imported remote trunk ──
    let trunk_name = gt.trunk().to_owned();
    let new_trunk = tx
        .repo()
        .view()
        .get_local_bookmark(&trunk_name)
        .as_normal()
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "trunk {:?} is missing or conflicted after fetch",
                gt.state.trunk
            )
        })?;
    ex.note(&format!(
        "trunk {} is now {}",
        gt.state.trunk,
        short(&new_trunk.hex())
    ));

    // ── restack: rebase every stack commit onto its (possibly replaced)
    // parent, bottom-up. A map old-id → replacement handles merged/abandoned
    // parents and sibling upstacks alike.
    let mut replacement: HashMap<jj_lib::backend::CommitId, jj_lib::backend::CommitId> =
        HashMap::new();
    let mut merged: Vec<String> = Vec::new();
    let mut restacked = 0usize;
    let mut conflicted: Vec<String> = Vec::new();
    // NOTE: scratch @ is restacked through the same loop — "it follows via
    // rebase_descendants" is only true when its parent gets REWRITTEN, which a
    // merge-commit/fast-forward landing does not do.
    for entry in &entries {
        let commit = &entry.commit;
        let label = entry
            .bookmark
            .as_ref()
            .map(|b| b.as_str().to_string())
            .unwrap_or_else(|| short(&commit.id().hex()));
        let in_trunk = tx
            .repo()
            .index()
            .is_ancestor(commit.id(), &new_trunk)
            .block_on()
            .map_err(|e| anyhow!("index error: {e}"))?;
        // A server-side squash may include edits, so rebasing it need not
        // become empty. Only drop it if the merged PR's latest submitted head
        // matches our commit and its merge commit is in the fetched trunk.
        // Unsubmitted local edits and merges not fetched yet must survive.
        let mut graphite_merged = false;
        if let Some(pr) = entry
            .bookmark
            .as_ref()
            .and_then(|name| graphite_prs.get(name.as_str()))
            && pr.state == PrState::Merged
            && pr
                .versions
                .iter()
                .max_by(|left, right| left.created_at.cmp(&right.created_at))
                .is_some_and(|version| version.head_sha == commit.id().hex())
            && let Some(sha) = &pr.merge_commit_sha
            && sha.len() == 40
            && sha.bytes().all(|b| b.is_ascii_hexdigit())
            && let Some(merge_id) = jj_lib::backend::CommitId::try_from_hex(sha)
        {
            graphite_merged = tx.repo().index().has_id(&merge_id).block_on()?
                && tx
                    .repo()
                    .index()
                    .is_ancestor(&merge_id, &new_trunk)
                    .block_on()
                    .unwrap_or(false);
        }
        // Children of a merged/landed commit restack onto the new trunk.
        let target_parent = commit
            .parent_ids()
            .first()
            .and_then(|p| replacement.get(p).cloned())
            .unwrap_or_else(|| new_trunk.clone());
        if in_trunk || graphite_merged {
            // Landed via merge/fast-forward: the commit is in trunk history.
            if let Some(name) = &entry.bookmark {
                Gt::delete_bookmark(&mut tx, name.as_ref());
            }
            if graphite_merged && !in_trunk {
                ex.note(&format!(
                    "{label}: Graphite confirms merged into fetched trunk"
                ));
                tx.repo_mut().record_abandoned_commit(commit);
            }
            replacement.insert(commit.id().clone(), new_trunk.clone());
            merged.push(label);
            continue;
        }
        if commit.parent_ids().first() == Some(&target_parent) {
            // Already based correctly (e.g. repeat sync).
            replacement.insert(commit.id().clone(), commit.id().clone());
            continue;
        }
        let new_commit =
            rebase_commit(tx.repo_mut(), commit.clone(), vec![target_parent.clone()]).block_on()?;
        let now_empty = new_commit.is_empty(tx.repo()).block_on()?;
        if now_empty && !commit.is_empty(tx.repo()).block_on().unwrap_or(false) {
            // Rebased to empty on the new trunk: this was a squash merge.
            ex.note(&format!(
                "{label}: rebased to EMPTY on trunk → squash-merged, abandoning"
            ));
            tx.repo_mut().record_abandoned_commit(&new_commit);
            if let Some(name) = &entry.bookmark {
                Gt::delete_bookmark(&mut tx, name.as_ref());
            }
            replacement.insert(commit.id().clone(), target_parent);
            merged.push(label);
            continue;
        }
        if new_commit.has_conflict() {
            conflicted.push(label.clone());
            ex.note(&format!(
                "{label}: rebase produced conflicts (materialized, not fatal)"
            ));
        } else {
            ex.note(&format!(
                "{label}: rebased {} → {}",
                short(&commit.id().hex()),
                short(&new_commit.id().hex())
            ));
        }
        if !entry.is_scratch {
            restacked += 1;
        }
        replacement.insert(commit.id().clone(), new_commit.id().clone());
    }
    // Bookmarks and @ follow the rewrites here; also flushes parent_mapping
    // (mandatory before commit — jj-lib asserts).
    tx.repo_mut().rebase_descendants().block_on()?;

    let changed = gt.finish_tx(tx, "jj-gt sync")?;

    for (branch, pr) in &graphite_prs {
        if !merged.contains(branch) {
            if pr.state == PrState::Open {
                gt.state.prs.insert(branch.clone(), pr.pr_number);
            } else {
                gt.state.prs.remove(branch);
            }
        }
    }

    // Forget PR numbers for branches that no longer exist.
    for branch in &merged {
        gt.state.prs.remove(branch);
    }
    gt.state.save(&gt.root)?;

    if !changed && merged.is_empty() {
        println!("Already up to date.");
        return Ok(());
    }
    for branch in &merged {
        println!(
            "{} {} merged into {} — branch deleted",
            "✓".green(),
            branch.magenta(),
            gt.state.trunk
        );
    }
    if restacked > 0 {
        println!(
            "{} restacked {restacked} commit(s) onto {}",
            "↻".cyan(),
            gt.state.trunk
        );
    }
    for branch in &conflicted {
        println!(
            "{} {branch} has conflicts — resolve in the working copy",
            "!".red().bold()
        );
    }
    if let Err(e) = crate::commands::log::print_stack(gt) {
        eprintln!("jj-gt: note: could not render the stack: {e:#}");
    }
    Ok(())
}
