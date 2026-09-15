//! Stack computation: the commits between trunk and @, bottom-up, with their
//! bookmarks — jj's revset engine does the graph walk (no parser needed).

use anyhow::{Result, bail};
use futures::TryStreamExt as _;
use jj_lib::commit::Commit;
use jj_lib::ref_name::RefNameBuf;
use jj_lib::repo::Repo as _;
use jj_lib::revset::{ResolvedRevsetExpression, RevsetStreamExt as _};
use pollster::FutureExt as _;

use crate::engine::Gt;

pub struct StackEntry {
    pub commit: Commit,
    pub bookmark: Option<RefNameBuf>,
    /// The scratch working-copy commit (empty, no description) on top.
    pub is_scratch: bool,
}

/// The full current stack: walk down from @ to the stack root (the child of
/// trunk), then take the root's whole descendant subtree — so upstack branches
/// stay visible and restackable even when @ sits mid-stack. Bottom-up order
/// (parents before children).
pub fn current_stack(gt: &Gt) -> Result<Vec<StackEntry>> {
    let trunk_id = gt.trunk_id()?;
    let wc = gt.wc_commit()?;
    // Downstack: trunk..@, bottom-up.
    let down_expr = ResolvedRevsetExpression::commit(trunk_id)
        .range(&ResolvedRevsetExpression::commit(wc.id().clone()));
    let down = down_expr.evaluate(gt.repo.as_ref())?;
    let mut down_ids: Vec<_> = futures::executor::block_on_stream(down.stream())
        .collect::<Result<Vec<_>, _>>()?;
    down_ids.reverse();
    let root_id = down_ids.first().cloned().unwrap_or_else(|| wc.id().clone());

    // Full stack = the root's descendant subtree (includes @ and any upstack).
    let expr = ResolvedRevsetExpression::commit(root_id).descendants();
    let revset = expr.evaluate(gt.repo.as_ref())?;
    let mut commits: Vec<Commit> = revset
        .stream()
        .commits(gt.repo.store())
        .try_collect()
        .block_on()?;
    drop(revset);
    commits.reverse();

    let mut entries = Vec::new();
    for commit in commits {
        let bookmark = gt
            .repo
            .view()
            .local_bookmarks_for_commit(commit.id())
            .map(|(name, _)| name.to_owned())
            .find(|name| **name != *gt.trunk());
        // Scratch commits: empty, undescribed, unbookmarked tips (gt keeps one
        // as @; abandoned ones may linger briefly mid-command).
        let is_scratch = bookmark.is_none()
            && commit.description().is_empty()
            && commit.is_empty(gt.repo.as_ref()).block_on()?;
        entries.push(StackEntry {
            commit,
            bookmark,
            is_scratch,
        });
    }
    Ok(entries)
}

/// The stack entries that represent branches (described commits, bottom-up).
/// Errors if a non-scratch commit has no bookmark — gt-created stacks always
/// bookmark every commit.
pub fn branch_entries(entries: &[StackEntry]) -> Result<Vec<(&StackEntry, RefNameBuf)>> {
    let mut out = Vec::new();
    for entry in entries {
        if entry.is_scratch {
            continue;
        }
        match &entry.bookmark {
            Some(name) => out.push((entry, name.clone())),
            None => bail!(
                "commit {} ({:?}) has no bookmark — create stack commits with `jj-gt create`",
                crate::util::short(&jj_lib::object_id::ObjectId::hex(entry.commit.id())),
                entry.commit.description().lines().next().unwrap_or("")
            ),
        }
    }
    Ok(out)
}
