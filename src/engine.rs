//! The "three writes" engine — now driven by jj-lib's `WorkspaceOperationRunner`.
//!
//! jj-lib's Transaction::commit writes ONLY the op log (op store + op heads +
//! index). Everything else the jj CLI quietly does around every command used to
//! be reimplemented here:
//!
//!   write 0  snapshot: fold dirty files into @ (its own operation)
//!   write 1  working-copy files on disk        (LockedWorkingCopy::check_out)
//!   write 2  workspace state (.jj/working_copy) (LockedWorkspace::finish)
//!   write 3  colocated git refs + HEAD + index (git::export_refs / reset_head)
//!
//! With jj-vcs/jj PR #9300 those writes live in the library:
//! `WorkspaceOperationRunner::{import_git_head, snapshot_working_copy,
//! import_git_refs, finish_transaction}` perform them in the right order and
//! hand back a state struct describing what happened. jj-gt's job shrinks to
//! configuring the runner (settings, snapshot options, import options) and
//! narrating the returned state for `--explain`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use jj_lib::backend::CommitId;
use jj_lib::commit::Commit;
use jj_lib::git;
use jj_lib::gitignore::GitIgnoreFile;
use jj_lib::matchers::{EverythingMatcher, NothingMatcher};
use jj_lib::object_id::ObjectId;
use jj_lib::op_store::{OperationId, RefTarget};
use jj_lib::readonly_user_repo::ReadonlyUserRepo;
use jj_lib::ref_name::{RefName, RemoteName, WorkspaceNameBuf};
use jj_lib::repo::{ReadonlyRepo, Repo as _};
use jj_lib::revset::RevsetExtensions;
use jj_lib::settings::UserSettings;
use jj_lib::transaction::Transaction;
use jj_lib::user_error::{ErrorHint, UserError};
use jj_lib::working_copy::SnapshotOptions;
use jj_lib::workspace::Workspace;
use jj_lib::workspace_operation_runner::{WorkspaceOperationError, WorkspaceOperationRunner};
use jj_lib::workspace_util::WorkspaceEnvironment;
use pollster::FutureExt as _;

use crate::explain::Explain;
use crate::state::GtState;
use crate::util::short;

/// Operation-metadata attribute stamping every op a jj-gt command creates, so
/// `jj-gt undo` can roll back a whole command as one group.
///
/// Only the transactions jj-gt starts itself carry it: the runner creates the
/// snapshot / git-import operations internally and offers no hook for extra
/// attributes, so `undo` recognises those by their `is_snapshot` flag and
/// fixed descriptions instead (see `commands::undo`).
pub const CMD_ID_ATTR: &str = "gt-command-id";

/// Descriptions the runner hard-codes for the operations it creates on its own.
pub const RUNNER_OP_DESCRIPTIONS: &[&str] =
    &["snapshot working copy", "import git head", "import git refs"];

/// The CLI knobs the runner's methods take as plain parameters. jj-gt always
/// runs as the head operation, always updates the working copy, always
/// publishes, and has no `--ignore-immutable`.
const MAY_UPDATE_WORKING_COPY: bool = true;
const WORKING_COPY_SHARED_WITH_GIT: bool = true;
const SHOULD_PUBLISH: bool = true;
const IGNORE_IMMUTABLE: bool = false;

pub struct Gt {
    /// Owns the `Workspace`, the loaded repo, and the `WorkspaceEnvironment`.
    runner: WorkspaceOperationRunner,
    pub state: GtState,
    pub explain: Explain,
    pub root: PathBuf,
    /// Unique id for this jj-gt invocation; stamped on every operation it commits.
    pub cmd_id: String,
    /// The operation head when this invocation loaded the repo, i.e. before
    /// any of its own snapshot/import operations. `undo` uses it to skip them.
    pub initial_op_id: OperationId,
    /// argv, recorded by the runner in every operation's `args` attribute.
    args: Vec<String>,
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

/// jj-lib's `UserError` is a plain struct (no `std::error::Error` impl), so it
/// can't ride `?` into anyhow directly. Flatten it, hints included.
pub fn anyhow_from_user_error(err: UserError) -> anyhow::Error {
    let mut msg = err.error.to_string();
    for hint in &err.hints {
        match hint {
            ErrorHint::PlainText(text) => {
                msg.push_str("\nhint: ");
                msg.push_str(text);
            }
            ErrorHint::Formatted(_) => {}
        }
    }
    anyhow!(msg)
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
        let initial_op_id = repo.op_id().clone();

        // The environment reads revset/fileset aliases and UI settings from the
        // workspace's settings. We deliberately don't call
        // `reload_revset_expressions`, which would need the CLI's
        // `immutable_heads()` alias; without it only root() is immutable,
        // matching jj-gt's previous behaviour of rebasing everything.
        let env = WorkspaceEnvironment::new(
            &workspace,
            cwd.to_path_buf(),
            Arc::new(RevsetExtensions::default()),
            |args| {
                eprintln!("jj-gt: warning: {args}");
                Ok(())
            },
        )
        .map_err(anyhow_from_user_error)?;
        let runner = WorkspaceOperationRunner::new(env, workspace, ReadonlyUserRepo::new(repo));

        let state = GtState::load(&root)?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let cmd_id = format!("{}-{nanos}", std::process::id());
        Ok(Gt {
            runner,
            state,
            explain,
            root,
            cmd_id,
            initial_op_id,
            args: std::env::args().collect(),
        })
    }

    /// The repo as of the last operation this invocation committed.
    pub fn repo(&self) -> &Arc<ReadonlyRepo> {
        self.runner.user_repo().repo()
    }

    pub fn settings(&self) -> &UserSettings {
        self.runner.settings()
    }

    /// Start a transaction stamped with this command's id (see CMD_ID_ATTR).
    /// The runner records argv as the `args` attribute, like the jj CLI.
    pub fn start_tx(&mut self) -> Transaction {
        let mut tx = self.runner.start_transaction(&self.args).into_inner();
        tx.set_attribute(CMD_ID_ATTR.to_string(), self.cmd_id.clone());
        tx
    }

    pub fn ws_name(&self) -> WorkspaceNameBuf {
        self.runner.workspace_name().to_owned()
    }

    pub fn trunk(&self) -> &RefName {
        RefName::new(&self.state.trunk)
    }

    pub fn remote(&self) -> &RemoteName {
        RemoteName::new(&self.state.remote)
    }

    pub fn wc_commit(&self) -> Result<Commit> {
        let id = self
            .runner
            .get_wc_commit_id()
            .ok_or_else(|| anyhow!("workspace has no working-copy commit"))?;
        Ok(self.repo().store().get_commit(id)?)
    }

    pub fn trunk_id(&self) -> Result<CommitId> {
        let target = self.repo().view().get_local_bookmark(self.trunk());
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
    /// snapshotting files. The three steps are the runner's; jj-gt only picks
    /// the options and narrates the returned state.
    pub fn snapshot(&mut self) -> Result<()> {
        let ex = self.explain;
        ex.section("snapshot working copy (what `jj` does before every command)");

        // Adopt writes made to the colocated .git by OTHER tools (raw git,
        // IDEs): if git moved HEAD, the files on disk already match it, so the
        // runner `reset`s the working copy (state only) to the new HEAD.
        let before = self.repo().op_id().clone();
        if let Some(old_head_present) = self
            .runner
            .import_git_head(
                &self.args,
                MAY_UPDATE_WORKING_COPY,
                WORKING_COPY_SHARED_WITH_GIT,
                SHOULD_PUBLISH,
                IGNORE_IMMUTABLE,
            )
            .block_on()?
        {
            ex.note(if old_head_present {
                "git HEAD moved outside jj-gt — adopting it (import_head)"
            } else {
                "git HEAD appeared — adopting it (import_head)"
            });
            self.narrate_new_op(&before, "import git head");
            ex.workspace_state(&self.repo().op_id().hex());
        }

        let base_ignores = self.base_ignores()?;
        let options = SnapshotOptions {
            base_ignores,
            progress: None,
            start_tracking_matcher: &EverythingMatcher,
            force_tracking_matcher: &NothingMatcher,
            max_new_file_size: 8 << 20, // 8 MiB — finite, demo-friendly
        };
        let before = self.repo().op_id().clone();
        let state = match self.snapshot_working_copy(&options) {
            Ok(state) => state,
            Err(WorkspaceOperationError::StaleWorkingCopy(stale_op_id)) => {
                // The runner refuses to snapshot a stale working copy (the jj
                // CLI sends users to `jj workspace update-stale`). jj-gt heals
                // it itself when that is provably safe, then retries.
                self.heal_stale_working_copy(&stale_op_id)?;
                self.snapshot_working_copy(&options)?
            }
            Err(err) => return Err(err.into()),
        };
        for (path, reason) in &state.stats.untracked_paths {
            eprintln!("jj-gt: warning: not tracking {}: {reason:?}", path.as_internal_file_string());
        }
        if state.committed_operation {
            ex.note("working copy differed from @ — folded files into the wc commit");
            if state.num_rebased > 0 {
                ex.note(&format!("rebased {} descendant(s) onto the updated @", state.num_rebased));
            }
            if let Some(stats) = &state.git_export_stats {
                report_export(stats);
                ex.git_refs("index updated (intent-to-add) and refs exported (export_refs)");
            }
            self.narrate_new_op(&before, "snapshot working copy");
        } else {
            ex.note("working copy clean — no snapshot operation needed");
        }
        if let Some(err) = &state.git_reset_err {
            eprintln!("jj-gt: warning: git HEAD moved concurrently, not resetting it: {err}");
        }
        // Write 2 happened inside the runner even when nothing changed (it
        // also persists the file-stat cache).
        ex.workspace_state(&self.repo().op_id().hex());

        // Refs moved by other tools import AFTER the snapshot (they can
        // rebase @, so the runner runs its full finish_transaction epilogue).
        let before = self.repo().op_id().clone();
        if let Some(imported) = self
            .runner
            .import_git_refs(
                &self.args,
                &self.import_options(),
                MAY_UPDATE_WORKING_COPY,
                WORKING_COPY_SHARED_WITH_GIT,
                SHOULD_PUBLISH,
                IGNORE_IMMUTABLE,
            )
            .block_on()?
        {
            ex.note(&format!(
                "git refs moved outside jj-gt — imported {} bookmark change(s), rebased {} commit(s)",
                imported.import_stats.changed_remote_bookmarks.len(),
                imported.num_rebased
            ));
            self.narrate_new_op(&before, "import git refs");
            ex.workspace_state(&self.repo().op_id().hex());
        }
        Ok(())
    }

    fn snapshot_working_copy(
        &mut self,
        options: &SnapshotOptions<'_>,
    ) -> Result<jj_lib::workspace_operation_runner::SnapshotState, WorkspaceOperationError> {
        self.runner
            .snapshot_working_copy(
                options,
                &self.args,
                WORKING_COPY_SHARED_WITH_GIT,
                SHOULD_PUBLISH,
                IGNORE_IMMUTABLE,
            )
            .block_on()
    }

    /// The working copy was left behind by an op-log write that never did
    /// writes 1+2. Healing by check_out is only safe if the disk has no
    /// unsnapshotted edits — check_out would erase them. Compare the stale
    /// op's wc tree with the recorded tree before touching anything.
    fn heal_stale_working_copy(&mut self, stale_op_id: &OperationId) -> Result<()> {
        let ex = self.explain;
        ex.note("STALE working copy detected (op log moved without writes 1+2)");
        let name = self.ws_name();
        let wc_commit = self.wc_commit()?;
        let repo = self.repo().clone();
        let old_op = repo.loader().load_operation(stale_op_id).block_on()?;
        let old_repo = repo.loader().load_at(&old_op).block_on()?;
        let stale_commit = old_repo
            .view()
            .get_wc_commit_id(&name)
            .map(|id| old_repo.store().get_commit(id))
            .transpose()?;
        let mut locked_ws = self
            .runner
            .workspace_mut()
            .start_working_copy_mutation()
            .block_on()?;
        let clean = stale_commit.is_some_and(|c| {
            c.tree().tree_ids_and_labels() == locked_ws.locked_wc().old_tree().tree_ids_and_labels()
        });
        if !clean {
            bail!(
                "working copy is stale AND has unsnapshotted changes — \
                 run `jj workspace update-stale` to recover"
            );
        }
        let stats = locked_ws.locked_wc().check_out(&wc_commit).block_on()?;
        ex.working_copy(stats.added_files, stats.updated_files, stats.removed_files);
        locked_ws.finish(repo.op_id().clone()).block_on()?;
        ex.workspace_state(&repo.op_id().hex());
        eprintln!("jj-gt: healed stale working copy (now at op {})", short(&repo.op_id().hex()));
        Ok(())
    }

    /// Narrate the op-log write if the runner committed a new operation since
    /// `before`. The runner picks the description; we can only observe it.
    fn narrate_new_op(&self, before: &OperationId, desc: &str) {
        let now = self.repo().op_id();
        if now != before {
            self.explain.op_log(desc, &now.hex());
        }
    }

    /// The epilogue every mutating jj-gt command shares — the jj CLI's
    /// finish_transaction, now `WorkspaceOperationRunner::finish_transaction`.
    /// Returns false if the transaction had no changes (and was dropped).
    pub fn finish_tx(&mut self, mut tx: Transaction, desc: &str) -> Result<bool> {
        let ex = self.explain;
        if !tx.repo().has_changes() {
            println!("Nothing changed.");
            return Ok(false);
        }
        ex.section(&format!("writes — {desc}"));
        // Like WorkspaceCommandTransaction::finish: rebase descendants first,
        // leaving immutable ones (only root(), for jj-gt) alone.
        let num_rebased = self
            .runner
            .rebase_mutable_descendants(&mut tx, IGNORE_IMMUTABLE)
            .block_on()?;
        if num_rebased > 0 {
            ex.note(&format!("rebased {num_rebased} descendant commit(s)"));
        }

        // The runner does, in order: reset git HEAD + export refs INSIDE the
        // tx (write 3), commit the op (the op-log write), then check out the
        // new @ (writes 1 + 2). It reports back what it did.
        let (state, _old_repo) = self
            .runner
            .finish_transaction(
                tx,
                desc,
                MAY_UPDATE_WORKING_COPY,
                WORKING_COPY_SHARED_WITH_GIT,
                SHOULD_PUBLISH,
                IGNORE_IMMUTABLE,
            )
            .block_on()?;

        if let Some(new_wc) = &state.maybe_new_wc_commit {
            match &state.git_reset_err {
                None => {
                    let parent = new_wc
                        .parent_ids()
                        .first()
                        .map(|id| short(&id.hex()))
                        .unwrap_or_default();
                    ex.git_refs(&format!(".git HEAD ⇒ detached at parent of @ ({parent}); index rebuilt"));
                }
                Some(err) => {
                    eprintln!("jj-gt: warning: git HEAD moved concurrently, not resetting it: {err}");
                }
            }
        }
        if let Some(stats) = &state.git_export_stats {
            report_export(stats);
            ex.git_refs(".git/refs/heads/* now mirror jj bookmarks (export_refs)");
        }
        if state.moved_off_immutable {
            eprintln!("jj-gt: the working-copy commit became immutable; a new commit was created on top");
        }

        ex.op_log(desc, &self.repo().op_id().hex());

        if let Some(new_wc) = &state.maybe_new_wc_commit {
            if let Some(stats) = &state.checkout_stats {
                ex.working_copy(stats.added_files, stats.updated_files, stats.removed_files);
                ex.workspace_state(&self.repo().op_id().hex());
            }
            if state.maybe_old_wc_commit.as_ref().map(|c| c.id()) != Some(new_wc.id()) {
                println!(
                    "Working copy now at: {} {}",
                    short(&new_wc.id().hex()),
                    summarize(new_wc)
                );
            }
        } else {
            // No working-copy commit changed hands (e.g. workspace-less repo).
            // Unlike the old hand-rolled epilogue, the runner (like the jj
            // CLI) leaves the workspace state alone in this case.
            ex.note("no working-copy commit for this workspace — files and workspace state untouched");
        }
        if state.missing_user_name || state.missing_user_mail {
            eprintln!("jj-gt: warning: user.name/user.email not configured; commits use an empty identity");
        }
        Ok(true)
    }

    /// Resolve a bookmark: local first, then remote (origin, then the "git"
    /// pseudo-remote colocated git branches live on).
    pub fn resolve_bookmark(&self, name: &str) -> Result<CommitId> {
        let ref_name = RefName::new(name);
        let target = self.repo().view().get_local_bookmark(ref_name);
        if target.has_conflict() {
            bail!("bookmark {name:?} is conflicted — resolve it with jj first");
        }
        if let Some(id) = target.as_normal() {
            return Ok(id.clone());
        }
        for remote in [self.state.remote.as_str(), "git"] {
            let symbol = ref_name.to_remote_symbol(RemoteName::new(remote));
            let remote_ref = self.repo().view().get_remote_bookmark(symbol);
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
