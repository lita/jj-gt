//! jj-gt checkout <bookmark>: put a fresh (empty) working-copy commit on top of
//! the target — jj's model of "being on a branch".

use anyhow::Result;
use jj_lib::repo::Repo as _;
use pollster::FutureExt as _;

use crate::engine::Gt;

pub fn run(gt: &mut Gt, name: &str) -> Result<()> {
    gt.snapshot()?;
    let target_id = gt.resolve_bookmark(name)?;
    let target = gt.repo().store().get_commit(&target_id)?;
    let ws_name = gt.ws_name();
    let mut tx = gt.start_tx();
    // View-side checkout only: creates the new empty wc commit and points the
    // view's wc pointer at it. Files on disk don't move until finish_tx.
    tx.repo_mut().check_out(ws_name, &target).block_on()?;
    gt.finish_tx(tx, &format!("jj-gt checkout {name}"))?;
    println!("Checked out {name}");
    Ok(())
}
