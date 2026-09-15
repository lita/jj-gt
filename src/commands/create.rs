//! jj-gt create -m <msg>: turn the working copy into a stack branch.
//!
//! jj model: @ already *is* a commit (snapshot made sure it holds the working
//! copy). We describe it, bookmark it, and open a fresh empty @ on top.

use anyhow::{Result, bail};
use jj_lib::op_store::RefTarget;
use jj_lib::ref_name::RefName;
use pollster::FutureExt as _;

use crate::engine::Gt;
use crate::util::{branch_user_prefix, slugify};

pub fn run(gt: &mut Gt, message: &str, _all: bool) -> Result<()> {
    gt.snapshot()?;
    let wc = gt.wc_commit()?;
    if wc.is_empty(gt.repo.as_ref()).block_on()? {
        bail!("no changes in the working copy — edit some files before `jj-gt create`");
    }

    // Graphite-style generated branch name.
    let slug = slugify(message, 48);
    if slug.is_empty() {
        bail!("could not derive a branch name from the message");
    }
    let mut branch = format!("{}/{}", branch_user_prefix(), slug);
    let mut n = 1;
    while gt
        .repo
        .view()
        .get_local_bookmark(RefName::new(&branch))
        .is_present()
    {
        n += 1;
        branch = format!("{}/{}-{}", branch_user_prefix(), slug, n);
    }

    let ws_name = gt.ws_name();
    let mut tx = gt.start_tx();
    // 1. Give @ its commit message (a rewrite: same change id, new commit id).
    let described = tx
        .repo_mut()
        .rewrite_commit(&wc)
        .set_description(message)
        .write()
        .block_on()?;
    tx.repo_mut().rebase_descendants().block_on()?;
    // 2. Bookmark it — this is the branch jj-gt submit will push.
    tx.repo_mut()
        .set_local_bookmark_target(RefName::new(&branch), RefTarget::normal(described.id().clone()));
    // 3. Fresh empty @ on top, so the next edits become the next stack entry.
    tx.repo_mut()
        .check_out(ws_name, &described)
        .block_on()?;
    gt.finish_tx(tx, &format!("jj-gt create {branch}"))?;
    println!("Created branch {branch}");
    if let Err(e) = crate::commands::log::print_stack(gt) {
        eprintln!("jj-gt: note: could not render the stack: {e:#}");
    }
    Ok(())
}
