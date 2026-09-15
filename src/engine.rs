//! The "three writes" engine.
//!
//! jj-lib's Transaction::commit writes ONLY the op log (op store + op heads +
//! index). Everything else the jj CLI quietly does around every command is our
//! job here:
//!
//!   write 0  snapshot: fold dirty files into @ (its own operation)
//!   write 1  working-copy files on disk        (LockedWorkingCopy::check_out)
//!   write 2  workspace state (.jj/working_copy) (LockedWorkspace::finish)
//!   write 3  colocated git refs + HEAD + index (git::export_refs / reset_head)
//!
//! Order is load-bearing: reset_head + export_refs run on tx.repo_mut()
//! BEFORE tx.commit (so the exported state is recorded in the same operation);
//! the working copy is updated AFTER tx.commit (finish() needs the new op id).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use jj_lib::backend::CommitId;
use jj_lib::commit::Commit;
use jj_lib::git;
use jj_lib::gitignore::GitIgnoreFile;
use jj_lib::matchers::{EverythingMatcher, NothingMatcher};
use jj_lib::object_id::ObjectId;
use jj_lib::op_store::RefTarget;
use jj_lib::ref_name::{RefName, RemoteName, WorkspaceNameBuf};
use jj_lib::repo::{ReadonlyRepo, Repo as _};
use jj_lib::settings::UserSettings;
use jj_lib::transaction::Transaction;
use jj_lib::working_copy::{SnapshotOptions, WorkingCopyFreshness};
use jj_lib::workspace::Workspace;
use pollster::FutureExt as _;

use crate::explain::Explain;
use crate::state::GtState;
use crate::util::short;

/// Operation-metadata attribute stamping every op a jj-gt command creates, so
/// `jj-gt undo` can roll back a whole command (snapshot included) as one group.
pub const CMD_ID_ATTR: &str = "gt-command-id";

pub struct Gt {
    pub workspace: Workspace,
    pub repo: Arc<ReadonlyRepo>,
    pub settings: UserSettings,
    pub state: GtState,
    pub explain: Explain,
    pub root: PathBuf,
    /// Unique id for this jj-gt invocation; stamped on every operation it commits.
    pub cmd_id: String,
}

/// Walk up from `cwd` to find the workspace root (the dir containing .jj).
pub fn find_workspace_root(cwd: &Path) -> Result<PathBuf> {
    let mut dir = cwd;
    loop {
        if dir.join(".jj").is_dir() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => bail!("no .jj repo found above {} — run `jj-gt init` first", cwd.display()),
        }
    }
}

impl Gt {
    pub fn load(cwd: &Path, explain: Explain) -> Result<Self> {
        let root = find_workspace_root(cwd)?;
        let settings = crate::settings::user_settings()?;
        let workspace = Workspace::load(
            &settings,
            &root,
            &jj_lib::default_backend_factories::default_backend_factories(),
            &jj_lib::default_backend_factories::default_working_copy_factories(),
        )
        .with_context(|| format!("failed to load jj workspace at {}", root.display()))?;
        // load_at_head also reconciles divergent op heads (concurrent writers)
        // by committing a merge operation — an op-log write most people never
        // know happens.
        let repo = workspace.repo_loader().load_at_head().block_on()?;
        let state = GtState::load(&root)?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let cmd_id = format!("{}-{nanos}", std::process::id());
        Ok(Gt {
            workspace,
            repo,
            settings,
            state,
            explain,
            root,
            cmd_id,
        })
    }

    /// Start a transaction stamped with this command's id (see CMD_ID_ATTR).
    pub fn start_tx(&self) -> Transaction {
        let mut tx = self.repo.start_transaction();
        tx.set_attribute(CMD_ID_ATTR.to_string(), self.cmd_id.clone());
        tx
    }

    pub fn ws_name(&self) -> WorkspaceNameBuf {
        self.workspace.workspace_name().to_owned()
    }

    pub fn trunk(&self) -> &RefName {
        RefName::new(&self.state.trunk)
    }

    pub fn remote(&self) -> &RemoteName {
        RemoteName::new(&self.state.remote)
    }

    pub fn wc_commit(&self) -> Result<Commit> {
        let name = self.ws_name();
        let id = self
            .repo
            .view()
            .get_wc_commit_id(&name)
            .ok_or_else(|| anyhow!("workspace has no working-copy commit"))?;
        Ok(self.repo.store().get_commit(id)?)
    }

    pub fn trunk_id(&self) -> Result<CommitId> {
        let target = self.repo.view().get_local_bookmark(self.trunk());
        target
            .as_normal()
            .cloned()
            .ok_or_else(|| anyhow!("trunk bookmark {:?} is missing or conflicted", self.state.trunk))
    }

    /// Import options mirroring init: auto-track everything on the remote.
    fn import_options(&self) -> git::GitImportOptions {
        let mut auto_track = std::collections::HashMap::new();
        auto_track.insert(
            jj_lib::ref_name::RemoteNameBuf::from(self.state.remote.as_str()),
            jj_lib::str_util::StringExpression::all().to_matcher(),
        );
        git::GitImportOptions {
            abandon_unreachable_commits: false,
            record_synthetic_predecessors: false,
            remote_auto_track_bookmarks: auto_track,
        }
    }

    /// Adopt writes made to the colocated .git by OTHER tools (raw git, IDEs):
    /// import HEAD before snapshotting — if git moved HEAD, the files on disk
    /// already match it, so the working copy gets `reset` (state only).
    /// Without this, an external `git commit` would be silently re-folded into
    /// the stale @ and HEAD sync would wedge forever. The jj CLI does exactly
    /// this at the start of every colocated command.
    fn import_git_head(&mut self) -> Result<()> {
        let ex = self.explain;
        let name = self.ws_name();
        let mut tx = self.start_tx();
        git::import_head(tx.repo_mut(), &name, &self.root).block_on()?;
        if !tx.repo().has_changes() {
            return Ok(());
        }
        ex.note("git HEAD moved outside jj-gt — adopting it (import_head)");
        if let Some(head_id) = tx.repo().view().git_head(&name).as_normal().cloned() {
            let head_commit = tx.repo().store().get_commit(&head_id)?;
            let new_wc = tx
                .repo_mut()
                .check_out(name.clone(), &head_commit)
                .block_on()?;
            tx.repo_mut().rebase_descendants().block_on()?;
            let mut locked_ws = self.workspace.start_working_copy_mutation().block_on()?;
            locked_ws.locked_wc().reset(&new_wc).block_on()?;
            self.repo = tx.commit("jj-gt: import git head").block_on()?;
            ex.op_log("jj-gt: import git head", &self.repo.op_id().hex());
            locked_ws.finish(self.repo.op_id().clone()).block_on()?;
            ex.workspace_state(&self.repo.op_id().hex());
        } else {
            self.repo = tx.commit("jj-gt: import git head").block_on()?;
            ex.op_log("jj-gt: import git head", &self.repo.op_id().hex());
        }
        Ok(())
    }

    /// Import git refs after snapshotting — branches moved by other tools
    /// become visible to jj (and may rebase @, hence the full epilogue).
    fn import_git_refs(&mut self) -> Result<()> {
        let mut tx = self.start_tx();
        let stats = git::import_refs(tx.repo_mut(), &self.import_options()).block_on()?;
        if !tx.repo().has_changes() {
            return Ok(());
        }
        self.explain.note(&format!(
            "git refs moved outside jj-gt — imported {} bookmark change(s)",
            stats.changed_remote_bookmarks.len()
        ));
        tx.repo_mut().rebase_descendants().block_on()?;
        self.finish_tx(tx, "jj-gt: import git refs")?;
        Ok(())
    }

    /// Base ignores for snapshotting, like the jj CLI builds them: the user's
    /// global gitignore plus .git/info/exclude (in-tree .gitignore files are
    /// handled by the snapshot walker itself).
    fn base_ignores(&self) -> Result<std::sync::Arc<GitIgnoreFile>> {
        use jj_lib::repo_path::RepoPath;
        let mut ignores = GitIgnoreFile::empty();
        let global = crate::util::git_output(&self.root, &["config", "--path", "--get", "core.excludesFile"])
            .ok()
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| std::path::Path::new(&home).join(".config/git/ignore"))
            });
        if let Some(path) = global {
            ignores = ignores.chain_with_file(RepoPath::root(), path)?;
        }
        ignores = ignores.chain_with_file(RepoPath::root(), self.root.join(".git/info/exclude"))?;
        Ok(ignores)
    }

    /// Write 0: snapshot the working copy — what the jj CLI does implicitly at
    /// the start of every command. For a colocated repo that includes ADOPTING
    /// git's own writes (import HEAD before, import refs after), not just
    /// snapshotting files. Always advances the workspace state to the latest op.
    pub fn snapshot(&mut self) -> Result<()> {
        let ex = self.explain;
        ex.section("snapshot working copy (what `jj` does before every command)");
        self.import_git_head()?;
        let name = self.ws_name();
        let base_ignores = self.base_ignores()?;
        let mut locked_ws = self.workspace.start_working_copy_mutation().block_on()?;
        let mut wc_commit = {
            let id = self
                .repo
                .view()
                .get_wc_commit_id(&name)
                .ok_or_else(|| anyhow!("workspace has no working-copy commit"))?;
            self.repo.store().get_commit(id)?
        };
        match WorkingCopyFreshness::check_stale(locked_ws.locked_wc(), &wc_commit, &self.repo)
            .block_on()?
        {
            WorkingCopyFreshness::Fresh => {}
            WorkingCopyFreshness::Updated(op) => {
                // The repo we loaded is older than the working copy: reload.
                ex.note("repo loaded at an older op than the working copy — reloading");
                self.repo = self.repo.reload_at(&op).block_on()?;
                let id = self
                    .repo
                    .view()
                    .get_wc_commit_id(&name)
                    .ok_or_else(|| anyhow!("workspace has no working-copy commit"))?;
                wc_commit = self.repo.store().get_commit(id)?;
            }
            WorkingCopyFreshness::WorkingCopyStale => {
                // The working copy was left behind by an op-log write that
                // never did writes 1+2. Healing by check_out is only safe if
                // the disk has no unsnapshotted edits — check_out would erase
                // them. Compare the stale op's wc tree with the recorded tree.
                ex.note("STALE working copy detected (op log moved without writes 1+2)");
                let old_op_id = locked_ws.locked_wc().old_operation_id().clone();
                let old_op = self.repo.loader().load_operation(&old_op_id).block_on()?;
                let old_repo = self.repo.loader().load_at(&old_op).block_on()?;
                let stale_commit = old_repo
                    .view()
                    .get_wc_commit_id(&name)
                    .map(|id| old_repo.store().get_commit(id))
                    .transpose()?;
                let clean = stale_commit.is_some_and(|c| {
                    c.tree().tree_ids_and_labels()
                        == locked_ws.locked_wc().old_tree().tree_ids_and_labels()
                });
                if !clean {
                    bail!(
                        "working copy is stale AND has unsnapshotted changes — \
                         run `jj workspace update-stale` to recover"
                    );
                }
                let stats = locked_ws.locked_wc().check_out(&wc_commit).block_on()?;
                ex.working_copy(stats.added_files, stats.updated_files, stats.removed_files);
                eprintln!(
                    "jj-gt: healed stale working copy (now at op {})",
                    short(&self.repo.op_id().hex())
                );
            }
            WorkingCopyFreshness::SiblingOperation => {
                bail!("working copy belongs to a sibling operation — resolve with `jj` first");
            }
        }
        let options = SnapshotOptions {
            base_ignores,
            progress: None,
            start_tracking_matcher: &EverythingMatcher,
            force_tracking_matcher: &NothingMatcher,
            max_new_file_size: 8 << 20, // 8 MiB — finite, demo-friendly
        };
        let (new_tree, snapshot_stats) = locked_ws.locked_wc().snapshot(&options).block_on()?;
        for (path, reason) in &snapshot_stats.untracked_paths {
            eprintln!("jj-gt: warning: not tracking {}: {reason:?}", path.as_internal_file_string());
        }
        if new_tree.tree_ids_and_labels() != wc_commit.tree().tree_ids_and_labels() {
            ex.note("working copy differs from @ — folding files into the wc commit");
            // Field-precise borrow (locked_ws holds &mut self.workspace, so
            // the whole-self start_tx() helper can't be used here).
            let mut tx = self.repo.start_transaction();
            tx.set_attribute(CMD_ID_ATTR.to_string(), self.cmd_id.clone());
            tx.set_is_snapshot(true);
            let new_wc = tx
                .repo_mut()
                .rewrite_commit(&wc_commit)
                .set_tree(new_tree)
                .write()
                .block_on()?;
            tx.repo_mut()
                .set_wc_commit(name.clone(), new_wc.id().clone())?;
            tx.repo_mut().rebase_descendants().block_on()?;
            // Keep `git status` in the colocated repo honest about wc changes.
            git::update_intent_to_add(tx.repo(), &self.root, &wc_commit.tree(), &new_wc.tree())
                .block_on()?;
            ex.git_refs(".git/index ← intent-to-add entries for files new in @ (update_intent_to_add)");
            let export_stats = git::export_refs(tx.repo_mut())?;
            report_export(&export_stats);
            ex.git_refs(".git/refs/heads/* now mirror jj bookmarks (export_refs)");
            self.repo = tx.commit("jj-gt: snapshot working copy").block_on()?;
            ex.op_log("jj-gt: snapshot working copy", &self.repo.op_id().hex());
        } else {
            ex.note("working copy clean — no snapshot operation needed");
        }
        // Always finish, even when nothing changed: this is write 2, and it
        // also persists the file-stat cache.
        locked_ws.finish(self.repo.op_id().clone()).block_on()?;
        ex.workspace_state(&self.repo.op_id().hex());
        // Refs moved by other tools import AFTER the snapshot (they can
        // rebase @, so this goes through the full epilogue).
        self.import_git_refs()?;
        Ok(())
    }

    /// The epilogue every mutating jj-gt command shares — the jj CLI's
    /// finish_transaction, reimplemented. Returns false if the transaction had
    /// no changes (and was dropped).
    pub fn finish_tx(&mut self, mut tx: Transaction, desc: &str) -> Result<bool> {
        let ex = self.explain;
        if !tx.repo().has_changes() {
            println!("Nothing changed.");
            return Ok(false);
        }
        ex.section(&format!("writes — {desc}"));
        tx.repo_mut().rebase_descendants().block_on()?;

        let name = self.ws_name();
        let old_wc_commit = tx
            .base_repo()
            .view()
            .get_wc_commit_id(&name)
            .map(|id| tx.base_repo().store().get_commit(id))
            .transpose()?;
        let new_wc_commit = tx
            .repo()
            .view()
            .get_wc_commit_id(&name)
            .map(|id| tx.repo().store().get_commit(id))
            .transpose()?;

        // ── write 3: colocated git (HEAD + index + refs), INSIDE the tx ──
        if let Some(new_wc) = &new_wc_commit {
            // reset_head only rewrites .git/HEAD when the view's record of it
            // differs from parent(@); capture the record first so --explain
            // can say which case this was.
            let old_git_head = tx.repo().view().git_head(&name).clone();
            let root_id = tx.repo().store().root_commit_id().clone();
            match git::reset_head(tx.repo_mut(), &name, &self.root, new_wc).block_on() {
                Ok(()) => ex.reset_head(&old_git_head, &new_wc.parent_ids()[0], &root_id),
                Err(git::GitResetHeadError::UpdateHeadRef(e)) => {
                    eprintln!("jj-gt: warning: git HEAD moved concurrently, not resetting it: {e}");
                }
                Err(e) => return Err(e.into()),
            }
        }
        let export_stats = git::export_refs(tx.repo_mut())?;
        report_export(&export_stats);
        ex.git_refs(".git/refs/heads/* now mirror jj bookmarks (export_refs)");

        // ── the op-log write (the only one Transaction::commit performs) ──
        let repo = tx.commit(desc).block_on()?;
        ex.op_log(desc, &repo.op_id().hex());
        self.repo = repo;

        // ── writes 1 + 2: files on disk + workspace state, AFTER the commit ──
        if let Some(new_wc) = &new_wc_commit {
            let old_tree = old_wc_commit.as_ref().map(|c| c.tree());
            let stats = self
                .workspace
                .check_out(self.repo.op_id().clone(), old_tree.as_ref(), new_wc)
                .block_on()?;
            ex.working_copy(stats.added_files, stats.updated_files, stats.removed_files);
            ex.workspace_state(&self.repo.op_id().hex());
            if old_wc_commit.as_ref().map(|c| c.id()) != Some(new_wc.id()) {
                println!(
                    "Working copy now at: {} {}",
                    short(&new_wc.id().hex()),
                    summarize(new_wc)
                );
            }
        } else {
            // No working-copy commit changed hands (e.g. workspace-less repo);
            // still advance the recorded op so the workspace isn't stale.
            let locked_ws = self.workspace.start_working_copy_mutation().block_on()?;
            locked_ws.finish(self.repo.op_id().clone()).block_on()?;
            ex.workspace_state(&self.repo.op_id().hex());
        }
        Ok(true)
    }

    /// Resolve a bookmark: local first, then remote (origin, then the "git"
    /// pseudo-remote colocated git branches live on).
    pub fn resolve_bookmark(&self, name: &str) -> Result<CommitId> {
        let ref_name = RefName::new(name);
        let target = self.repo.view().get_local_bookmark(ref_name);
        if target.has_conflict() {
            bail!("bookmark {name:?} is conflicted — resolve it with jj first");
        }
        if let Some(id) = target.as_normal() {
            return Ok(id.clone());
        }
        for remote in [self.state.remote.as_str(), "git"] {
            let symbol = ref_name.to_remote_symbol(RemoteName::new(remote));
            let remote_ref = self.repo.view().get_remote_bookmark(symbol);
            if let Some(id) = remote_ref.target.as_normal() {
                return Ok(id.clone());
            }
        }
        bail!("no bookmark named {name:?} (locally or on {})", self.state.remote)
    }

    /// Delete a local bookmark in the current transaction (RefTarget::absent
    /// is jj's deletion sentinel; export_refs then removes refs/heads/<name>).
    pub fn delete_bookmark(tx: &mut Transaction, name: &RefName) {
        tx.repo_mut()
            .set_local_bookmark_target(name, RefTarget::absent());
    }
}

fn report_export(stats: &git::GitExportStats) {
    for (symbol, reason) in &stats.failed_bookmarks {
        eprintln!("jj-gt: warning: could not export {symbol} to git: {reason:?}");
    }
}

pub fn summarize(commit: &Commit) -> String {
    let desc = commit.description().lines().next().unwrap_or("");
    if desc.is_empty() {
        "(no description set)".to_string()
    } else {
        desc.to_string()
    }
}
