//! jj-gt init: colocate jj onto the repo.
//!
//! On an existing git clone this is Workspace::init_external_git — which, and
//! this is the first talk beat, does NOT import any git refs: the jj view
//! starts empty and @ sits on the root commit. We must run our own
//! "import git refs" and "import git head" operations, exactly like
//! `jj git init --colocate` does.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use jj_lib::git;
use jj_lib::object_id::ObjectId as _;
use jj_lib::ref_name::RemoteNameBuf;
use jj_lib::repo::Repo as _;
use jj_lib::str_util::StringExpression;
use jj_lib::workspace::Workspace;
use pollster::FutureExt as _;

use crate::engine::export_refs;
use crate::explain::Explain;
use crate::state::GtState;
use crate::util::{git_output, short};

pub fn run(cwd: &Path, explain: Explain) -> Result<()> {
    let root = cwd.to_path_buf();
    if root.join(".jj").exists() {
        bail!("{} is already a jj workspace", root.display());
    }
    let settings = crate::settings::user_settings()?;
    let ex = explain;

    if !root.join(".git").exists() {
        ex.section("jj-gt init: no .git yet — creating one");
        git_output(&root, &["init", "-b", "main"])?;
    }
    ex.section("jj-gt init: colocating jj onto the git repo");
    ex.note("Workspace::init_external_git — creates .jj, does NOT import refs");
    let (mut workspace, repo) = Workspace::init_external_git(&settings, &root, &root.join(".git"))
        .block_on()
        .context("failed to initialize jj against .git")?;
    // Keep `git status` clean about jj's own state dir, like jj CLI does.
    let exclude = root.join(".git").join("info").join("exclude");
    if let Ok(existing) = std::fs::read_to_string(&exclude)
        && !existing.contains(".jj/")
    {
        let _ = std::fs::write(&exclude, format!("{existing}\n/.jj/\n"));
    }
    let ws_name = workspace.workspace_name().to_owned();

    // Operation 2: import git refs. Auto-track everything on origin so remote
    // branches become local bookmarks (clone-like UX for the demo).
    let mut auto_track = HashMap::new();
    auto_track.insert(
        RemoteNameBuf::from("origin"),
        StringExpression::all().to_matcher(),
    );
    let import_options = git::GitImportOptions {
        abandon_unreachable_commits: false,
        record_synthetic_predecessors: false,
        remote_auto_track_bookmarks: auto_track,
    };
    ex.note(
        "no Snapshot phase: the jj view is empty and git already wrote the files — \
         nothing to fold into @",
    );
    ex.phase_transact();
    let mut tx = repo.start_transaction();
    let stats = git::import_refs(tx.repo_mut(), &import_options).block_on()?;
    ex.note(&format!(
        "import_refs: {} remote bookmark(s) imported into the jj view",
        stats.changed_remote_bookmarks.len()
    ));
    let repo = if tx.repo().has_changes() {
        export_refs(tx.repo_mut(), ex)?;
        let repo = tx.commit("jj-gt init: import git refs").block_on()?;
        ex.op_log("jj-gt init: import git refs", &repo.op_id().hex());
        ex.note("no Sync/Finalize: @ did not move — the next transaction records the op id");
        repo
    } else {
        ex.note("transaction has no changes — dropped without publishing an operation");
        repo
    };

    // Operation 3: adopt git HEAD as the working-copy position. The files are
    // already on disk (git put them there), so the working copy gets `reset`
    // (state only), not `check_out` (which would rewrite every file).
    ex.phase_transact();
    let mut tx = repo.start_transaction();
    git::import_head(tx.repo_mut(), &ws_name, &root).block_on()?;
    let head_target = tx.repo().view().git_head(&ws_name).as_normal().cloned();
    let repo = if let Some(head_id) = head_target {
        ex.note(&format!(
            "import_head: .git/HEAD ({}) → jj view (git_head); nothing written to .git",
            short(&head_id.hex())
        ));
        let head_commit = tx.repo().store().get_commit(&head_id)?;
        let wc_commit = tx
            .repo_mut()
            .check_out(ws_name.clone(), &head_commit)
            .block_on()?;
        tx.repo_mut().rebase_descendants().block_on()?;
        // The view's git_head is what import_head just recorded, so reset_head
        // sees parent(@) == HEAD and leaves .git/HEAD alone (still attached to
        // whatever branch git had). Only the index gets rebuilt.
        let old_git_head = tx.repo().view().git_head(&ws_name).clone();
        let root_id = tx.repo().store().root_commit_id().clone();
        git::reset_head(tx.repo_mut(), &ws_name, &root, &wc_commit).block_on()?;
        ex.reset_head(&old_git_head, &wc_commit.parent_ids()[0], &root_id);
        let repo = tx.commit("jj-gt init: import git head").block_on()?;
        ex.op_log("jj-gt init: import git head", &repo.op_id().hex());
        ex.phase_finalize();
        let mut locked_ws = workspace.start_working_copy_mutation().block_on()?;
        locked_ws.locked_wc().reset(&wc_commit).block_on()?;
        ex.note(
            "no `working copy` write: git already put HEAD's tree on disk, so \
             LockedWorkingCopy::reset only re-points .jj/working_copy at @'s tree \
             (files get re-stat'ed on the next snapshot)",
        );
        locked_ws.finish(repo.op_id().clone()).block_on()?;
        ex.workspace_state(&repo.op_id().hex());
        repo
    } else {
        // Repo with no commits yet: nothing to adopt.
        ex.note(
            "git HEAD is unborn — nothing to adopt, transaction dropped (no operation published)",
        );
        drop(tx);
        repo
    };

    // Figure out trunk: origin/HEAD, else common defaults, else current head.
    let trunk = git_output(&root, &["symbolic-ref", "refs/remotes/origin/HEAD"])
        .ok()
        .and_then(|s| s.strip_prefix("refs/remotes/origin/").map(str::to_string))
        .or_else(|| {
            ["main", "master"].into_iter().find_map(|name| {
                repo.view()
                    .get_local_bookmark(jj_lib::ref_name::RefName::new(name))
                    .is_present()
                    .then(|| name.to_string())
            })
        })
        .unwrap_or_else(|| "main".to_string());

    // An empty repo (unborn HEAD) has no trunk bookmark yet — bootstrap one so
    // every later command's trunk_id() resolves.
    let trunk_ref = jj_lib::ref_name::RefName::new(&trunk);
    let repo = if !repo.view().get_local_bookmark(trunk_ref).is_present() {
        ex.phase_transact();
        ex.note(&format!("no {trunk} bookmark yet (empty repo) — creating an initial commit"));
        let mut tx = repo.start_transaction();
        let root_id = tx.repo().store().root_commit_id().clone();
        let root_commit = tx.repo().store().get_commit(&root_id)?;
        let initial = tx
            .repo_mut()
            .new_commit(vec![root_id.clone()], root_commit.tree())
            .set_description("initial commit")
            .write()
            .block_on()?;
        tx.repo_mut().set_local_bookmark_target(
            trunk_ref,
            jj_lib::op_store::RefTarget::normal(initial.id().clone()),
        );
        let new_wc = tx.repo_mut().check_out(ws_name.clone(), &initial).block_on()?;
        tx.repo_mut().rebase_descendants().block_on()?;
        let old_git_head = tx.repo().view().git_head(&ws_name).clone();
        git::reset_head(tx.repo_mut(), &ws_name, &root, &new_wc).block_on()?;
        ex.reset_head(&old_git_head, &new_wc.parent_ids()[0], &root_id);
        export_refs(tx.repo_mut(), ex)?;
        let repo = tx.commit("jj-gt init: create trunk").block_on()?;
        ex.op_log("jj-gt init: create trunk", &repo.op_id().hex());
        ex.phase_finalize();
        let stats = workspace
            .check_out(repo.op_id().clone(), None, &new_wc)
            .block_on()?;
        ex.working_copy(stats.added_files, stats.updated_files, stats.removed_files);
        ex.workspace_state(&repo.op_id().hex());
        repo
    } else {
        repo
    };

    let state = GtState {
        trunk: trunk.clone(),
        remote: "origin".to_string(),
        prs: Default::default(),
    };
    state.save(&root)?;
    println!("Initialized colocated jj+git repo (trunk: {trunk})");
    println!("op log so far:");
    crate::commands::ops::print_ops(&repo, 5)?;
    Ok(())
}
