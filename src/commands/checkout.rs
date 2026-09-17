//! jj-gt checkout <bookmark|commit>: resume saved work, or put a fresh working
//! copy on a named/historical commit — jj's model of "being on a branch".

use anyhow::Result;
use jj_lib::repo::Repo as _;
use pollster::FutureExt as _;

use crate::engine::Gt;

pub fn run(gt: &mut Gt, name: &str) -> Result<()> {
    gt.snapshot()?;
    let target_id = gt.resolve_checkout_target(name)?;
    let target = gt.repo.store().get_commit(&target_id)?;
    let old_wc = gt.wc_commit()?;
    let saved_id = if old_wc.id() != &target_id && gt.is_saved_work(&old_wc)? {
        Some(gt.short_commit_id(old_wc.id())?)
    } else {
        None
    };
    let resume = gt.is_saved_work(&target)?;
    let ws_name = gt.ws_name();
    let mut tx = gt.start_tx();
    // Resume the saved commit itself so create/modify can use its diff.
    // For a branch or historical commit, keep the fresh scratch @ on top.
    // Files on disk don't move until finish_tx.
    if resume {
        tx.repo_mut().edit(ws_name, &target).block_on()?;
    } else {
        tx.repo_mut().check_out(ws_name, &target).block_on()?;
    }
    gt.finish_tx(tx, &format!("jj-gt checkout {name}"))?;
    if let Some(id) = saved_id {
        println!("Saved unbookmarked work at {id}; return with `jj-gt checkout {id}`");
    }
    if resume {
        println!("Resumed saved work at {}", gt.short_commit_id(&target_id)?);
    } else {
        println!("Checked out {name}");
    }
    Ok(())
}
