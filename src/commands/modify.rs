//! gt modify: amend the working-copy changes into the current branch commit
//! (the parent of the scratch @), auto-restacking everything above it.

use anyhow::{Result, bail};
use jj_lib::repo::Repo as _;
use pollster::FutureExt as _;

use crate::engine::Gt;
use crate::util::short;

pub fn run(gt: &mut Gt, _all: bool) -> Result<()> {
    gt.snapshot()?;
    let wc = gt.wc_commit()?;
    if wc.is_empty(gt.repo.as_ref()).block_on()? {
        bail!("no changes in the working copy — nothing to amend");
    }
    let parent_id = wc
        .parent_ids()
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("working-copy commit has no parent"))?;
    let trunk_id = gt.trunk_id()?;
    if parent_id == trunk_id {
        bail!("@ sits directly on {} — use `gt create` to start a branch", gt.state.trunk);
    }
    let parent = gt.repo.store().get_commit(&parent_id)?;
    let branch = gt
        .repo
        .view()
        .local_bookmarks_for_commit(&parent_id)
        .map(|(name, _)| name.to_owned())
        .next();

    let mut tx = gt.start_tx();
    // Amend: the branch commit takes @'s tree (trees are snapshots, so the
    // squash is just "use the child's tree"). The bookmark follows the rewrite
    // and sibling descendants rebase automatically. @ itself is ABANDONED, not
    // rebased — rebasing it would re-apply its diff (e.g. a conflict
    // resolution) on top of the amended tree; abandoning lets jj open a fresh
    // empty @ on the amended commit, exactly like `jj squash`.
    let amended = tx
        .repo_mut()
        .rewrite_commit(&parent)
        .set_tree(wc.tree())
        .write()
        .block_on()?;
    tx.repo_mut().record_abandoned_commit(&wc);
    let rebased = tx.repo_mut().rebase_descendants().block_on()?;
    gt.explain.note(&format!(
        "amended {} → {}; {} descendant(s) auto-restacked",
        short(&jj_lib::object_id::ObjectId::hex(&parent_id)),
        short(&jj_lib::object_id::ObjectId::hex(amended.id())),
        rebased
    ));
    let label = branch
        .as_ref()
        .map(|b| b.as_str().to_string())
        .unwrap_or_else(|| short(&jj_lib::object_id::ObjectId::hex(&parent_id)));
    gt.finish_tx(tx, &format!("gt modify {label}"))?;
    println!("Amended {label} (descendants restacked automatically)");
    if let Err(e) = crate::commands::log::print_stack(gt) {
        eprintln!("gt: note: could not render the stack: {e:#}");
    }
    Ok(())
}
