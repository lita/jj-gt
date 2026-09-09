//! gt sync: fetch trunk, drop merged branches, rebase the stack — the command
//! where jj's rebase machinery shines.
//!
//! Detection is two-tier:
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
use crate::stack::current_stack;
use crate::gitnet::{Sideband, subprocess_options};
use crate::util::short;

pub fn run(gt: &mut Gt, askpass: Option<&std::path::Path>) -> Result<()> {
    gt.snapshot()?;
    let ex = gt.explain;
    let remote = gt.remote().to_owned();
    // Compute the stack BEFORE fetching, against the old trunk.
    let entries = current_stack(gt)?;

    let mut tx = gt.repo.start_transaction();

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
        record_synthetic_predecessors: false,
        remote_auto_track_bookmarks: auto_track,
    };
    {
        let options = subprocess_options(&gt.settings, askpass)?;
        let mut fetch = git::GitFetch::new(tx.repo_mut(), options, &import_options)?;
        ex.net(&format!("git fetch {} (updates .git/refs/remotes/* only)", remote.as_str()));
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
        .ok_or_else(|| anyhow!("trunk {:?} is missing or conflicted after fetch", gt.state.trunk))?;
    ex.note(&format!("trunk {} is now {}", gt.state.trunk, short(&new_trunk.hex())));

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
        let is_merged = tx
            .repo()
            .index()
            .is_ancestor(commit.id(), &new_trunk)
            .block_on()
            .map_err(|e| anyhow!("index error: {e}"))?;
        // Children of a merged/landed commit restack onto the new trunk.
        let target_parent = commit
            .parent_ids()
            .first()
            .and_then(|p| replacement.get(p).cloned())
            .unwrap_or_else(|| new_trunk.clone());
        if is_merged {
            // Landed via merge/fast-forward: the commit is in trunk history.
            if let Some(name) = &entry.bookmark {
                Gt::delete_bookmark(&mut tx, name.as_ref());
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
            ex.note(&format!("{label}: rebased to EMPTY on trunk → squash-merged, abandoning"));
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
            ex.note(&format!("{label}: rebase produced conflicts (materialized, not fatal)"));
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

    let changed = gt.finish_tx(tx, "gt sync")?;

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
        println!("{} {} merged into {} — branch deleted", "✓".green(), branch.magenta(), gt.state.trunk);
    }
    if restacked > 0 {
        println!("{} restacked {restacked} commit(s) onto {}", "↻".cyan(), gt.state.trunk);
    }
    for branch in &conflicted {
        println!("{} {branch} has conflicts — resolve in the working copy", "!".red().bold());
    }
    if let Err(e) = crate::commands::log::print_stack(gt) {
        eprintln!("gt: note: could not render the stack: {e:#}");
    }
    Ok(())
}
