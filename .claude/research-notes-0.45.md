# jj-lib 0.45.1 API verification (crates.io source)
Local checkout /Users/lita/src/jj is now main @ v0.45.1-24-g86d8bdf37; the only lib API delta
between local main and the 0.45.1 crate is additive git-worktree helpers (create_worktree /
unlink_worktree) — everything gt uses is identical. gt builds against crates.io jj-lib 0.45.1.


---

# repo-txn

## Summary

Verified against jj-lib 0.45.1 source at /Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src (paths below abbreviated as src/...; ref_name types live in the re-exported jj-core-0.45.1 crate). Headline deltas vs 0.36: nearly the whole mutation API is now async (drive with pollster::FutureExt::block_on; Repo trait is #[async_trait(?Send)] so futures are !Send); Transaction::commit is async and still returns Result<Arc<ReadonlyRepo>, TransactionCommitError> (call .operation().id() on the Arc<ReadonlyRepo>); Transaction::set_tag is gone, replaced by set_attribute (OperationMetadata.tags renamed to attributes); the has_rewrites assert before write is still present and rebase_descendants is still mandatory; MutableRepo::new_commit now takes MergedTree instead of MergedTreeId; Commit::tree() is now sync and infallible; Commit::is_empty is async and takes &dyn Repo; View::git_head is now per-workspace; StoreFactories::default() no longer exists — use jj_lib::default_backend_factories::default_backend_factories() (new module src/default_backend_factories.rs) plus default_working_copy_factories() for Workspace::load; WorkspaceId is now WorkspaceName/WorkspaceNameBuf in jj_lib::ref_name; bookmark merge/track methods are async returning IndexResult<()>.

## Key APIs

### ReadonlyRepo::start_transaction  (src/repo.rs:335)
```rust
pub fn start_transaction(self: &Arc<Self>) -> Transaction
```
Same shape as 0.36: takes &Arc<Self>, infallible, sync, no settings param (settings pulled from self internally).

### Transaction::commit [ASYNC]  (src/transaction.rs:125-130)
```rust
pub async fn commit(
        self,
        description: impl Into<String>,
    ) -> Result<Arc<ReadonlyRepo>, TransactionCommitError>
```
ASYNC now (0.36 was sync). Still returns Arc<ReadonlyRepo>; ReadonlyRepo::operation() (repo.rs:302) exists so result.operation().id() still works. Error type is TransactionCommitError (transaction.rs:44), not RepoLoaderError. Implemented as self.write(description).await?.publish().await.

### Transaction::write [ASYNC]  (src/transaction.rs:135-138)
```rust
pub async fn write(
        mut self,
        description: impl Into<String>,
    ) -> Result<UnpublishedOperation, TransactionCommitError>
```
ASYNC. UnpublishedOperation is #[must_use]; finish with .publish().await or .leave_unpublished().

### UnpublishedOperation::publish / leave_unpublished / operation [ASYNC]  (src/transaction.rs:231, 239, 227)
```rust
pub async fn publish(self) -> Result<Arc<ReadonlyRepo>, TransactionCommitError>
pub fn leave_unpublished(self) -> Arc<ReadonlyRepo>
pub fn operation(&self) -> &Operation
```
publish is async (takes op_heads lock); leave_unpublished and operation are sync.

### Transaction::set_is_snapshot  (src/transaction.rs:116)
```rust
pub fn set_is_snapshot(&mut self, is_snapshot: bool)
```
Unchanged, sync.

### Transaction::set_tag -> set_attribute (RENAMED)  (src/transaction.rs:90-92)
```rust
pub fn set_attribute(&mut self, key: String, value: String)
```
0.36 set_tag no longer exists. OperationMetadata now has `pub attributes: BTreeMap<String, String>` (op_store.rs:427). Also new: set_workspace_name(&mut self, workspace_name: &WorkspaceName) at transaction.rs:120.

### Transaction::merge_operation [ASYNC]  (src/transaction.rs:103-107)
```rust
pub async fn merge_operation(
        &mut self,
        base_op: &Operation,
        other_op: &Operation,
    ) -> Result<(), RepoLoaderError>
```
ASYNC now.

### has_rewrites assert before write (STILL PRESENT)  (src/transaction.rs:141-144)
```rust
assert!(
            !mut_repo.has_rewrites(),
            "BUG: Descendants have not been rebased after the last rewrites."
        );
```
Still mandatory to call rebase_descendants() (or reparent_descendants(), or rebase_descendants_with_options) before write/commit whenever rewrite_commit/record_abandoned_commit/set_rewritten_commit were used. Additional 0.45 assert at transaction.rs:147-150: view.is_heads_normalized() — satisfied automatically because write() calls mut_repo.consume() which normalizes heads.

### MutableRepo::new_commit (SIGNATURE CHANGED)  (src/repo.rs:1004)
```rust
pub fn new_commit(&mut self, parents: Vec<CommitId>, tree: MergedTree) -> CommitBuilder<'_>
```
Delta vs 0.36: takes MergedTree (from commit.tree(), now sync) instead of MergedTreeId; no settings param. CommitBuilder::write is `pub async fn write(self) -> BackendResult<Commit>` (commit_builder.rs:163); set_parents/set_description are by-value builder setters (commit_builder.rs:66,116).

### MutableRepo::rewrite_commit  (src/repo.rs:1010)
```rust
pub fn rewrite_commit(&mut self, predecessor: &Commit) -> CommitBuilder<'_>
```
No settings param (0.36 also dropped it). CommitBuilder::write records the rewrite in parent_mapping automatically.

### MutableRepo::rebase_descendants [ASYNC]  (src/repo.rs:1531)
```rust
pub async fn rebase_descendants(&mut self) -> BackendResult<usize>
```
ASYNC now. Returns number of rebased commits, clears parent_mapping.

### MutableRepo::rebase_descendants_with_options [ASYNC]  (src/repo.rs:1496-1501)
```rust
pub async fn rebase_descendants_with_options(
        &mut self,
        immutable: &Arc<ResolvedRevsetExpression>,
        options: &RebaseOptions,
        mut progress: impl FnMut(Commit, RebasedCommit),
    ) -> BackendResult<()>
```
New required `immutable` param vs 0.36 (pass &RevsetExpression::none() for no immutability). RebaseOptions/RebasedCommit from crate::rewrite.

### MutableRepo::transform_descendants [ASYNC]  (src/repo.rs:1406-1410)
```rust
pub async fn transform_descendants(
        &mut self,
        roots: Vec<CommitId>,
        callback: impl AsyncFnMut(CommitRewriter) -> BackendResult<()>,
    ) -> BackendResult<()>
```
Callback is now AsyncFnMut (0.36 took a sync FnMut). CommitRewriter from crate::rewrite.

### MutableRepo::transform_descendants_with_options [ASYNC]  (src/repo.rs:1430-1437)
```rust
pub async fn transform_descendants_with_options(
        &mut self,
        roots: Vec<CommitId>,
        immutable: &Arc<ResolvedRevsetExpression>,
        new_parents_map: &HashMap<CommitId, Vec<CommitId>>,
        options: &RewriteRefsOptions,
        callback: impl AsyncFnMut(CommitRewriter) -> BackendResult<()>,
    ) -> BackendResult<()>
```
Added `immutable` param vs 0.36. Also new: transform_commits (repo.rs:1448) which rewrites only the given commits, not descendants.

### MutableRepo::record_abandoned_commit  (src/repo.rs:1061)
```rust
pub fn record_abandoned_commit(&mut self, old_commit: &Commit)
```
Sync. Variant record_abandoned_commit_with_parents(old_id: CommitId, new_parent_ids: impl IntoIterator<Item = CommitId>) at repo.rs:1075.

### MutableRepo::set_rewritten_commit  (src/repo.rs:1027)
```rust
pub fn set_rewritten_commit(&mut self, old_id: CommitId, new_id: CommitId)
```
Sync. set_divergent_rewrite(old_id, new_ids: impl IntoIterator<Item = CommitId>) at repo.rs:1040.

### MutableRepo::new_parents  (src/repo.rs:1097)
```rust
pub fn new_parents(&self, old_ids: &[CommitId]) -> Vec<CommitId>
```
Sync; takes slice (0.36 same).

### MutableRepo::has_changes / has_rewrites  (src/repo.rs:986, 1087)
```rust
pub fn has_changes(&self) -> bool
pub fn has_rewrites(&self) -> bool
```
Both sync.

### MutableRepo::check_out [ASYNC]  (src/repo.rs:1621-1625)
```rust
pub async fn check_out(
        &mut self,
        name: WorkspaceNameBuf,
        commit: &Commit,
    ) -> Result<Commit, CheckOutCommitError>
```
ASYNC. Param type renamed: 0.36 WorkspaceId -> 0.45 WorkspaceNameBuf (jj_lib::ref_name). No settings param.

### MutableRepo::edit [ASYNC]  (src/repo.rs:1634-1638)
```rust
pub async fn edit(
        &mut self,
        name: WorkspaceNameBuf,
        commit: &Commit,
    ) -> Result<(), EditCommitError>
```
ASYNC (abandons discardable old wc commit, adds head, sets wc commit).

### MutableRepo::set_wc_commit  (src/repo.rs:1566-1570)
```rust
pub fn set_wc_commit(
        &mut self,
        name: WorkspaceNameBuf,
        commit_id: CommitId,
    ) -> Result<(), RewriteRootCommit>
```
Sync; WorkspaceNameBuf instead of WorkspaceId.

### MutableRepo::get_local_bookmark  (src/repo.rs:1782)
```rust
pub fn get_local_bookmark(&self, name: &RefName) -> &RefTarget
```
Name type is &RefName (jj_lib::ref_name), not &str — build via "name".as_ref() or RefNameBuf::from(String).

### MutableRepo::set_local_bookmark_target  (src/repo.rs:1786)
```rust
pub fn set_local_bookmark_target(&mut self, name: &RefName, target: RefTarget)
```
Sync; also adds target's added_ids as view heads.

### MutableRepo::merge_local_bookmark [ASYNC]  (src/repo.rs:1793-1798)
```rust
pub async fn merge_local_bookmark(
        &mut self,
        name: &RefName,
        base_target: &RefTarget,
        other_target: &RefTarget,
    ) -> IndexResult<()>
```
ASYNC + fallible now (0.36 was sync returning ()).

### MutableRepo::get_remote_bookmark / set_remote_bookmark  (src/repo.rs:1806, 1810)
```rust
pub fn get_remote_bookmark(&self, symbol: RemoteRefSymbol<'_>) -> &RemoteRef
pub fn set_remote_bookmark(&mut self, symbol: RemoteRefSymbol<'_>, remote_ref: RemoteRef)
```
Sync. RemoteRefSymbol<'a> { pub name: &'a RefName, pub remote: &'a RemoteName } defined in jj-core-0.45.1/src/ref_name.rs:371 (re-exported as jj_lib::ref_name).

### MutableRepo::track_remote_bookmark [ASYNC]  (src/repo.rs:1829)
```rust
pub async fn track_remote_bookmark(&mut self, symbol: RemoteRefSymbol<'_>) -> IndexResult<()>
```
ASYNC + fallible now. untrack_remote_bookmark (repo.rs:1840) remains sync/infallible.

### RefTarget  (src/op_store.rs:51-134)
```rust
pub fn absent() -> Self  // :63
pub fn absent_ref() -> &'static Self  // :70
pub fn normal(id: CommitId) -> Self  // :81
pub fn as_normal(&self) -> Option<&CommitId>  // :103
pub fn is_present(&self) -> bool  // :114
pub fn has_conflict(&self) -> bool  // :119
pub fn added_ids(&self) -> impl Iterator<Item = &CommitId>  // :127
```
Still in jj_lib::op_store; unchanged from 0.36. Also is_absent():108, as_resolved():98, from_merge():93.

### RemoteRef / RemoteRefState  (src/op_store.rs:138-141, 190-196)
```rust
pub struct RemoteRef {
    pub target: RefTarget,
    pub state: RemoteRefState,
}
pub enum RemoteRefState { New, Tracked }
```
Still in jj_lib::op_store. Helpers: absent()/absent_ref()/is_tracked()/tracked_target() (op_store.rs:145-185).

### op_store::View (struct)  (src/op_store.rs:249-263)
```rust
pub struct View {
    pub head_ids: HashSet<CommitId>,
    pub local_bookmarks: BTreeMap<RefNameBuf, RefTarget>,
    pub local_tags: BTreeMap<RefNameBuf, RefTarget>,
    pub remote_views: BTreeMap<RemoteNameBuf, RemoteView>,
    pub git_refs: BTreeMap<GitRefNameBuf, RefTarget>,
    pub git_heads: BTreeMap<WorkspaceNameBuf, RefTarget>,
    pub wc_commit_ids: BTreeMap<WorkspaceNameBuf, CommitId>,
}
```
Delta vs 0.36: git_head is now per-workspace map `git_heads`; keys are typed name Bufs, not String. tags split into local_tags + remote_views.tags.

### RepoLoader::load_at_head [ASYNC]  (src/repo.rs:714)
```rust
pub async fn load_at_head(&self) -> Result<Arc<ReadonlyRepo>, RepoLoaderError>
```
ASYNC now; still no args. Merges divergent op heads automatically.

### RepoLoader::load_at [ASYNC]  (src/repo.rs:740)
```rust
pub async fn load_at(&self, op: &Operation) -> Result<Arc<ReadonlyRepo>, RepoLoaderError>
```
ASYNC now.

### RepoLoader::load_operation [ASYNC]  (src/repo.rs:772)
```rust
pub async fn load_operation(&self, id: &OperationId) -> OpStoreResult<Operation>
```
ASYNC now. Also root_operation() (repo.rs:765, async) and init_from_file_system(settings, repo_path, store_factories) -> Result<Self, StoreLoadError> (repo.rs:653, sync).

### StoreFactories (default() REMOVED)  (src/repo.rs:434-469 and src/default_backend_factories.rs:30)
```rust
impl StoreFactories { pub fn empty() -> Self ... }
pub fn default_backend_factories() -> StoreFactories  // default_backend_factories.rs:30
pub fn default_working_copy_factories() -> WorkingCopyFactories  // :87
pub fn default_working_copy_factory() -> Box<dyn WorkingCopyFactory>  // :97
```
DELTA: StoreFactories no longer implements Default. Use jj_lib::default_backend_factories::default_backend_factories() (registers GitBackend under `git` feature, SimpleBackend, SimpleOpStore, SimpleOpHeadsStore, DefaultIndexStore, DefaultSubmoduleStore).

### Workspace::load  (src/workspace.rs:410-419)
```rust
pub fn load(
        user_settings: &UserSettings,
        workspace_path: &Path,
        store_factories: &StoreFactories,
        working_copy_factories: &WorkingCopyFactories,
    ) -> Result<Self, WorkspaceLoadError>
```
SYNC. WorkingCopyFactories = HashMap<String, Box<dyn WorkingCopyFactory>> (workspace.rs:565). Accessors: workspace_root():421, workspace_name():425 (-> &WorkspaceName), repo_path():429, repo_loader():433, settings():438. Workspace::check_out(operation_id, old_tree: Option<&MergedTree>, commit) at workspace.rs:456 and start_working_copy_mutation() at :446 are ASYNC.

### ReadonlyRepo accessors  (src/repo.rs:298-347, 356)
```rust
pub fn op_id(&self) -> &OperationId  // :298
pub fn operation(&self) -> &Operation  // :302
pub fn view(&self) -> &View  // :306
pub fn readonly_index(&self) -> &dyn ReadonlyIndex  // :310
pub fn settings(&self) -> &UserSettings  // :331
pub fn loader(&self) -> &RepoLoader  // :294
pub async fn reload_at_head(&self) -> Result<Arc<Self>, RepoLoaderError>  // :340
pub async fn reload_at(&self, operation: &Operation) -> Result<Arc<Self>, RepoLoaderError>  // :345
// via Repo trait impl: fn store(&self) -> &Arc<Store>  // :356
// via Repo trait impl: fn index(&self) -> &dyn Index  // :364
```
reload_at/reload_at_head are ASYNC (0.36 sync). store()/index()/op_store()/view() come from the Repo trait (repo.rs:120-158, #[async_trait(?Send)]) — `use jj_lib::repo::Repo;` needed.

### View (wrapper) accessors  (src/view.rs:68, 203, 269, 175, 184, 110, 106)
```rust
pub fn get_wc_commit_id(&self, name: &WorkspaceName) -> Option<&CommitId>  // :68
pub fn get_local_bookmark(&self, name: &RefName) -> &RefTarget  // :203
pub fn get_remote_bookmark(&self, symbol: RemoteRefSymbol<'_>) -> &RemoteRef  // :269
pub fn local_bookmarks(&self) -> impl Iterator<Item = (&RefName, &RefTarget)>  // :175
pub fn local_bookmarks_for_commit(&self, commit_id: &CommitId) -> impl Iterator<Item = (&RefName, &RefTarget)>  // :184
pub fn git_head(&self, workspace: &WorkspaceName) -> &RefTarget  // :110
pub fn git_refs(&self) -> &BTreeMap<GitRefNameBuf, RefTarget>  // :106
```
All sync. DELTA: git_head now takes &WorkspaceName (per-workspace); heads():86 -> &HashSet<CommitId>; wc_commit_ids():64 -> &BTreeMap<WorkspaceNameBuf, CommitId>; local_remote_bookmarks(remote_name):304; bookmarks():91 yields LocalRemoteRefTarget.

### Commit accessors  (src/commit.rs:102-201)
```rust
pub fn id(&self) -> &CommitId  // :102 sync
pub fn change_id(&self) -> &ChangeId  // :171 sync
pub fn parent_ids(&self) -> &[CommitId]  // :106 sync
pub async fn parents(&self) -> BackendResult<Vec<Self>>  // :110 ASYNC
pub fn tree(&self) -> MergedTree  // :120 sync, infallible
pub fn tree_ids(&self) -> &Merge<TreeId>  // :128 sync
pub fn description(&self) -> &str  // :179 sync
pub async fn is_empty(&self, repo: &dyn Repo) -> BackendResult<bool>  // :160 ASYNC
pub fn has_conflict(&self) -> bool  // :167 sync, infallible
pub async fn is_discardable(&self, repo: &dyn Repo) -> BackendResult<bool>  // :199 ASYNC
```
DELTAS vs 0.36: tree() is sync and returns MergedTree directly (0.36 returned BackendResult); tree_id() renamed tree_ids() returning &Merge<TreeId>; is_empty is async and requires &dyn Repo; has_conflict is sync bool (checks tree_ids().is_resolved()); parents() is async. Also parent_tree(repo) :134 ASYNC.

### git integration fns (for colocated flow) [ASYNC]  (src/git.rs:590, 1174, 1327, 1841)
```rust
pub async fn import_refs(
    mut_repo: &mut MutableRepo,
    options: &GitImportOptions,
) -> Result<GitImportStats, GitImportError>  // :590 ASYNC
pub async fn import_head(
    mut_repo: &mut MutableRepo,
    workspace_name: &WorkspaceName,
    workspace_root: &Path,
) -> Result<(), GitImportError>  // :1174 ASYNC
pub fn export_refs(mut_repo: &mut MutableRepo) -> Result<GitExportStats, GitExportError>  // :1327 SYNC
pub async fn reset_head(
    mut_repo: &mut MutableRepo,
    workspace_name: &WorkspaceName,
    workspace_root: &Path,
    wc_commit: &Commit,
) -> Result<(), GitResetHeadError>  // :1841 ASYNC
```
DELTAS: import_refs takes &GitImportOptions (git.rs:521, #[derive(Debug)] only — NO Default; fields: abandon_unreachable_commits: bool, record_synthetic_predecessors: bool, remote_auto_track_bookmarks: HashMap<RemoteNameBuf, StringMatcher>) instead of &GitSettings; import_head/reset_head now require workspace_name + workspace_root (per-workspace git HEAD).

## Minimal flow
use pollster::FutureExt as _; // .block_on() on every async call
// 1. Load workspace (sync):
let factories = jj_lib::default_backend_factories::default_backend_factories();
let wc_factories = jj_lib::default_backend_factories::default_working_copy_factories();
let workspace = Workspace::load(&user_settings, workspace_path, &factories, &wc_factories)?;
// 2. Load repo at head (async):
let repo: Arc<ReadonlyRepo> = workspace.repo_loader().load_at_head().block_on()?;
// 3. Start transaction (sync, infallible):
let mut tx = repo.start_transaction();
let mut_repo = tx.repo_mut();
// 4. Mutate, e.g.:
let commit = repo.store().get_commit_async(&id).block_on()?;            // async
let new = mut_repo.rewrite_commit(&commit).set_description("...").write().block_on()?; // async write
mut_repo.set_local_bookmark_target("feat-1".as_ref(), RefTarget::normal(new.id().clone())); // sync
// 5. MANDATORY before commit if any rewrites/abandons happened:
let num_rebased = mut_repo.rebase_descendants().block_on()?;            // async
// 6. Commit (async; publishes op):
let new_repo: Arc<ReadonlyRepo> = tx.commit("gt: restack").block_on()?;
let op_id = new_repo.operation().id();
// Colocated git sync inside the tx before rebase/commit as needed:
//   git::import_refs(tx.repo_mut(), &GitImportOptions{..}).block_on()?  (async)
//   git::export_refs(tx.repo_mut())?                                     (sync)
//   git::reset_head(tx.repo_mut(), workspace.workspace_name(), workspace.workspace_root(), &wc_commit).block_on()? (async)
// Working-copy update after commit (async):
//   workspace.check_out(new_repo.op_id().clone(), Some(&old_tree), &wc_commit).block_on()?

## Gotchas
- Async everywhere: Transaction::commit/write, RepoLoader::load_at_head/load_at/load_operation, ReadonlyRepo::reload_at(_head), MutableRepo::rebase_descendants/transform_descendants/check_out/edit/merge_local_bookmark/track_remote_bookmark, CommitBuilder::write, Commit::parents/is_empty/is_discardable, git::import_refs/import_head/reset_head are all async in 0.45.1. The Repo trait is #[async_trait(?Send)] so futures are !Send — block_on them on the current thread (pollster), never send to a threaded executor.
- Transaction::set_tag no longer exists; use Transaction::set_attribute(key: String, value: String) (transaction.rs:90). OperationMetadata field is `attributes: BTreeMap<String, String>` (op_store.rs:427).
- The `assert!(!mut_repo.has_rewrites(), "BUG: Descendants have not been rebased after the last rewrites.")` is still in Transaction::write (transaction.rs:141-144) — commit() calls write(), so any rewrite_commit/record_abandoned_commit/set_rewritten_commit MUST be followed by rebase_descendants()/reparent_descendants()/rebase_descendants_with_options() before tx.commit(), or the process panics.
- MutableRepo::new_commit takes MergedTree (a value from commit.tree(), which is now sync/infallible), NOT MergedTreeId. Commit::tree_id() was renamed to tree_ids() -> &Merge<TreeId>.
- StoreFactories::default() is gone (only StoreFactories::empty() remains, repo.rs:461). Use jj_lib::default_backend_factories::default_backend_factories() and default_working_copy_factories(); the GitBackend factory is only registered with the `git` cargo feature enabled on jj-lib.
- All name parameters are strongly typed (jj_lib::ref_name re-exported from jj-core): &RefName / RefNameBuf for bookmarks, &RemoteName / RemoteNameBuf, RemoteRefSymbol<'a>{name, remote}, WorkspaceName/WorkspaceNameBuf (replaces 0.36 WorkspaceId). Convert with "main".as_ref(), RefNameBuf::from(string), name.to_remote_symbol(remote), WorkspaceName::DEFAULT.
- View::git_head is now per-workspace: git_head(&self, workspace: &WorkspaceName) -> &RefTarget (view.rs:110); op_store::View has git_heads: BTreeMap<WorkspaceNameBuf, RefTarget>, and git::import_head/reset_head require workspace_name + workspace_root.
- git::import_refs takes &GitImportOptions (git.rs:521), which derives only Debug — no Default; you must construct all three fields (abandon_unreachable_commits, record_synthetic_predecessors, remote_auto_track_bookmarks). GitSettings::from_settings (git.rs:110) exists separately for the abandon/record bools.
- Transaction::commit error type is TransactionCommitError (Index/IndexStore/OpHeadsStore/OpStore variants), no longer a generic backend error; RepoLoaderError has a TransactionCommit variant wrapping it.
- Bookmark write helpers changed fallibility: merge_local_bookmark and track_remote_bookmark are async -> IndexResult<()>; get/set_local_bookmark_target and get/set_remote_bookmark remain sync. set_local_bookmark_target with RefTarget::absent() deletes the bookmark and prunes absent tracked remote refs.
- MutableRepo methods needing Repo-trait accessors (store(), view(), index()) require `use jj_lib::repo::Repo as _;`.
- Store::get_commit (sync, store.rs:152) still exists alongside get_commit_async (store.rs:156); both take self: &Arc<Store>.


---

# workspace-wc

## Summary

Verified jj-lib 0.45.1 workspace/working-copy API against source at /Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src (and jj-core-0.45.1, which 0.45 split out and re-exports through jj_lib). Headline deltas vs 0.36/0.40 research: (1) Workspace::init_colocated_git gained a required `object_hash: gix::hash::Kind` param; (2) Workspace::start_working_copy_mutation and WorkingCopy::start_mutation are now async; (3) StoreFactories no longer implements Default — use default_backend_factories::default_backend_factories(); (4) default_working_copy_factories() moved from jj_lib::workspace to jj_lib::default_backend_factories; (5) matchers/repo_path/ref_name/merge/conflict_labels/file_util now live in the jj-core crate but keep their jj_lib::* paths via `pub use jj_core::...` in lib.rs; (6) there is NO CheckoutOptions anywhere in 0.45.1 — Workspace::check_out takes (operation_id, old_tree, commit) only; (7) MergedTree equality is via tree_ids_and_labels() -> (&Merge<TreeId>, &ConflictLabels); Commit::tree() is sync and returns an owned MergedTree; (8) SnapshotOptions still has exactly the 5 known fields and no constructor/Default. All async fns are drivable with pollster::FutureExt::block_on.

## Key APIs

### Workspace::init_colocated_git [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/workspace.rs:225)
```rust
pub async fn init_colocated_git(user_settings: &UserSettings, workspace_root: &Path, object_hash: gix::hash::Kind) -> Result<(Self, Arc<ReadonlyRepo>), WorkspaceInitError>
```
Delta vs 0.36/0.40: new required object_hash param (pass gix::hash::Kind::Sha1 for normal repos). Creates a NEW git repo with worktree at workspace_root (gix create::Kind::WithWorktree) plus .jj. cfg(feature = "git"). For colocating onto an EXISTING git repo (jj git init --colocate on a repo that already has .git), use init_external_git with git_repo_path = workspace_root.join(".git") instead — that is what jj-cli does.

### Workspace::init_external_git [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/workspace.rs:259)
```rust
pub async fn init_external_git(user_settings: &UserSettings, workspace_root: &Path, git_repo_path: &Path) -> Result<(Self, Arc<ReadonlyRepo>), WorkspaceInitError>
```
Same shape as 0.40 (async in both). git_repo_path is the .git dir, not the worktree. Wraps GitBackend::init_external (git_backend.rs:280, sync). After init you must import refs/head yourself: git::import_refs (git.rs:590, async, takes &mut MutableRepo and &GitImportOptions) and git::import_head (git.rs:1174, async, now takes (mut_repo, workspace_name: &WorkspaceName, workspace_root: &Path) — delta vs 0.36 which took only mut_repo), then check out the resulting commit.

### Workspace::init_workspace_with_existing_repo [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/workspace.rs:369)
```rust
pub async fn init_workspace_with_existing_repo(workspace_root: &Path, repo_path: &Path, repo: &Arc<ReadonlyRepo>, working_copy_factory: &dyn WorkingCopyFactory, workspace_name: WorkspaceNameBuf) -> Result<(Self, Arc<ReadonlyRepo>), WorkspaceInitError>
```
For adding a second workspace to an existing .jj repo (jj workspace add), not for git colocation. WorkspaceNameBuf is jj_core::ref_name re-exported as jj_lib::ref_name; WorkspaceName::DEFAULT = "default" (jj-core-0.45.1/src/ref_name.rs:318). No longer takes UserSettings (0.36 took &UserSettings first).

### Workspace::load  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/workspace.rs:410)
```rust
pub fn load(user_settings: &UserSettings, workspace_path: &Path, store_factories: &StoreFactories, working_copy_factories: &WorkingCopyFactories) -> Result<Self, WorkspaceLoadError>
```
SYNC. Current and simplest entry point — internally does DefaultWorkspaceLoader::new(path)?.load(...). Call as: Workspace::load(&settings, root, &default_backend_factories(), &default_working_copy_factories()). Delta: StoreFactories::default() existed in 0.40 (repo.rs:429) but is GONE in 0.45.1 — the populated factories now come from jj_lib::default_backend_factories::default_backend_factories() (default_backend_factories.rs:30). StoreFactories::empty() (repo.rs:461) is the empty one — do not use it for loading.

### DefaultWorkspaceLoaderFactory / trait WorkspaceLoader  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/workspace.rs:545 (factory), :526 (trait), :534 (load))
```rust
impl WorkspaceLoaderFactory for DefaultWorkspaceLoaderFactory { fn create(&self, workspace_root: &Path) -> Result<Box<dyn WorkspaceLoader>, WorkspaceLoadError> }  //  trait WorkspaceLoader { fn workspace_root(&self) -> &Path; fn repo_path(&self) -> &Path; fn load(&self, user_settings: &UserSettings, store_factories: &StoreFactories, working_copy_factories: &WorkingCopyFactories) -> Result<Workspace, WorkspaceLoadError>; fn get_working_copy_type(&self) -> Result<String, StoreLoadError>; }
```
The 0.40 impulse pattern still compiles except for StoreFactories::default(): DefaultWorkspaceLoaderFactory.create(root)?.load(&settings, &default_backend_factories(), &default_working_copy_factories()). Workspace::load is the shorter equivalent. All sync. Note the loader does NOT search parent dirs for .jj — you must walk up yourself (NoWorkspaceHere error otherwise).

### WorkingCopyFactories (type) + default_working_copy_factories()  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/workspace.rs:565 (type); /Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/default_backend_factories.rs:87 (fn))
```rust
pub type WorkingCopyFactories = HashMap<String, Box<dyn WorkingCopyFactory>>;  //  pub fn default_working_copy_factories() -> WorkingCopyFactories  //  also default_backend_factories.rs:97: pub fn default_working_copy_factory() -> Box<dyn WorkingCopyFactory>
```
Delta: in 0.36/0.40 default_working_copy_factories() lived in jj_lib::workspace; in 0.45.1 it moved to jj_lib::default_backend_factories (module default_backend_factories, file default_backend_factories.rs). Type alias WorkingCopyFactories is still in jj_lib::workspace.

### Workspace accessors  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/workspace.rs:421,425,429,433,438,442)
```rust
pub fn workspace_root(&self) -> &Path; pub fn workspace_name(&self) -> &WorkspaceName; pub fn repo_path(&self) -> &Path; pub fn repo_loader(&self) -> &RepoLoader; pub fn settings(&self) -> &UserSettings; pub fn working_copy(&self) -> &dyn WorkingCopy
```
All sync. workspace_name() returns &WorkspaceName (jj_core::ref_name re-export) — delta vs 0.36 which returned &WorkspaceId. settings() delegates to repo_loader. To get a ReadonlyRepo: workspace.repo_loader().load_at_head() — repo.rs:714, pub async fn load_at_head(&self) -> Result<Arc<ReadonlyRepo>, RepoLoaderError> (ASYNC).

### Workspace::start_working_copy_mutation -> LockedWorkspace [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/workspace.rs:446)
```rust
pub async fn start_working_copy_mutation(&mut self) -> Result<LockedWorkspace<'_>, WorkingCopyStateError>
```
Delta: SYNC in 0.40, ASYNC in 0.45.1 — block_on it. Takes &mut self.

### LockedWorkspace::locked_wc / finish [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/workspace.rs:491,495)
```rust
pub fn locked_wc(&mut self) -> &mut dyn LockedWorkingCopy;  pub async fn finish(self, operation_id: OperationId) -> Result<(), WorkingCopyStateError>
```
locked_wc is sync; finish is ASYNC and consumes self, installing the new WorkingCopy back into the Workspace. OperationId comes from the repo you committed (repo.op_id().clone()).

### Workspace::check_out [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/workspace.rs:456)
```rust
pub async fn check_out(&mut self, operation_id: OperationId, old_tree: Option<&MergedTree>, commit: &Commit) -> Result<CheckoutStats, CheckoutError>
```
NO options argument; CheckoutOptions does not exist anywhere in jj-lib 0.45.1 (grepped whole src). Concurrent-checkout detection compares old_tree.tree_ids_and_labels() against locked old_tree — pass Some(&wc_commit.tree()) of the expected current wc commit, or None to skip the check. Returns CheckoutError::ConcurrentCheckout on mismatch.

### trait WorkingCopy [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/working_copy.rs:52-76)
```rust
#[async_trait(?Send)] pub trait WorkingCopy: Any + Send { fn name(&self) -> &str; fn workspace_name(&self) -> &WorkspaceName; fn operation_id(&self) -> &OperationId; fn tree(&self) -> Result<&MergedTree, WorkingCopyStateError>; fn sparse_patterns(&self) -> Result<&[RepoPathBuf], WorkingCopyStateError>; async fn start_mutation(&self) -> Result<Box<dyn LockedWorkingCopy>, WorkingCopyStateError>; }
```
operation_id/tree are sync; start_mutation is async (delta: sync in 0.40). Delta vs 0.36: fn tree_id() -> &MergedTreeId is gone — replaced by fn tree() -> Result<&MergedTree, _> (borrowed, no I/O). Trait is #[async_trait(?Send)] — its futures are not Send.

### trait LockedWorkingCopy [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/working_copy.rs:109-156)
```rust
#[async_trait] pub trait LockedWorkingCopy: Any + Send { fn old_operation_id(&self) -> &OperationId; fn old_tree(&self) -> &MergedTree; async fn snapshot(&mut self, options: &SnapshotOptions) -> Result<(MergedTree, SnapshotStats), SnapshotError>; async fn check_out(&mut self, commit: &Commit) -> Result<CheckoutStats, CheckoutError>; fn rename_workspace(&mut self, new_workspace_name: WorkspaceNameBuf); async fn reset(&mut self, commit: &Commit) -> Result<(), ResetError>; async fn recover(&mut self, commit: &Commit) -> Result<(), ResetError>; fn sparse_patterns(&self) -> Result<&[RepoPathBuf], WorkingCopyStateError>; async fn set_sparse_patterns(&mut self, new_sparse_patterns: Vec<RepoPathBuf>) -> Result<CheckoutStats, CheckoutError>; async fn finish(self: Box<Self>, operation_id: OperationId) -> Result<Box<dyn WorkingCopy>, WorkingCopyStateError>; }
```
snapshot/check_out/reset/recover/set_sparse_patterns/finish all ASYNC; old_operation_id/old_tree/rename_workspace/sparse_patterns sync. Delta vs 0.36: snapshot returns (MergedTree, SnapshotStats) tuple (0.36 returned MergedTreeId) and old_tree() returns &MergedTree (was old_tree_id() -> &MergedTreeId); reset/recover take &Commit.

### SnapshotOptions  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/working_copy.rs:205-227)
```rust
pub struct SnapshotOptions<'a> { pub base_ignores: Arc<GitIgnoreFile>, pub progress: Option<&'a SnapshotProgress<'a>>, pub start_tracking_matcher: &'a dyn Matcher, pub force_tracking_matcher: &'a dyn Matcher, pub max_new_file_size: u64, }
```
Exactly the 5 known fields — NO new fields vs 0.36/0.40, and no Default/empty_for_test constructor in 0.45.1: construct the literal. Typical gt values: base_ignores: GitIgnoreFile::empty() (or chained), progress: None, start_tracking_matcher: &EverythingMatcher, force_tracking_matcher: &NothingMatcher, max_new_file_size: u64::MAX. SnapshotProgress<'a> = dyn Fn(&RepoPath) + 'a + Sync (working_copy.rs:230).

### SnapshotStats / UntrackedReason / CheckoutStats  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/working_copy.rs:234-241, 245-255, 260-272)
```rust
pub struct SnapshotStats { pub untracked_paths: BTreeMap<RepoPathBuf, UntrackedReason>, pub invalid_utf8_paths: BTreeSet<(RepoPathBuf, OsString)>, }  //  pub enum UntrackedReason { FileTooLarge { size: u64, max_size: u64 }, FileNotAutoTracked }  //  pub struct CheckoutStats { pub updated_files: u32, pub added_files: u32, pub removed_files: u32, pub skipped_files: u32, }
```
SnapshotStats derives Default; invalid_utf8_paths is new-ish vs 0.36 (which had only untracked_paths). CheckoutStats unchanged vs 0.36.

### WorkingCopyFreshness::check_stale [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/working_copy.rs:361 (fn), 344-355 (enum))
```rust
pub enum WorkingCopyFreshness { Fresh, Updated(Box<Operation>), WorkingCopyStale, SiblingOperation }  //  pub async fn check_stale(locked_wc: &dyn LockedWorkingCopy, wc_commit: &Commit, repo: &ReadonlyRepo) -> Result<Self, OpStoreError>
```
ASYNC (also async in 0.40). Compares locked_wc.old_operation_id() to repo.op_id(); on Updated(op) you should reload the repo at that operation (repo_loader.load_at(&op)).

### GitIgnoreFile::empty / chain / chain_with_file  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/gitignore.rs:44, 53, 87)
```rust
pub fn empty() -> Arc<Self>;  pub fn chain(self: &Arc<Self>, prefix: &RepoPath, ignore_path: &Path, input: &[u8]) -> Result<Arc<Self>, GitIgnoreError>;  pub fn chain_with_file(self: &Arc<Self>, prefix: &RepoPath, file: PathBuf) -> Result<Arc<Self>, GitIgnoreError>
```
Delta vs 0.36: prefix is now &RepoPath (was &str). chain_with_file silently returns self.clone() if file is not a regular file. Module path unchanged: jj_lib::gitignore.

### EverythingMatcher / NothingMatcher (jj_lib::matchers)  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-core-0.45.1/src/matchers.rs:141 (EverythingMatcher), :127 (NothingMatcher); re-export at jj-lib-0.45.1/src/lib.rs:67 (pub use jj_core::matchers;))
```rust
pub struct EverythingMatcher;  pub struct NothingMatcher;
```
matchers.rs no longer exists inside jj-lib src — 0.45 split core types into the jj-core crate and jj-lib re-exports them, so use paths jj_lib::matchers::{EverythingMatcher, NothingMatcher, Matcher} exactly as before. Same applies to jj_lib::{repo_path, ref_name, merge, object_id, conflict_labels, file_util, graph, str_util} (lib.rs pub use jj_core::* lines 29-104).

### MergedTree::tree_ids_and_labels (equality check)  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/merged_tree.rs:129 (also :111 tree_ids, :122 labels, :135 into_tree_ids_and_labels, :140 trees))
```rust
pub fn tree_ids_and_labels(&self) -> (&Merge<TreeId>, &ConflictLabels);  pub fn tree_ids(&self) -> &Merge<TreeId>;  pub fn labels(&self) -> &ConflictLabels;  pub async fn trees(&self) -> BackendResult<Merge<Tree>>
```
NOT renamed — tree_ids_and_labels() exists in 0.45.1 (and already in 0.40). It replaced 0.36's MergedTree::id()/MergedTreeId equality: compare a.tree_ids_and_labels() == b.tree_ids_and_labels() (tuple of &Merge<TreeId> and &ConflictLabels, both PartialEq) — this is exactly what Workspace::check_out and check_stale do internally. MergedTree struct at merged_tree.rs:67 { store, tree_ids: Merge<TreeId>, labels: ConflictLabels }. ConflictLabels is jj_lib::conflict_labels (jj-core re-export). trees() is async; tree_ids_and_labels is sync/no I/O.

### Commit::tree  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/commit.rs:120 (also :128 tree_ids, :135 parent_tree))
```rust
pub fn tree(&self) -> MergedTree;  pub fn tree_ids(&self) -> &Merge<TreeId>;  pub async fn parent_tree(&self, repo: &dyn Repo) -> BackendResult<MergedTree>
```
Commit::tree() is SYNC and returns an OWNED MergedTree built from the commit's stored root_tree Merge<TreeId> + conflict_labels — no backend I/O, no Result. Delta vs 0.36 where tree() returned BackendResult<MergedTree>. parent_tree()/parents() are async.

### git::import_refs / git::import_head / GitImportOptions (colocated sync) [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:590 (import_refs), :1174 (import_head), :521 (GitImportOptions))
```rust
pub async fn import_refs(mut_repo: &mut MutableRepo, options: &GitImportOptions) -> Result<GitImportStats, GitImportError>;  pub async fn import_head(mut_repo: &mut MutableRepo, workspace_name: &WorkspaceName, workspace_root: &Path) -> Result<(), GitImportError>;  pub struct GitImportOptions { pub abandon_unreachable_commits: bool, pub record_synthetic_predecessors: bool, pub remote_auto_track_bookmarks: HashMap<RemoteNameBuf, StringMatcher>, }
```
Both ASYNC. Delta vs 0.36: import_refs takes &GitImportOptions instead of &GitSettings; import_head now needs workspace_name + workspace_root (colocated git dir is discovered per-workspace). Needed after init_external_git and on every gt command that must see externally-made git changes in a colocated repo.

### RepoLoader::load_at_head / load_at / init_from_file_system [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/repo.rs:714, :740, :653)
```rust
pub async fn load_at_head(&self) -> Result<Arc<ReadonlyRepo>, RepoLoaderError>;  pub async fn load_at(&self, op: &Operation) -> Result<Arc<ReadonlyRepo>, RepoLoaderError>;  pub fn init_from_file_system(settings: &UserSettings, repo_path: &Path, store_factories: &StoreFactories) -> Result<Self, StoreLoadError>
```
load_at_head/load_at ASYNC (load_at_head also auto-merges divergent op heads). init_from_file_system is sync. Delta vs 0.36: load_at_head no longer takes &UserSettings (settings captured in the loader).

## Minimal flow
All async calls driven via pollster::FutureExt::block_on. Load: let factories = jj_lib::default_backend_factories::default_backend_factories(); let wc_factories = jj_lib::default_backend_factories::default_working_copy_factories(); let mut workspace = Workspace::load(&user_settings, &workspace_root, &factories, &wc_factories)?;  (sync; walk up parents to find .jj yourself). Repo: let repo = workspace.repo_loader().load_at_head().block_on()?;  Snapshot (colocated: first import git changes in a tx — git::import_head(tx.repo_mut(), workspace.workspace_name(), workspace.workspace_root()).block_on()?; git::import_refs(tx.repo_mut(), &options).block_on()?): let mut locked_ws = workspace.start_working_copy_mutation().block_on()?; match WorkingCopyFreshness::check_stale(locked_ws.locked_wc(), &wc_commit, &repo).block_on()? { Fresh => {}, Updated(op) => repo = repo_loader.load_at(&op).block_on()?, ... }; let (new_tree, _stats) = locked_ws.locked_wc().snapshot(&SnapshotOptions { base_ignores: GitIgnoreFile::empty(), progress: None, start_tracking_matcher: &EverythingMatcher, force_tracking_matcher: &NothingMatcher, max_new_file_size: u64::MAX }).block_on()?; if new_tree.tree_ids_and_labels() != wc_commit.tree().tree_ids_and_labels() { rewrite wc commit in a tx with the new tree ids; let repo = tx.commit(...).block_on()?; locked_ws.finish(repo.op_id().clone()).block_on()?; } else { locked_ws.finish(repo.op_id().clone()).block_on()?; }  Checkout: let stats = workspace.check_out(repo.op_id().clone(), Some(&old_wc_commit.tree()), &new_commit).block_on()?;  Init colocated on existing git repo: Workspace::init_external_git(&settings, root, &root.join(\".git\")).block_on()? then import_head/import_refs + check_out; on a fresh dir: Workspace::init_colocated_git(&settings, root, gix::hash::Kind::Sha1).block_on()?.

## Gotchas
- jj-lib 0.45 depends on a new jj-core crate (jj-core-0.45.1 in the registry) and re-exports matchers, repo_path, ref_name, merge, object_id, conflict_labels, file_util, graph, dag_walk, str_util, hex_util, diff from it (lib.rs:29-104). Consumer paths jj_lib::matchers::EverythingMatcher etc. still work — do NOT add a direct jj-core dependency; version-match issues arise if you do.
- StoreFactories::default() is GONE in 0.45.1 (existed through 0.40 at repo.rs:429). StoreFactories::empty() exists but is empty — loading a git-backed repo with it fails with UnsupportedType. Use jj_lib::default_backend_factories::default_backend_factories().
- default_working_copy_factories() moved modules: jj_lib::workspace (0.36/0.40) -> jj_lib::default_backend_factories (0.45.1). The WorkingCopyFactories type alias stayed in jj_lib::workspace.
- Workspace::start_working_copy_mutation and WorkingCopy::start_mutation became ASYNC (sync in 0.40) — every mutation path now needs block_on.
- Workspace::init_colocated_git gained a required object_hash: gix::hash::Kind param (use gix::hash::Kind::Sha1). It CREATES a new git repo; for an existing git repo use init_external_git(root.join(".git")) — matching jj git init --colocate semantics — followed by git::import_head + git::import_refs, both async, with import_head now requiring workspace_name and workspace_root.
- There is NO CheckoutOptions type anywhere in 0.45.1; Workspace::check_out(operation_id, old_tree: Option<&MergedTree>, commit) and LockedWorkingCopy::check_out(commit) — no options args.
- SnapshotOptions has NO constructor (no Default, no empty_for_test) in 0.45.1 — you must write the 5-field struct literal. Fields unchanged vs 0.36: base_ignores, progress, start_tracking_matcher, force_tracking_matcher, max_new_file_size.
- MergedTree identity: no MergedTreeId anymore. Equality/staleness checks compare .tree_ids_and_labels() tuples ((&Merge<TreeId>, &ConflictLabels)). LockedWorkingCopy::old_tree() returns &MergedTree (not an id), and LockedWorkingCopy::snapshot returns (MergedTree, SnapshotStats).
- Commit::tree() is sync and infallible in 0.45.1 (returns owned MergedTree, no backend I/O) — delta vs 0.36's BackendResult<MergedTree>. But Commit::parents()/parent_tree() are async.
- trait WorkingCopy is #[async_trait(?Send)] (non-Send futures) while trait LockedWorkingCopy is #[async_trait] (Send). pollster::block_on handles both, but don't try to spawn these futures on a multithreaded executor.
- Workspace::load / DefaultWorkspaceLoader do not search ancestor directories for .jj — gt must implement the upward walk (jj-cli does this itself).
- WorkspaceId is gone: workspace naming uses WorkspaceName / WorkspaceNameBuf (jj_lib::ref_name), with WorkspaceName::DEFAULT = "default" (jj-core ref_name.rs:318).
- Local grep on this machine is aliased to ugrep and silently mismatches some BRE patterns — during verification /usr/bin/grep was used; if you re-verify, do the same.


---

# git

## Summary

Verified jj-lib 0.45.1 git APIs at /Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src (git.rs 3615 lines, git_subprocess.rs, git_backend.rs, refs.rs) plus jj-core-0.45.1 (ref_name.rs, merge.rs, str_util.rs — jj-lib re-exports these as jj_lib::ref_name / jj_lib::merge / jj_lib::str_util). Major deltas vs 0.36/0.40: reset_head and import_head now take workspace_name+workspace_root; GitFetch::fetch lost the fetch_tags_override param (now 4 args); push is push_refs/push_updates with GitPushRefTargets built from merge::Diff<Option<CommitId>> (BookmarkPushUpdate/GitBranchPushTargets gone); refs.rs renamed classify_bookmark_push_action→classify_ref_push_action and BookmarkPushAction→RefPushAction; GitSettings dropped auto_local_bookmark and gained record_synthetic_predecessors; GitImportOptions gained remote_auto_track_bookmarks; GitImportStats fields restructured (Vec<Commit>, GitImportRefUpdate, rewritten_commit_ids). GitSubprocessOptions with .environment map exists and is the GIT_ASKPASS injection point. Callbacks are the `GitSubprocessCallback` trait (no RemoteCallbacks struct). export_refs is sync and still runs on tx.repo_mut() pre-commit. push_refs DOES update the view (remote-tracking bookmarks set to Tracked) and exports refs/remotes/* to the local git repo automatically.

## Key APIs

### GitSettings  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:102)
```rust
pub struct GitSettings {
    pub abandon_unreachable_commits: bool,
    pub executable_path: PathBuf,
    pub record_synthetic_predecessors: bool,
    pub write_change_id_header: bool,
}
```
Delta vs 0.36/0.40: auto_local_bookmark field REMOVED (auto-tracking moved to GitImportOptions.remote_auto_track_bookmarks); record_synthetic_predecessors added. from_settings at git.rs:110: `pub fn from_settings(settings: &UserSettings) -> Result<Self, ConfigGetError>` reads git.abandon-unreachable-commits, git.executable-path, git.record-synthetic-predecessors, git.write-change-id-header. Helper to_subprocess_options at git.rs:120: `pub fn to_subprocess_options(&self) -> GitSubprocessOptions` (clones executable_path, empty environment).

### GitSubprocessOptions  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:130)
```rust
pub struct GitSubprocessOptions {
    pub executable_path: PathBuf,
    /// Used by consumers of jj-lib to set environment variables like
    /// GIT_ASKPASS (for authentication callbacks) or GIT_TRACE (for debugging).
    pub environment: HashMap<OsString, OsString>,
}
```
EXISTS in 0.45.1 with the .environment map — this is the documented GIT_ASKPASS injection point. from_settings at git.rs:140: `pub fn from_settings(settings: &UserSettings) -> Result<Self, ConfigGetError>` (reads git.executable-path, environment starts empty). Same shape as 0.40.

### import_refs [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:590)
```rust
pub async fn import_refs(
    mut_repo: &mut MutableRepo,
    options: &GitImportOptions,
) -> Result<GitImportStats, GitImportError>
```
ASYNC. Takes &GitImportOptions (0.36 took &GitSettings). Drive with pollster block_on. Runs on tx.repo_mut().

### import_some_refs [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:601)
```rust
pub async fn import_some_refs(
    mut_repo: &mut MutableRepo,
    options: &GitImportOptions,
    git_ref_filter: impl Fn(GitRefKind, RemoteRefSymbol<'_>) -> bool,
) -> Result<GitImportStats, GitImportError>
```
ASYNC. Filter receives (GitRefKind::{Bookmark,Tag}, RemoteRefSymbol{name: &RefName, remote: &RemoteName}).

### GitImportOptions  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:521)
```rust
pub struct GitImportOptions {
    /// Whether to abandon commits that became unreachable in Git.
    pub abandon_unreachable_commits: bool,
    /// Whether to generate synthetic predecessors for imported commits.
    pub record_synthetic_predecessors: bool,
    /// Per-remote patterns whether to track bookmarks automatically.
    pub remote_auto_track_bookmarks: HashMap<RemoteNameBuf, StringMatcher>,
}
```
No from_settings constructor in jj-lib — consumer builds it by hand (copy abandon_unreachable_commits/record_synthetic_predecessors from GitSettings). Delta vs 0.40: auto_local_bookmark bool removed; record_synthetic_predecessors added. StringMatcher is jj_lib::str_util::StringMatcher (e.g. StringExpression::all().to_matcher() to auto-track everything on a remote).

### GitImportStats  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:553)
```rust
pub struct GitImportStats {
    pub abandoned_commits: Vec<Commit>,
    pub rewritten_commit_ids: HashSet<CommitId>,
    pub changed_remote_bookmarks: Vec<GitImportRefUpdate>,
    pub changed_remote_tags: Vec<GitImportRefUpdate>,
    pub failed_ref_names: Vec<BString>,
}
```
Delta vs 0.40: abandoned_commits was Vec<CommitId>, now Vec<Commit>; changed_remote_* were Vec<(RemoteRefSymbolBuf, (RemoteRef, RefTarget))>, now Vec<GitImportRefUpdate>; rewritten_commit_ids is new. GitImportRefUpdate (git.rs:531): pub struct GitImportRefUpdate { pub symbol: RemoteRefSymbolBuf, pub old_remote_ref: RemoteRef, pub new_target: RefTarget }.

### import_head [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:1174)
```rust
pub async fn import_head(
    mut_repo: &mut MutableRepo,
    workspace_name: &WorkspaceName,
    workspace_root: &Path,
) -> Result<(), GitImportError>
```
ASYNC. Delta vs 0.40: was import_head(mut_repo) only; now takes workspace_name + workspace_root (per-workspace HEAD via git_backend.open_git_repo_at_workdir). Use WorkspaceName::DEFAULT for the default workspace. Also new: import_head_commit(mut_repo) -> Result<Option<Commit>, GitImportError> at git.rs:1217 (async, no view update).

### export_refs / export_some_refs  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:1327)
```rust
pub fn export_refs(mut_repo: &mut MutableRepo) -> Result<GitExportStats, GitExportError>

pub fn export_some_refs(
    mut_repo: &mut MutableRepo,
    git_ref_filter: impl Fn(GitRefKind, RemoteRefSymbol<'_>) -> bool,
) -> Result<GitExportStats, GitExportError>
```
SYNC (not asyncified). YES — still takes &mut MutableRepo, so it must run on tx.repo_mut() before tx.commit; it writes Git refs immediately and records the exported state in the mutable view. GitExportStats (git.rs:1291): { pub failed_bookmarks: Vec<(RemoteRefSymbolBuf, FailedRefExportReason)>, pub failed_tags: Vec<(RemoteRefSymbolBuf, FailedRefExportReason)> }. FailedRefExportReason (git.rs:1261) variants: InvalidGitName, ConflictedOldState, OnRootCommit, DeletedInJjModifiedInGit, AddedInJjAddedInGit, ModifiedInJjDeletedInGit, FailedToDelete(#[source] Box<dyn Error+Send+Sync>), FailedToSet(#[source] Box<dyn Error+Send+Sync>). Also detaches HEAD in the main repo and all worktrees when the checked-out branch moves (git.rs:1348-1399).

### reset_head [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:1841)
```rust
pub async fn reset_head(
    mut_repo: &mut MutableRepo,
    workspace_name: &WorkspaceName,
    workspace_root: &Path,
    wc_commit: &Commit,
) -> Result<(), GitResetHeadError>
```
ASYNC. Delta vs 0.36/0.40: gained workspace_name + workspace_root params. Sets HEAD to wc_commit's FIRST parent (RefTarget::absent if parent is root), clears merge/rebase state files, then rebuilds the Git index from the parent tree (conflict stages included). GitResetHeadError (git.rs:1822): Backend(#[from] BackendError) | Git(Box<dyn Error+Send+Sync>) | UpdateHeadRef(#[source] Box<gix::reference::edit::Error>) | UnexpectedBackend(#[from] UnexpectedGitBackendError). UpdateHeadRef is the 'HEAD moved concurrently' case suitable for downgrade-to-warning.

### update_intent_to_add [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:2100)
```rust
pub async fn update_intent_to_add(
    repo: &dyn Repo,
    workspace_root: &Path,
    old_tree: &MergedTree,
    new_tree: &MergedTree,
) -> Result<(), GitResetHeadError>
```
Present in 0.45.1. Call when the wc-commit-vs-parent diff changed (e.g. after snapshot) to keep `git status` sane in colocated repos.

### GitFetch::new  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:3108)
```rust
pub fn new(
    mut_repo: &'a mut MutableRepo,
    subprocess_options: GitSubprocessOptions,
    import_options: &'a GitImportOptions,
) -> Result<Self, UnexpectedGitBackendError>
```
Same as 0.40. Struct GitFetch<'a> at git.rs:3099 holds &'a mut MutableRepo for its whole lifetime — drop it before tx.commit.

### GitFetch::fetch  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:3131)
```rust
pub fn fetch(
    &mut self,
    remote_name: &RemoteName,
    ExpandedFetchRefSpecs {
        expr,
        refspecs: mut remaining_refspecs,
        negative_refspecs,
    }: ExpandedFetchRefSpecs,
    callback: &mut dyn GitSubprocessCallback,
    depth: Option<NonZeroU32>,
) -> Result<(), GitFetchError>
```
SYNC. DELTA vs 0.40: the 5th param `fetch_tags_override: Option<FetchTagsOverride>` was REMOVED — call with 4 args now: fetch(remote, refspecs, &mut cb, None). depth is std::num::NonZeroU32. Callback is the trait object (no RemoteCallbacks struct anywhere in 0.45.1). Subprocess always passes --no-tags; tags are fetched only via the GitFetchRefExpression.tag refspecs. GitFetchError (git.rs:2751): NoSuchRemote(RemoteNameBuf) | RemoteName(#[from] GitRemoteNameError) | RejectedUpdates(Vec<GitRefNameBuf>) | Subprocess(#[from] GitSubprocessError).

### expand_fetch_refspecs + GitFetchRefExpression  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:2810)
```rust
pub fn expand_fetch_refspecs(
    remote: &RemoteName,
    expr: GitFetchRefExpression,
) -> Result<ExpandedFetchRefSpecs, GitRefExpansionError>

pub struct GitFetchRefExpression {
    pub bookmark: StringExpression,
    pub tag: StringExpression,
}
```
GitFetchRefExpression at git.rs:2778; both fields are jj_lib::str_util::StringExpression (constructors: StringExpression::all()/none()/exact(s)/pattern(StringPattern)/union_all(vec)). ExpandedFetchRefSpecs (git.rs:2791) has all-private fields — opaque, consumed by value by GitFetch::fetch, so build one per fetch call. For 'fetch one branch': GitFetchRefExpression { bookmark: StringExpression::exact(name), tag: StringExpression::none() }. Also load_default_fetch_bookmarks(remote_name: &RefName..., git_repo: &gix::Repository) -> Result<(IgnoredRefspecs, StringExpression), GitDefaultRefspecError> at git.rs:3010 reads the remote's configured fetch refspecs.

### GitFetch::get_default_branch  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:3206)
```rust
pub fn get_default_branch(
    &self,
    remote_name: &RemoteName,
) -> Result<Option<RefNameBuf>, GitFetchError>
```
SYNC. Runs `git remote show` subprocess and parses 'HEAD branch:'.

### GitFetch::import_refs [ASYNC]  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:3225)
```rust
pub async fn import_refs(&mut self) -> Result<GitImportStats, GitImportError>
```
ASYNC. Imports only the refs matched by prior fetch() calls (bookmark/tag matchers), then clears the fetched list. block_on it.

### push_refs  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:3291)
```rust
pub fn push_refs(
    mut_repo: &mut MutableRepo,
    subprocess_options: GitSubprocessOptions,
    remote: &RemoteName,
    targets: &GitPushRefTargets,
    callback: &mut dyn GitSubprocessCallback,
    options: &GitPushOptions,
) -> Result<GitPushStats, GitPushError>
```
SYNC. This REPLACES 0.36's push_branches (push_branches/GitBranchPushTargets do not exist in 0.45.1). On success it (a) exports pushed bookmarks to refs/remotes/<remote>/<name> in the local git repo, (b) calls mut_repo.set_remote_bookmark(symbol, RemoteRef{target, state: RemoteRefState::Tracked}) for each pushed bookmark, and (c) updates remote tags — i.e. YES, the view is updated automatically; only export failures land in stats.unexported_bookmarks. GitPushError (git.rs:3255): NoSuchRemote | RemoteName | Subprocess | UnexpectedBackend.

### GitPushRefTargets / GitRefUpdate / GitPushOptions / GitPushStats  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:3267)
```rust
pub struct GitPushRefTargets {
    pub bookmarks: Vec<(RefNameBuf, Diff<Option<CommitId>>)>,
    pub tags: Vec<(RefNameBuf, Diff<Option<CommitId>>)>,
}

pub struct GitRefUpdate {
    pub qualified_name: GitRefNameBuf,
    pub targets: Diff<Option<gix::ObjectId>>,
}

pub struct GitPushOptions {
    /// `--push-option` arguments.
    pub remote_push_options: Vec<String>,
}

pub struct GitPushStats {
    pub pushed: Vec<GitRefNameBuf>,
    pub rejected: Vec<(GitRefNameBuf, Option<String>)>,
    pub remote_rejected: Vec<(GitRefNameBuf, Option<String>)>,
    pub unexported_bookmarks: Vec<(RemoteRefSymbolBuf, FailedRefExportReason)>,
}
```
GitPushRefTargets git.rs:3267 (Clone,Debug,Default); GitRefUpdate git.rs:3274; GitPushOptions git.rs:3285 (Clone,Debug,Default); GitPushStats git.rs:191 with all_ok() at :203 (true iff rejected+remote_rejected+unexported_bookmarks all empty) and some_exported() at :211. Diff is jj_lib::merge::Diff { pub before: T, pub after: T } (jj-core-0.45.1/src/merge.rs:41, Diff::new(before, after) at :50) — this replaces 0.36's BookmarkPushUpdate {old_target,new_target}. Bookmark names are bare RefNameBuf (no refs/heads/ prefix; push_refs adds it). Diff.before = expected current position on remote (from tracked remote ref; None = must not exist), Diff.after = new target (None = delete).

### push_updates (force-with-lease mechanics)  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:3405)
```rust
pub fn push_updates(
    repo: &dyn Repo,
    subprocess_options: GitSubprocessOptions,
    remote_name: &RemoteName,
    updates: &[GitRefUpdate],
    callback: &mut dyn GitSubprocessCallback,
    options: &GitPushOptions,
) -> Result<GitPushStats, GitPushError>
```
SYNC, lower-level: does NOT update the view. Every update becomes `--force-with-lease=<refname>:<expected-oid-or-empty>` (RefToPush::to_git_lease, git.rs:326; spawn_push builds `git push --porcelain --no-verify` with non-forced refspecs, git_subprocess.rs:257-298). Empty expected = ref must not exist on remote. Lease failures surface as stats.rejected; remote hook rejections as stats.remote_rejected.

### GitSubprocessCallback (trait) + no-op impl requirements  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git_subprocess.rs:677)
```rust
pub trait GitSubprocessCallback {
    fn needs_progress(&self) -> bool;
    fn progress(&mut self, progress: &GitProgress) -> io::Result<()>;
    fn local_sideband(
        &mut self,
        message: &[u8],
        term: Option<GitSidebandLineTerminator>,
    ) -> io::Result<()>;
    fn remote_sideband(
        &mut self,
        message: &[u8],
        term: Option<GitSidebandLineTerminator>,
    ) -> io::Result<()>;
}
```
This is the 0.45.1 callback type — a trait object, not the old git2 RemoteCallbacks struct (which no longer exists). No default method bodies: a no-op impl must implement all four (needs_progress -> false, others -> Ok(())). Publicly nameable as jj_lib::git::GitSubprocessCallback (re-exported at git.rs:49-51 along with GitProgress and GitSidebandLineTerminator); the git_subprocess module itself is PRIVATE (`mod git_subprocess;` in lib.rs:57).

### GitSubprocessContext::create_command (env/auth behavior)  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git_subprocess.rs:99)
```rust
fn create_command(&self) -> Command  // private; behavior:
// Command::new(&self.options.executable_path)
//   .args(["-c","core.fsmonitor=false"]).args(["-c","submodule.recurse=false"])
//   .arg("--git-dir").arg(&self.git_dir)
//   .env_remove("LC_ALL").env_remove("LANGUAGE").env("LC_MESSAGES","C")
//   .stdin(Stdio::null()).stderr(Stdio::piped());
// git_cmd.envs(&self.options.environment);
```
YES: parent environment is inherited (std Command default), stdin is Stdio::null(), and GitSubprocessOptions.environment is applied LAST via .envs() (git_subprocess.rs:140) — so inserting ("GIT_ASKPASS", path) and e.g. ("GIT_TERMINAL_PROMPT","0") into subprocess_options.environment is the supported token-injection route. GitSubprocessContext itself is pub(crate) (git_subprocess.rs:78); consumers only pass GitSubprocessOptions into GitFetch::new/push_refs/push_updates.

### GitBackend::init_colocated  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git_backend.rs:251)
```rust
pub fn init_colocated(
    settings: &UserSettings,
    store_path: &Path,
    workspace_root: &Path,
    object_hash: gix::hash::Kind,
) -> Result<Self, Box<GitBackendInitError>>
```
Delta vs 0.36: new required param object_hash: gix::hash::Kind (use gix::hash::Kind::Sha1). Related: init_external(settings, store_path, git_repo_path) at :280 (no object_hash — opens existing), load(settings, store_path) at :335, git_repo() -> gix::Repository at :372, open_git_repo_at_workdir(&self, path: &Path) -> Result<gix::Repository, GitRepoAtWorkdirError> at :378, git_repo_path() -> &Path at :415. Usually you init via Workspace::init_colocated_git rather than calling GitBackend directly.

### get_git_backend / get_git_repo  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/git.rs:422)
```rust
pub fn get_git_backend(store: &Store) -> Result<&GitBackend, UnexpectedGitBackendError>

pub fn get_git_repo(store: &Store) -> Result<gix::Repository, UnexpectedGitBackendError>
```
get_git_repo at git.rs:427; returns thread-local gix::Repository. UnexpectedGitBackendError (git.rs:419) is a unit struct error. Delta vs 0.36: same names; 0.36 took &Store too.

### ref name types (jj_lib::ref_name = jj_core::ref_name)  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-core-0.45.1/src/ref_name.rs:51)
```rust
pub struct GitRefNameBuf(String);   // :51
pub struct GitRefName(str);         // :59
pub struct RefNameBuf(String);      // :67
pub struct RefName(str);            // :75
pub struct RemoteNameBuf(String);   // :83
pub struct RemoteName(str);         // :91
pub struct WorkspaceNameBuf(String);// :100
pub struct WorkspaceName(str);      // :111

impl RefName {
    pub fn to_remote_symbol<'a>(&'a self, remote: &'a RemoteName) -> RemoteRefSymbol<'a>  // :311
}
impl WorkspaceName {
    pub const DEFAULT: &Self = Self::new("default");  // :318
}
pub struct RemoteRefSymbolBuf { pub name: RefNameBuf, pub remote: RemoteNameBuf }  // :349
pub struct RemoteRefSymbol<'a> { pub name: &'a RefName, pub remote: &'a RemoteName }  // :371
```
Module lives in jj-core-0.45.1 but is re-exported as jj_lib::ref_name (lib.rs:79) — import paths unchanged from 0.36-style `jj_lib::ref_name::*`. Construction: borrowed types via `RefName::new("main")` (const) or `"main".as_ref()`; owned via `RefNameBuf::from("main")` / `.to_owned()`. Deref: RefNameBuf -> &RefName. WorkspaceName::DEFAULT for default workspace. REMOTE_NAME_FOR_LOCAL_GIT_REPO: &RemoteName = RemoteName::new("git") at git.rs:88. parse_git_ref(full_name: &GitRefName) -> Option<(GitRefKind, RemoteRefSymbol<'_>)> at git.rs:340.

### refs.rs push classification  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src/refs.rs:216)
```rust
pub struct LocalAndRemoteRef<'a> {
    pub local_target: &'a RefTarget,
    pub remote_ref: &'a RemoteRef,
}  // refs.rs:200

pub enum RefPushAction {
    Update(Diff<Option<CommitId>>),
    AlreadyMatches,
    LocalConflicted,
    RemoteConflicted,
    RemoteUntracked,
}  // refs.rs:206

pub fn classify_ref_push_action(targets: LocalAndRemoteRef) -> RefPushAction  // refs.rs:216
```
RENAMES vs 0.36: classify_bookmark_push_action -> classify_ref_push_action; BookmarkPushAction -> RefPushAction; BookmarkPushUpdate is GONE — Update carries jj_lib::merge::Diff<Option<CommitId>> which plugs straight into GitPushRefTargets.bookmarks. LocalAndRemoteRef unchanged. Takes LocalAndRemoteRef by VALUE (it's Copy). RemoteUntracked returned when remote ref present but not tracked.

### merge::Diff  (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-core-0.45.1/src/merge.rs:41)
```rust
pub struct Diff<T> {
    pub before: T,
    pub after: T,
}
impl<T> Diff<T> {
    pub fn new(before: T, after: T) -> Self  // :50
    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> Diff<U>  // :55
    pub fn as_ref(&self) -> Diff<&T>  // :79
}
```
Import as jj_lib::merge::Diff. Used for GitPushRefTargets entries and RefPushAction::Update.

## Minimal flow
// all paths jj_lib::...; async fns driven with pollster::FutureExt::block_on
use std::collections::HashMap;
use jj_lib::git::{self, GitFetch, GitFetchRefExpression, GitImportOptions, GitPushOptions, GitPushRefTargets, GitSettings, GitSubprocessCallback, GitProgress, GitSidebandLineTerminator};
use jj_lib::merge::Diff;
use jj_lib::ref_name::{RefName, RefNameBuf, RemoteName, WorkspaceName};
use jj_lib::refs::{classify_ref_push_action, LocalAndRemoteRef, RefPushAction};
use jj_lib::str_util::StringExpression;
use pollster::FutureExt as _;

struct Quiet; // no-op callback: all 4 methods required
impl GitSubprocessCallback for Quiet {
    fn needs_progress(&self) -> bool { false }
    fn progress(&mut self, _: &GitProgress) -> std::io::Result<()> { Ok(()) }
    fn local_sideband(&mut self, _: &[u8], _: Option<GitSidebandLineTerminator>) -> std::io::Result<()> { Ok(()) }
    fn remote_sideband(&mut self, _: &[u8], _: Option<GitSidebandLineTerminator>) -> std::io::Result<()> { Ok(()) }
}

let git_settings = GitSettings::from_settings(repo.settings())?;
let mut sp = git_settings.to_subprocess_options();
sp.environment.insert("GIT_ASKPASS".into(), askpass_helper_path.into()); // token injection
let import_options = GitImportOptions {
    abandon_unreachable_commits: git_settings.abandon_unreachable_commits,
    record_synthetic_predecessors: git_settings.record_synthetic_predecessors,
    remote_auto_track_bookmarks: HashMap::new(),
};
let remote: &RemoteName = "origin".as_ref();
let mut tx = repo.start_transaction();

// --- colocated pre-sync: pick up external git changes ---
git::import_head(tx.repo_mut(), WorkspaceName::DEFAULT, workspace_root).block_on()?;
let _stats = git::import_refs(tx.repo_mut(), &import_options).block_on()?;

// --- fetch (trunk or stack branches) ---
{
    let mut fetch = GitFetch::new(tx.repo_mut(), sp.clone(), &import_options)?; // borrows tx mutably
    let expr = GitFetchRefExpression { bookmark: StringExpression::exact("main"), tag: StringExpression::none() };
    fetch.fetch(remote, git::expand_fetch_refspecs(remote, expr)?, &mut Quiet, None)?; // 4 args, sync
    let default = fetch.get_default_branch(remote)?; // Option<RefNameBuf>
    let _stats = fetch.import_refs().block_on()?; // async
} // fetch dropped -> tx usable again

// --- push a stack bookmark (force-with-lease) ---
let name: &RefName = "feat-1".as_ref();
let local = tx.repo().view().get_local_bookmark(name);
let remote_ref = tx.repo().view().get_remote_bookmark(name.to_remote_symbol(remote));
let diff: Diff<Option<CommitId>> = match classify_ref_push_action(LocalAndRemoteRef { local_target: local, remote_ref }) {
    RefPushAction::Update(diff) => diff, // before = lease expectation, after = new target
    other => return Err(...),
};
let targets = GitPushRefTargets { bookmarks: vec![(name.to_owned(), diff)], tags: vec![] };
let stats = git::push_refs(tx.repo_mut(), sp.clone(), remote, &targets, &mut Quiet, &GitPushOptions::default())?; // sync; view + refs/remotes updated automatically on success
if !stats.all_ok() { /* stats.rejected = lease failure; stats.remote_rejected = hook denial */ }

// --- export jj bookmark moves back to git, fix HEAD/index, commit op ---
let _export_stats = git::export_refs(tx.repo_mut())?; // sync, MUST be inside tx pre-commit
git::reset_head(tx.repo_mut(), WorkspaceName::DEFAULT, workspace_root, &wc_commit).block_on()?;
let repo = tx.commit("gt sync")?;

## Gotchas
- Async split: import_refs / import_some_refs / import_head / import_head_commit / GitFetch::import_refs / reset_head / update_intent_to_add are `async fn` (block_on them). fetch/get_default_branch/push_refs/push_updates/export_refs/export_some_refs are SYNC — do not wrap them in block_on.
- GitFetch::fetch lost its 5th parameter: 0.40's `fetch_tags_override: Option<FetchTagsOverride>` is gone in 0.45.1 (FetchTagsOverride type no longer exists). Call sites written for 0.40's `fetch(remote, refspecs, &mut cb, None, None)` must drop the last None.
- push_branches / GitBranchPushTargets / BookmarkPushUpdate / classify_bookmark_push_action / BookmarkPushAction (0.36 names) all no longer exist. Replacements: push_refs + GitPushRefTargets + merge::Diff<Option<CommitId>> + classify_ref_push_action + RefPushAction (refs.rs:200-233).
- reset_head and import_head gained required `workspace_name: &WorkspaceName, workspace_root: &Path` params vs 0.36/0.40 (worktree-aware; they reopen the repo at the workdir). Pass WorkspaceName::DEFAULT and the workspace root path.
- GitSettings no longer has auto_local_bookmark and there is no GitImportOptions::from_settings in jj-lib — gt must construct GitImportOptions by hand (jj-cli builds it from git.track-default-bookmark-on-* settings; jj-lib only exposes remote_auto_track_bookmarks: HashMap<RemoteNameBuf, StringMatcher>). Untracked-by-default applies to fetched bookmarks unless the remote has a matcher entry (tags are always Tracked).
- GIT_ASKPASS injection: GitSubprocessOptions.environment (HashMap<OsString, OsString>) is applied via Command::envs AFTER the LC_* scrubbing; parent env is inherited and stdin is Stdio::null() — the askpass helper cannot read the terminal, so it must emit the token on stdout non-interactively. Consider also setting GIT_TERMINAL_PROMPT=0.
- git_subprocess is a PRIVATE module; only GitSubprocessCallback, GitProgress, GitSidebandLineTerminator are re-exported (jj_lib::git::*). GitSubprocessError, GitSubprocessContext, GitFetchStatus are not publicly nameable — handle subprocess failures through GitFetchError::Subprocess / GitPushError::Subprocess variants via Display/source().
- GitSubprocessCallback has NO default method bodies — a no-op callback must implement all four methods, and needs_progress() should return false to skip --progress.
- push_refs updates state on success: it writes refs/remotes/<remote>/<name> in the local git repo AND sets the view's remote-tracking bookmark to Tracked at the new target — do not call export_refs/import for pushed refs afterward. push_updates (lower level) updates nothing. Check GitPushStats.all_ok(); rejected = --force-with-lease failure (expected `Diff.before` didn't match remote), remote_rejected = server-side rejection; both carry Option<String> reasons. GitPushStats gained `unexported_bookmarks: Vec<(RemoteRefSymbolBuf, FailedRefExportReason)>` vs 0.36.
- export_refs is still sync and still must run on tx.repo_mut() before tx.commit — it mutates the Git repo immediately and records exported state in the mutable view; it also detaches HEAD (main repo and all worktrees) if the checked-out branch is being moved, so run reset_head after it in colocated flows.
- ExpandedFetchRefSpecs is opaque (private fields) and consumed by value by fetch(); call expand_fetch_refspecs once per fetch invocation. Branch patterns containing ':' '^' '?' '[' ']' are rejected (GitRefExpansionError::InvalidBranchPattern).
- GitFetch::new borrows &mut MutableRepo for the struct's lifetime — scope it in a block so tx.commit / other tx.repo_mut() uses compile.
- GitImportStats shape changed: abandoned_commits is Vec<Commit> (was Vec<CommitId>), changed_remote_bookmarks/tags are Vec<GitImportRefUpdate{symbol, old_remote_ref, new_target}> (were tuple vecs), and rewritten_commit_ids: HashSet<CommitId> is new.
- GitBackend::init_colocated gained a required `object_hash: gix::hash::Kind` parameter (use gix::hash::Kind::Sha1 for normal repos).
- ref_name, merge, str_util, matchers, repo_path, object_id etc. are re-exports from the new jj-core crate (jj-lib 0.45.1 depends on jj-core 0.45.1); import paths `jj_lib::ref_name::...`, `jj_lib::merge::Diff`, `jj_lib::str_util::{StringExpression, StringPattern, StringMatcher}` still work. jj_lib::git is behind the default `git` cargo feature.
- Fetch tag/bookmark selection moved from Vec<StringPattern> (0.36) to StringExpression trees (StringExpression::exact/pattern/all/none/union_all; negations map to negative refspecs). GitFetchError::NoSuchRemote is returned from both fetch() and get_default_branch() if the remote isn't configured in .git/config.


---

# settings-revsets-rewrite

## Summary

Verified jj-lib 0.45.1 APIs at /Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src (companion crate jj-core-0.45.1 re-exported for merge/object_id/ref_name/repo_path/hex_util). Major drift vs 0.36: (1) refs.rs renamed classify_bookmark_push_action->classify_ref_push_action, BookmarkPushAction->RefPushAction, BookmarkPushUpdate replaced by jj_core::merge::Diff<Option<CommitId>>{before,after}; (2) Revset::iter()/RevsetIteratorExt are GONE - Revset::stream() returns LocalBoxStream (children-before-parents) and RevsetStreamExt::commits(store) adapts it; evaluate_programmatic() gone, use ResolvedRevsetExpression constructors + evaluate(repo); (3) Index::is_ancestor is now async and IndexResult-wrapped; (4) Commit::tree() is now sync+infallible returning MergedTree, tree_id()->tree_ids() returning &Merge<TreeId>, is_empty(repo) is async; (5) all rewrite/rebase/transaction-commit paths are async, driven with pollster::FutureExt::block_on (pollster is still the blessed executor; tokio only behind optional watchman feature); (6) UserSettings/CommitBuilder/rebase fns no longer take &UserSettings params (settings live on repo); (7) GitSettings lost auto_local_bookmark (per-remote RemoteSettings now); (8) bookmark names are typed (&RefName/RefNameBuf/RemoteRefSymbol) not &str; (9) StackedConfig::with_defaults() ships config/misc.toml which defaults user.name/email="" and operation.hostname/username="" so UserSettings::from_config succeeds without any user layer; (10) push API is git::push_refs(sync) with GitPushRefTargets of Diff<Option<CommitId>>.

## Key APIs

### StackedConfig::with_defaults  (jj-lib-0.45.1/src/config.rs:663)
```rust
pub fn with_defaults() -> Self
```
Defaults layer = single ConfigLayer parsed from src/config/misc.toml (config.rs:910-913). Keys supplied: fsmonitor.backend="none", fsmonitor.watchman.register-snapshot-trigger=false, git.abandon-unreachable-commits=true, git.executable-path="git", git.record-synthetic-predecessors=true, git.write-change-id-header=true, merge.hunk-level="line", merge.same-change="accept", operation.hostname="", operation.username="", signing.backend="none", signing.behavior="keep", signing.backends.{gpg,gpgsm,ssh}.*, ui.conflict-marker-style="diff", user.email="", user.name="", working-copy.eol-conversion="none", working-copy.exec-bit-change="auto", experimental.record-predecessors-in-commit=true. debug.* and signing.key intentionally unset.

### ConfigLayer::empty / parse / set_value  (jj-lib-0.45.1/src/config.rs:330,344,425)
```rust
pub fn empty(source: ConfigSource) -> Self; pub fn parse(source: ConfigSource, text: &str) -> Result<Self, ConfigLoadError>; pub fn set_value(&mut self, name: impl ToConfigNamePath, new_value: impl Into<ConfigValue>) -> Result<Option<ConfigValue>, ConfigUpdateError>
```
ConfigLayer fields pub: source, path: Option<PathBuf>, data: DocumentMut. Add to stack via StackedConfig::add_layer(impl Into<Arc<ConfigLayer>>) (config.rs:694). ToConfigNamePath accepts &'static str ("user.name") or [&str; N].

### ConfigSource variants  (jj-lib-0.45.1/src/config.rs:282-299)
```rust
pub enum ConfigSource { Default, System, EnvBase, User, Repo, Workspace, EnvOverrides, CommandArg }
```
Delta vs 0.36: Workspace variant added. Precedence is declaration order (Default lowest).

### UserSettings::from_config  (jj-lib-0.45.1/src/settings.rs:135)
```rust
pub fn from_config(config: StackedConfig) -> Result<Self, ConfigGetError>
```
Reads (required, all satisfied by misc.toml defaults): user.name, user.email, operation.hostname, operation.username, signing.behavior; optional: debug.randomness-seed, debug.commit-timestamp, debug.operation-timestamp, signing.key. operation.hostname/username default to "" — from_config never fails on with_defaults() alone, but set real values (User layer set_value) for meaningful op-log entries. Also with_new_config(&self, config) (settings.rs:174) preserves RNG state.

### GitSettings::from_settings  (jj-lib-0.45.1/src/git.rs:110)
```rust
pub fn from_settings(settings: &UserSettings) -> Result<Self, ConfigGetError>
```
struct GitSettings (git.rs:101-107): abandon_unreachable_commits: bool ("git.abandon-unreachable-commits"), executable_path: PathBuf ("git.executable-path"), record_synthetic_predecessors: bool ("git.record-synthetic-predecessors"), write_change_id_header: bool ("git.write-change-id-header"). Delta vs 0.36: auto_local_bookmark GONE — replaced by per-remote RemoteSettings (settings.rs:65, keys remotes.<name>.auto-track-bookmarks etc.). Companion GitSubprocessOptions::from_settings (git.rs:140) reads git.executable-path; has pub environment: HashMap<OsString,OsString>.

### RevsetExpression constructors  (jj-lib-0.45.1/src/revset.rs:390 (commit), 394 (commits), 611 (range), 495 (ancestors), 540 (descendants), 485 (roots), 480 (heads), 658 (minus), 643 (union), 653 (intersection), 648 (union_all))
```rust
pub fn commit(commit_id: CommitId) -> Arc<Self>; pub fn commits(commit_ids: Vec<CommitId>) -> Arc<Self>; pub fn range(self: &Arc<Self>, heads: &Arc<Self>) -> Arc<Self>; pub fn ancestors(self: &Arc<Self>) -> Arc<Self>; pub fn descendants(self: &Arc<Self>) -> Arc<Self>; pub fn roots(self: &Arc<Self>) -> Arc<Self>; pub fn heads(self: &Arc<Self>) -> Arc<Self>; pub fn minus(self: &Arc<Self>, other: &Arc<Self>) -> Arc<Self>; pub fn union(self: &Arc<Self>, other: &Arc<Self>) -> Arc<Self>
```
Generic over ExpressionState; write ResolvedRevsetExpression::commit(...) (type alias revset.rs:262) so the chain ends in a type that has .evaluate(). Also children() :535, parents() :490, dag_range_to() :588, connected() :597, latest(count) :472, NEW first_ancestors()/first_ancestors_at()/first_ancestors_range() :515-532 (first-parent spine — ideal for stacks), fork_point() :559, merge_point() :564.

### ResolvedRevsetExpression::evaluate  (jj-lib-0.45.1/src/revset.rs:697)
```rust
pub fn evaluate<'index>(self: Arc<Self>, repo: &'index dyn Repo) -> Result<Box<dyn Revset + 'index>, RevsetEvaluationError>
```
Consumes the Arc (clone if reusing). Optimizes then evaluates via repo.index().evaluate_revset. Delta vs 0.36: evaluate_programmatic() is gone; this is the replacement.

### Revset trait (iteration) [ASYNC]  (jj-lib-0.45.1/src/revset.rs:3420-3458)
```rust
pub trait Revset: fmt::Debug { fn stream<'a>(&self) -> LocalBoxStream<'a, Result<CommitId, RevsetEvaluationError>> where Self: 'a; fn commit_change_ids<'a>(&self) -> LocalBoxStream<'a, Result<(CommitId, ChangeId), RevsetEvaluationError>> where Self: 'a; fn is_empty(&self) -> Result<bool, RevsetEvaluationError>; fn count_estimate(&self) -> Result<(usize, Option<usize>), RevsetEvaluationError>; fn containing_fn<'a>(&self) -> Box<RevsetContainingFn<'a>> where Self: 'a; }
```
DELTA vs 0.36: iter() is GONE — stream() replaces it. Ordering: doc comment (revset.rs:3421) "Streams in topological order with children before parents" — yes, children first. Streams are LocalBoxStream (not Send).

### RevsetStreamExt::commits (replaces RevsetIteratorExt) [ASYNC]  (jj-lib-0.45.1/src/revset.rs:3465-3487)
```rust
pub trait RevsetStreamExt { fn commits(self, store: &Arc<Store>) -> impl Stream<Item = Result<Commit, RevsetEvaluationError>> + use<'_, Self>; }
```
DELTA vs 0.36: RevsetIteratorExt no longer exists. Canonical pattern used inside jj-lib (git.rs:794-799): expr.evaluate(repo)?.stream().commits(repo.store()).try_collect::<Vec<Commit>>() driven with .await or pollster block_on. Uses buffered(store.concurrency()).

### rebase_commit [ASYNC]  (jj-lib-0.45.1/src/rewrite.rs:257)
```rust
pub async fn rebase_commit(mut_repo: &mut MutableRepo, old_commit: Commit, new_parents: Vec<CommitId>) -> BackendResult<Commit>
```
DELTA vs 0.36: &UserSettings param dropped (settings come from repo); now async. Internally CommitRewriter::new(...).rebase().await?.write().await.

### CommitRewriter [ASYNC]  (jj-lib-0.45.1/src/rewrite.rs:268-460)
```rust
pub fn new(mut_repo: &'repo mut MutableRepo, old_commit: Commit, new_parents: Vec<CommitId>) -> Self [:276, sync]; pub fn parents_changed(&self) -> bool [:331, sync]; pub async fn rebase(self) -> BackendResult<CommitBuilder<'repo>> [:448]; pub async fn rebase_with_empty_behavior(self, empty: EmptyBehavior) -> BackendResult<Option<CommitBuilder<'repo>>> [:361]; pub fn reparent(self) -> CommitBuilder<'repo> [:455, sync]; pub async fn simplify_ancestor_merge(&mut self) -> IndexResult<()> [:337]; pub fn abandon(self) [:352, sync]
```
DELTA vs 0.36: rebase/rebase_with_empty_behavior now async; reparent no longer takes settings and is infallible. rebase_with_empty_behavior returns None when the commit was abandoned (only ever with single new parent).

### EmptyBehavior / RebaseOptions / RewriteRefsOptions / RebasedCommit  (jj-lib-0.45.1/src/rewrite.rs:539-577, 462-466)
```rust
pub enum EmptyBehavior { #[default] Keep, AbandonNewlyEmpty, AbandonAllEmpty } ; pub struct RebaseOptions { pub empty: EmptyBehavior, pub rewrite_refs: RewriteRefsOptions, pub simplify_ancestor_merge: bool } ; pub struct RewriteRefsOptions { pub delete_abandoned_bookmarks: bool } ; pub enum RebasedCommit { Rewritten(Commit), Abandoned { parent_id: CommitId } }
```
All Default-derivable. rebase_commit_with_options(rewriter, &RebaseOptions) -> BackendResult<RebasedCommit> at rewrite.rs:468 is async.

### classify_ref_push_action (was classify_bookmark_push_action)  (jj-lib-0.45.1/src/refs.rs:216)
```rust
pub fn classify_ref_push_action(targets: LocalAndRemoteRef) -> RefPushAction
```
DELTA vs 0.36: renamed from classify_bookmark_push_action; BookmarkPushAction -> RefPushAction (refs.rs:205): Update(Diff<Option<CommitId>>), AlreadyMatches, LocalConflicted, RemoteConflicted, RemoteUntracked. BookmarkPushUpdate struct is GONE — replaced by jj_core::merge::Diff<T> { pub before: T, pub after: T } with Diff::new(before, after) (jj-core-0.45.1/src/merge.rs:41-52).

### LocalAndRemoteRef  (jj-lib-0.45.1/src/refs.rs:198-203)
```rust
pub struct LocalAndRemoteRef<'a> { pub local_target: &'a RefTarget, pub remote_ref: &'a RemoteRef }
```
Same shape as 0.36. Obtain per-remote pairs from View::local_remote_bookmarks(&self, remote_name: &RemoteName) -> impl Iterator<Item = (&RefName, LocalAndRemoteRef<'_>)> (view.rs:304).

### Index::is_ancestor [ASYNC]  (jj-lib-0.45.1/src/index.rs:126-130)
```rust
async fn is_ancestor(&self, ancestor_id: &CommitId, descendant_id: &CommitId) -> IndexResult<bool>
```
DELTA vs 0.36: was sync returning bare bool; now async + IndexResult (IndexResult<T> = Result<T, IndexError>, index.rs:64). Access via repo.index() -> &dyn Index (Repo trait, repo.rs:130); drive with .block_on(). Also async: heads(), common_ancestors(), has_id(), changed_paths_in_commit().

### ObjectId trait / CommitId / ChangeId display  (jj-core-0.45.1/src/object_id.rs:25-34; jj-lib-0.45.1/src/backend.rs:47-84)
```rust
pub trait ObjectId { fn object_type(&self) -> String; fn as_bytes(&self) -> &[u8]; fn to_bytes(&self) -> Vec<u8>; fn hex(&self) -> String; } ; id_type!(pub CommitId { hex() }); id_type!(pub ChangeId { reverse_hex() }); impl ChangeId { pub fn try_from_reverse_hex(hex: impl AsRef<[u8]>) -> Option<Self> [backend.rs:76]; pub fn reverse_hex(&self) -> String [backend.rs:82] }
```
Display for CommitId pads hex(); Display for ChangeId pads reverse_hex() (the z-k alphabet users see, e.g. "zzzzzzzz"). ObjectId::hex() on ChangeId still gives forward hex — use reverse_hex()/Display for user-facing ids. from_hex(&'static str) panics on bad input; try_from_hex for runtime strings. HexPrefix::try_from_reverse_hex also exists (object_id.rs:173).

### Commit::tree / tree_ids / is_empty / parents [ASYNC]  (jj-lib-0.45.1/src/commit.rs:120,128,160,110)
```rust
pub fn tree(&self) -> MergedTree; pub fn tree_ids(&self) -> &Merge<TreeId>; pub async fn is_empty(&self, repo: &dyn Repo) -> BackendResult<bool>; pub async fn parents(&self) -> BackendResult<Vec<Self>>
```
DELTA vs 0.36: tree() is now SYNC and infallible (constructs MergedTree from stored ids+labels lazily); tree_id() renamed tree_ids() and MergedTreeId type replaced by Merge<TreeId>; is_empty now async and consults the index fast-path; parents() async returning Vec (was an iterator of Results). has_conflict() sync :167.

### MergedTree::tree_ids_and_labels  (jj-lib-0.45.1/src/merged_tree.rs:129,134)
```rust
pub fn tree_ids_and_labels(&self) -> (&Merge<TreeId>, &ConflictLabels); pub fn into_tree_ids_and_labels(self) -> (Merge<TreeId>, ConflictLabels)
```
Present in 0.45.1. Also tree_ids() :111, MergedTree::merge(merge: Merge<(Self, String)>) -> BackendResult<Self> is async :339 (each side now carries a conflict-label String — new vs 0.36).

### Store::get_commit (sync) / get_commit_async  (jj-lib-0.45.1/src/store.rs:152,156)
```rust
pub fn get_commit(self: &Arc<Self>, id: &CommitId) -> BackendResult<Commit>; pub async fn get_commit_async(self: &Arc<Self>, id: &CommitId) -> BackendResult<Commit>
```
Sync variant still present; it is literally get_commit_async(id).block_on() via pollster (store.rs:28 use pollster::FutureExt as _). Commits are LRU-cached in the store.

### MutableRepo rewrite plumbing [ASYNC]  (jj-lib-0.45.1/src/repo.rs:1004,1010,1097,1406,1496,1531)
```rust
pub fn new_commit(&mut self, parents: Vec<CommitId>, tree: MergedTree) -> CommitBuilder<'_>; pub fn rewrite_commit(&mut self, predecessor: &Commit) -> CommitBuilder<'_>; pub fn new_parents(&self, old_ids: &[CommitId]) -> Vec<CommitId>; pub async fn transform_descendants(&mut self, roots: Vec<CommitId>, callback: impl AsyncFnMut(CommitRewriter) -> BackendResult<()>) -> BackendResult<()>; pub async fn rebase_descendants_with_options(&mut self, immutable: &Arc<ResolvedRevsetExpression>, options: &RebaseOptions, progress: impl FnMut(Commit, RebasedCommit)) -> BackendResult<()>; pub async fn rebase_descendants(&mut self) -> BackendResult<usize>
```
DELTA vs 0.36: no &UserSettings params anywhere; rebase_descendants* now async; rebase_descendants_with_options gained required immutable revset param (pass &RevsetExpression::none() to match old behavior, cf. repo.rs:1534) and progress callback. transform_descendants is the restack primitive (used with CommitRewriter callback). Bookmark accessors on MutableRepo: get_local_bookmark(&RefName) :1782, set_local_bookmark_target(&RefName, RefTarget) :1786, get/set_remote_bookmark(RemoteRefSymbol...) :1806/1810 — names are typed (jj-core ref_name.rs), not &str (0.36 delta).

### Transaction lifecycle [ASYNC]  (jj-lib-0.45.1/src/repo.rs:335; jj-lib-0.45.1/src/transaction.rs:94,98,125)
```rust
pub fn start_transaction(self: &Arc<Self>) -> Transaction; pub fn repo(&self) -> &MutableRepo; pub fn repo_mut(&mut self) -> &mut MutableRepo; pub async fn commit(self, description: impl Into<String>) -> Result<Arc<ReadonlyRepo>, TransactionCommitError>
```
DELTA vs 0.36: start_transaction takes no settings; Transaction::commit is async. write() asserts !mut_repo.has_rewrites() (transaction.rs:141) — you MUST call repo_mut().rebase_descendants().await (or transform_descendants) after any rewrite_commit/record_abandoned before committing, or it panics.

### git::push_refs (push flow, replaces push_branches)  (jj-lib-0.45.1/src/git.rs:3266-3298)
```rust
pub struct GitPushRefTargets { pub bookmarks: Vec<(RefNameBuf, Diff<Option<CommitId>>)>, pub tags: Vec<(RefNameBuf, Diff<Option<CommitId>>)> } ; pub fn push_refs(mut_repo: &mut MutableRepo, subprocess_options: GitSubprocessOptions, remote: &RemoteName, targets: &GitPushRefTargets, callback: &mut dyn GitSubprocessCallback, options: &GitPushOptions) -> Result<GitPushStats, GitPushError>
```
SYNC (spawns git subprocess; gix-based push is gone). Diff.before = expected remote position (None = must not exist), Diff.after = new target (None = delete) — feed straight from RefPushAction::Update. GitSubprocessCallback trait (git_subprocess.rs:677): needs_progress/progress/local_sideband/remote_sideband — implement a no-op. GitFetch::fetch is also sync (git.rs:3131).

### pollster status  (jj-lib-0.45.1/src/store.rs:28 (and ~15 other modules); Cargo.toml:139)
```rust
use pollster::FutureExt as _;  ...  future.block_on()
```
pollster is a hard dependency and the blessed executor — jj-lib itself drives async internals with it (store.rs:153, revset.rs:2633, commit_builder.rs, local_working_copy.rs, git_backend.rs, ...). tokio is optional, only pulled by the watchman feature (Cargo.toml [features] watchman = [dep:tokio]); local_working_copy spins a private current-thread runtime only in that path. No tokio runtime needed for any API listed here.

## Minimal flow
// all block_on = pollster::FutureExt::block_on
// 1. Settings
let mut config = StackedConfig::with_defaults();               // config.rs:663
let mut layer = ConfigLayer::empty(ConfigSource::User);        // config.rs:330
layer.set_value("user.name", "Lita")?; layer.set_value("user.email", "lita@moment.dev")?;
layer.set_value("operation.hostname", hostname)?; layer.set_value("operation.username", username)?;
config.add_layer(layer);
let settings = UserSettings::from_config(config)?;             // settings.rs:135
// 2. Load colocated workspace (workspace.rs) -> repo: Arc<ReadonlyRepo>
//    Workspace::load(&settings, path, &default_working_copy_factories(), ...) then
//    workspace.repo_loader().load_at_head().block_on()?
// 3. Query a stack (trunk..head, children-before-parents)
let trunk = ResolvedRevsetExpression::commit(trunk_id);        // revset.rs:390
let stack = trunk.range(&ResolvedRevsetExpression::commit(head_id)); // revset.rs:611
let revset = stack.evaluate(repo.as_ref())?;                   // revset.rs:697
let commits: Vec<Commit> = revset.stream()                     // revset.rs:3423, children first
    .commits(repo.store())                                     // revset.rs:3466 (RevsetStreamExt)
    .try_collect().block_on()?;                                // futures::TryStreamExt
// 4. Rewrite/rebase in a transaction
let mut tx = repo.start_transaction();                          // repo.rs:335
let mut_repo = tx.repo_mut();
let new_commit = rebase_commit(mut_repo, old_commit, vec![new_parent_id]).block_on()?; // rewrite.rs:257
// or granular: CommitRewriter::new(mut_repo, c, parents).rebase().block_on()?.set_description(d).write().block_on()?
mut_repo.rebase_descendants().block_on()?;                      // repo.rs:1531 — MANDATORY before commit
mut_repo.set_local_bookmark_target(RefName::new("feat-1"), RefTarget::normal(new_commit.id().clone()));
let repo = tx.commit("gt restack").block_on()?;                 // transaction.rs:125
// 5. Classify + push bookmarks
for (name, pair) in repo.view().local_remote_bookmarks(RemoteName::new("origin")) { // view.rs:304
    match classify_ref_push_action(pair) {                      // refs.rs:216
        RefPushAction::Update(diff) => bookmarks.push((name.to_owned(), diff)), // Diff<Option<CommitId>>
        _ => {}
    }
}
// ancestry check: repo.index().is_ancestor(&a, &b).block_on()? (index.rs:126, IndexResult)
let mut tx = repo.start_transaction();
let stats = git::push_refs(tx.repo_mut(), GitSubprocessOptions::from_settings(&settings)?,
    RemoteName::new("origin"), &GitPushRefTargets { bookmarks, tags: vec![] },
    &mut NoopCallback, &GitPushOptions::default())?;            // git.rs:3291, sync
tx.commit("gt submit").block_on()?;

## Gotchas
- Renames vs 0.36 that will not appear in grep: classify_bookmark_push_action->classify_ref_push_action, BookmarkPushAction->RefPushAction, BookmarkPushUpdate->jj_core::merge::Diff<Option<CommitId>>{before,after}, Revset::iter()->Revset::stream(), RevsetIteratorExt->RevsetStreamExt, evaluate_programmatic()->evaluate(), Commit::tree_id()->tree_ids(), push_branches/GitBranchPushTargets->push_refs/GitPushRefTargets.
- jj-lib 0.45.1 is split: merge (Diff, Merge), object_id (ObjectId, HexPrefix), ref_name (RefName, RefNameBuf, RemoteName, RemoteRefSymbol), repo_path, hex_util, str_util, graph, dag_walk live in the jj-core crate but are re-exported at the same jj_lib::* paths (lib.rs pub use jj_core::...), so imports keep working — but rustdoc/source lookups must go to jj-core-0.45.1.
- Transaction::write asserts !mut_repo.has_rewrites() and PANICS (transaction.rs:141-144) if you rewrite_commit/record_abandoned and forget rebase_descendants().block_on() before tx.commit().
- Revset::stream() borrows the Box<dyn Revset> — bind the revset to a variable that outlives the stream, and the streams are LocalBoxStream (not Send): pollster block_on is fine, tokio::spawn is not.
- ResolvedRevsetExpression::evaluate takes self: Arc<Self> by value — clone the Arc if you need the expression again.
- Bookmark/remote names are typed everywhere: RefName::new("foo") / RemoteName::new("origin") / RemoteRefSymbol { name, remote }; String/&str no longer compile against view/bookmark APIs.
- ChangeId user-facing form is reverse_hex() (z-k alphabet, also its Display); ObjectId::hex() on a ChangeId produces forward hex that will NOT match what jj CLI prints. CommitId::from_hex takes &'static str and panics on bad input — use try_from_hex for user input.
- misc.toml defaults user.name/email to empty strings, so commits get empty signatures unless you add a User config layer; likewise operation.hostname/username default to "" (from_config succeeds either way).
- Index::is_ancestor and friends are async + IndexResult now; CommitRewriter::simplify_ancestor_merge returns IndexResult which jj-lib itself wraps as BackendError::Other when needed (rewrite.rs:477-478).
- GitSettings no longer has auto_local_bookmark; auto-tracking is per-remote via settings.remote_settings() / remotes.<name>.auto-track-bookmarks (settings.rs:61-90). git.subprocess flag is gone — subprocess git is the only transport, configured by git.executable-path.
- rebase_descendants_with_options gained a required immutable: &Arc<ResolvedRevsetExpression> first param (pass &RevsetExpression::none() for old behavior) — positional-arg code from 0.36 will mis-compile in confusing ways.
- New 0.45.1 helpers material to a Graphite-style gt: RevsetExpression::first_ancestors/_at/_range (revset.rs:515) first-parent stack spine; fork_point()/merge_point() (revset.rs:559/564); MutableRepo::transform_descendants (repo.rs:1406) callback restack primitive; rewrite::compute_move_commits/move_commits + MoveCommitsLocation/Stats (rewrite.rs:656-667) high-level subtree moves; evolution.rs walk_predecessors (evolution.rs:86) commit predecessor history; converge.rs converge_change (converge.rs:150) auto-merge divergent changes; graph_dominators.rs DominatorFinder::find_closest_common_dominator (graph_dominators.rs:249); View::local_remote_bookmarks (view.rs:304) feeds classify_ref_push_action directly; absorb.rs/fix.rs/bisect.rs now in-lib.


---

# GAP-CRITIC-045

## Summary

Resolved the 5 remaining engine holes against jj-lib 0.45.1 source (/Users/lita/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jj-lib-0.45.1/src) cross-checked with the SAME-version canonical consumer jj-cli 0.45.1 at /Users/lita/src/jj/cli/src. (1) Per-command colocated write sequence: NOTHING in jj-lib auto-exports (grep proves export_refs/reset_head have zero callers outside git.rs); jj-cli's finish_transaction (cli_util.rs:2392-2427) runs git::reset_head(tx.repo_mut(), ws_name, ws_root, &new_wc_commit) FIRST, then git::export_refs(tx.repo_mut()), both BEFORE tx.commit(), then updates the working copy AFTER commit. (2) Commit→finish plumbing: tx.commit(desc).block_on() -> Arc<ReadonlyRepo>; the op id for LockedWorkspace::finish is repo.op_id().clone() (repo.rs:298); snapshot flow holds the LockedWorkspace across the whole tx and calls locked_ws.finish(repo.op_id().clone()) after commit (cli_util.rs:2268-2273); non-snapshot flow uses workspace.check_out(repo.op_id().clone(), old_tree.as_ref(), &new_commit) (cli_util.rs:3412-3431). (3) Colocated init on an existing clone (jj git init --colocate, commands/git/init.rs:235-316): Workspace::init_external_git → tx "import git refs" {import_refs with abandon_unreachable_commits=false + record_synthetic_predecessors=false; export_refs if colocated; commit} → then the import-git-head flow (cli_util.rs:1385-1450): tx {git::import_head; mut_repo.check_out(ws_name, &head_commit)}; locked_wc.reset(&wc_commit) (NOT check_out — files already on disk); rebase_descendants; tx.commit("import git head"); locked_ws.finish(op_id). (4) Default branch: GitFetch::get_default_branch(&self, remote_name: &RemoteName) -> Result<Option<RefNameBuf>, GitFetchError> (git.rs:3206, SYNC, spawns `git remote show`); clone.rs:392-455 calls it after fetch()+import_refs() and track_remote_bookmark()s it when git.track-default-bookmark-on-clone; per-fetch bookmark patterns come from load_default_fetch_bookmarks(remote, &git_repo) parsing .git/config refspecs (git.rs:3010). (5) Auth: GitSubprocessOptions{executable_path, environment: HashMap<OsString,OsString>} (git.rs:130-137) is the injection point — create_command (git_subprocess.rs:99-142) applies Command::envs(&options.environment) at line 140 AFTER LC scrubbing, with stdin=Stdio::null() (line 137); build via GitSettings::from_settings(settings)?.to_subprocess_options() (git.rs:120) then insert GIT_ASKPASS/GIT_TERMINAL_PROMPT=0; passed by value into GitFetch::new and push_refs (jj-cli never sets environment itself — it's explicitly documented for lib consumers).

## Key APIs

### Transaction::commit [ASYNC]  (jj-lib-0.45.1/src/transaction.rs:125-130)
```rust
pub async fn commit(
        self,
        description: impl Into<String>,
    ) -> Result<Arc<ReadonlyRepo>, TransactionCommitError>
```
Returns the NEW repo head; use its .op_id() for LockedWorkspace::finish and .operation() for records. write() asserts !has_rewrites (transaction.rs:141-144).

### Transaction::set_workspace_name  (jj-lib-0.45.1/src/transaction.rs:120-122)
```rust
pub fn set_workspace_name(&mut self, workspace_name: &WorkspaceName)
```
NEW vs 0.36: stamps op metadata with the acting workspace. jj-cli's start_repo_transaction (cli_util.rs:2939-2950) calls this plus tx.set_attribute(k,v) for the command args on every tx.

### ReadonlyRepo::op_id  (jj-lib-0.45.1/src/repo.rs:298-300)
```rust
pub fn op_id(&self) -> &OperationId
```
The plumbing from tx.commit to LockedWorkspace::finish: locked_ws.finish(repo.op_id().clone()). operation() at repo.rs:302; start_transaction(self: &Arc<Self>) -> Transaction at repo.rs:335.

### LockedWorkspace::finish [ASYNC]  (jj-lib-0.45.1/src/workspace.rs:495-499)
```rust
pub async fn finish(self, operation_id: OperationId) -> Result<(), WorkingCopyStateError>
```
Takes OperationId BY VALUE. Replaces workspace.working_copy internally. Call AFTER tx.commit with the new repo's op_id().clone().

### Workspace::check_out [ASYNC]  (jj-lib-0.45.1/src/workspace.rs:456-482)
```rust
pub async fn check_out(
        &mut self,
        operation_id: OperationId,
        old_tree: Option<&MergedTree>,
        commit: &Commit,
    ) -> Result<CheckoutStats, CheckoutError>
```
The post-commit working-copy update: jj-cli's update_working_copy (cli_util.rs:3412-3431) is exactly workspace.check_out(repo.op_id().clone(), old_commit.map(|c| c.tree()).as_ref(), new_commit). Errors with CheckoutError::ConcurrentCheckout if old_tree mismatches (tree_ids_and_labels comparison).

### LockedWorkingCopy::reset [ASYNC]  (jj-lib-0.45.1/src/working_copy.rs:130)
```rust
async fn reset(&mut self, commit: &Commit) -> Result<(), ResetError>;
```
Points wc state at commit WITHOUT touching files — the correct primitive when files on disk already match (colocated init on existing clone; git moved HEAD). check_out at :124, snapshot at :118-121, finish(self: Box<Self>, operation_id: OperationId) at :152-155. Trait is #[async_trait] (Send).

### git::reset_head [ASYNC]  (jj-lib-0.45.1/src/git.rs:1841-1846)
```rust
pub async fn reset_head(
    mut_repo: &mut MutableRepo,
    workspace_name: &WorkspaceName,
    workspace_root: &Path,
    wc_commit: &Commit,
) -> Result<(), GitResetHeadError>
```
Delta vs 0.36: +workspace_name/+workspace_root. Sets git HEAD to wc_commit's FIRST parent (root parent => unborn HEAD), clears merge/rebase state files, and rewrites the git index to the parent tree including intent-to-add entries (reset_index git.rs:1926) — so a separate update_intent_to_add call is NOT needed on the reset_head path. jj-cli tolerates GitResetHeadError::UpdateHeadRef as a warning (cli_util.rs:2742-2750).

### git::export_refs  (jj-lib-0.45.1/src/git.rs:1327-1329)
```rust
pub fn export_refs(mut_repo: &mut MutableRepo) -> Result<GitExportStats, GitExportError>
```
SYNC. Must run on tx.repo_mut() BEFORE tx.commit. Detaches HEAD (main repo AND all git worktrees, git.rs:1393-1400) if a moved/deleted branch is checked out — hence jj-cli's order: reset_head first, export_refs second (cli_util.rs:2392-2407). No caller inside jj-lib: gt must call it itself every colocated tx.

### git::update_intent_to_add [ASYNC]  (jj-lib-0.45.1/src/git.rs:2100-2105)
```rust
pub async fn update_intent_to_add(
    repo: &dyn Repo,
    workspace_root: &Path,
    old_tree: &MergedTree,
    new_tree: &MergedTree,
) -> Result<(), GitResetHeadError>
```
Used on the SNAPSHOT path when the wc commit is rewritten in place (parents unchanged, so no reset_head): jj-cli's export_working_copy_changes_to_git (cli_util.rs:2702-2714) = update_intent_to_add(repo, ws_root, &old_wc_tree, &new_wc_tree) then export_refs(mut_repo).

### Workspace::init_external_git [ASYNC]  (jj-lib-0.45.1/src/workspace.rs:259-263)
```rust
pub async fn init_external_git(
        user_settings: &UserSettings,
        workspace_root: &Path,
        git_repo_path: &Path,
    ) -> Result<(Self, Arc<ReadonlyRepo>), WorkspaceInitError>
```
For an EXISTING clone: git_repo_path = workspace_root.join(".git") (jj-cli init.rs:209-210 uses External mode when .git exists, even under --colocate; init_colocated_git is only for creating a brand-new git repo and takes object_hash: gix::hash::Kind instead). Working copy starts at the root commit; no object_hash param here.

### git::import_refs + GitImportOptions [ASYNC]  (jj-lib-0.45.1/src/git.rs:590-595 (options struct 521-528))
```rust
pub async fn import_refs(
    mut_repo: &mut MutableRepo,
    options: &GitImportOptions,
) -> Result<GitImportStats, GitImportError>
```
GitImportOptions{abandon_unreachable_commits: bool, record_synthetic_predecessors: bool, remote_auto_track_bookmarks: HashMap<RemoteNameBuf, StringMatcher>} — no Default. For the initial import on a big clone jj-cli forces abandon_unreachable_commits=false and record_synthetic_predecessors=false (init.rs:290-297); normal ops take both from GitSettings (git_util.rs:204-214).

### git::import_head [ASYNC]  (jj-lib-0.45.1/src/git.rs:1174-1178)
```rust
pub async fn import_head(
    mut_repo: &mut MutableRepo,
    workspace_name: &WorkspaceName,
    workspace_root: &Path,
) -> Result<(), GitImportError>
```
Sets view git_head + adds head commit; does NOT move the wc commit — caller does mut_repo.check_out(...) then locked_wc.reset(...). Sibling import_head_commit(mut_repo) -> Result<Option<Commit>, GitImportError> (git.rs:1217-1219) imports HEAD's commit without touching the view (used by jj-cli only for NON-colocated external-git init, init.rs:250).

### MutableRepo::check_out [ASYNC]  (jj-lib-0.45.1/src/repo.rs:1621-1632)
```rust
pub async fn check_out(
        &mut self,
        name: WorkspaceNameBuf,
        commit: &Commit,
    ) -> Result<Commit, CheckOutCommitError>
```
Creates a fresh empty wc commit on top of `commit` and edits it; returns the new wc commit — pass it to locked_wc.reset()/check_out() and set the view. This is what the colocated head-import flow uses (cli_util.rs:1404-1412).

### GitFetch::new  (jj-lib-0.45.1/src/git.rs:3108-3112)
```rust
pub fn new(
        mut_repo: &'a mut MutableRepo,
        subprocess_options: GitSubprocessOptions,
        import_options: &'a GitImportOptions,
    ) -> Result<Self, UnexpectedGitBackendError>
```
Takes subprocess_options (with your auth env) BY VALUE and borrows tx.repo_mut() for its lifetime — scope the GitFetch in a block before tx.commit (clone.rs:392-408 does exactly this).

### GitFetch::fetch  (jj-lib-0.45.1/src/git.rs:3131-3141)
```rust
pub fn fetch(
        &mut self,
        remote_name: &RemoteName,
        ExpandedFetchRefSpecs { .. }: ExpandedFetchRefSpecs,
        callback: &mut dyn GitSubprocessCallback,
        depth: Option<NonZeroU32>,
    ) -> Result<(), GitFetchError>
```
SYNC (subprocess). 4 args in 0.45.1 (0.40's fetch_tags_override is gone). Consumes ExpandedFetchRefSpecs by value; empty refspecs => Ok no-op; retries per failing refspec; rejected updates => GitFetchError::RejectedUpdates.

### GitFetch::get_default_branch  (jj-lib-0.45.1/src/git.rs:3206-3209)
```rust
pub fn get_default_branch(
        &self,
        remote_name: &RemoteName,
    ) -> Result<Option<RefNameBuf>, GitFetchError>
```
SYNC; spawns `git remote show <remote>` — network call, needs no prior fetch(), errors NoSuchRemote if unconfigured. Clone flow (clone.rs:404-455): fetch → import_refs().await → get_default_branch → if the default branch's remote ref is present and git.track-default-bookmark-on-clone, repo_mut().track_remote_bookmark(name.to_remote_symbol(remote)).await.

### git::load_default_fetch_bookmarks  (jj-lib-0.45.1/src/git.rs:3010-3013)
```rust
pub fn load_default_fetch_bookmarks(
    remote_name: &RemoteName,
    git_repo: &gix::Repository,
) -> Result<(IgnoredRefspecs, StringExpression), GitDefaultRefspecError>
```
Parses the remote's fetch refspecs from .git/config into a StringExpression for GitFetchRefExpression{bookmark, tag} — jj-cli's default when no patterns given (fetch.rs:217); tag default is StringExpression::all(). Feed into expand_fetch_refspecs(remote, expr) -> ExpandedFetchRefSpecs (git.rs:2810-2813).

### git::push_refs  (jj-lib-0.45.1/src/git.rs:3291-3298)
```rust
pub fn push_refs(
    mut_repo: &mut MutableRepo,
    subprocess_options: GitSubprocessOptions,
    remote: &RemoteName,
    targets: &GitPushRefTargets,
    callback: &mut dyn GitSubprocessCallback,
    options: &GitPushOptions,
) -> Result<GitPushStats, GitPushError>
```
SYNC; run on tx.repo_mut() then tx.commit (push.rs:598-605 then tx finish). On success it exports refs/remotes/* locally AND sets view remote bookmarks to Tracked — no import/export needed after. GitPushRefTargets{bookmarks: Vec<(RefNameBuf, Diff<Option<CommitId>>)>, tags: ...} derives Default; GitPushOptions{remote_push_options: Vec<String>} derives Default. Check stats.all_ok().

### GitSubprocessOptions (auth env injection)  (jj-lib-0.45.1/src/git.rs:130-137 (applied at git_subprocess.rs:140))
```rust
pub struct GitSubprocessOptions {
    pub executable_path: PathBuf,
    /// Used by consumers of jj-lib to set environment variables like
    /// GIT_ASKPASS (for authentication callbacks) or GIT_TRACE (for debugging).
    pub environment: HashMap<OsString, OsString>,
}
```
THE 0.45.1 auth mechanism (0.36's RemoteCallbacks/git2 credentials are gone). Build via GitSettings::from_settings(settings)?.to_subprocess_options() (git.rs:120-125, environment starts empty) or GitSubprocessOptions::from_settings (git.rs:140). create_command (git_subprocess.rs:99-142) does envs(&options.environment) AFTER env_remove(LC_ALL/LANGUAGE)+LC_MESSAGES=C, with stdin=Stdio::null() and --git-dir <path> -c core.fsmonitor=false -c submodule.recurse=false; parent env inherited. Askpass helper must print the secret on stdout non-interactively; also set GIT_TERMINAL_PROMPT=0. Consumed by value per GitFetch::new/push_refs call — clone it.

### WorkingCopyFreshness::check_stale [ASYNC]  (jj-lib-0.45.1/src/working_copy.rs:361-365)
```rust
pub async fn check_stale(
        locked_wc: &dyn LockedWorkingCopy,
        wc_commit: &Commit,
        repo: &ReadonlyRepo,
    ) -> Result<Self, OpStoreError>
```
Run right after start_working_copy_mutation, before snapshot. Variants: Fresh / Updated(Box<Operation>) => reload repo at that op (repo.reload_at(&op).await) and retry / WorkingCopyStale / SiblingOperation => error out like jj does.

### Workspace::start_working_copy_mutation [ASYNC]  (jj-lib-0.45.1/src/workspace.rs:446-454)
```rust
pub async fn start_working_copy_mutation(
        &mut self,
    ) -> Result<LockedWorkspace<'_>, WorkingCopyStateError>
```
Borrows &mut Workspace. Concurrency guard used by jj-cli (cli_util.rs:1525-1536): compare wc_commit.tree().tree_ids_and_labels() != locked_ws.locked_wc().old_tree().tree_ids_and_labels() => 'Concurrent working copy operation. Try again.'

## Minimal flow
Canonical gt command on a colocated repo (all .block_on() via pollster; ws_name = workspace.workspace_name().to_owned(), ws_root = workspace.workspace_root().to_owned()):
[A] LOAD: Workspace::load(&settings, &found_root, &StoreFactories from default_backend_factories(), &default_working_copy_factories()); repo = workspace.repo_loader().load_at_head().block_on()?.
[B] IMPORT GIT HEAD (colocated, cli_util.rs:1385-1450): tx = repo.start_transaction(); tx.set_workspace_name(&ws_name); git::import_head(tx.repo_mut(), &ws_name, &ws_root).block_on()?; if tx.repo().has_changes() && let Some(head_id) = tx.repo().view().git_head(&ws_name).as_normal() { head_commit = store.get_commit(head_id)?; wc_commit = tx.repo_mut().check_out(ws_name.clone(), &head_commit).block_on()?; locked_ws = workspace.start_working_copy_mutation().block_on()?; locked_ws.locked_wc().reset(&wc_commit).block_on()?; tx.repo_mut().rebase_descendants().block_on()?; repo = tx.commit("import git head").block_on()?; locked_ws.finish(repo.op_id().clone()).block_on()?; }
[C] SNAPSHOT (cli_util.rs:2088-2275): locked_ws = workspace.start_working_copy_mutation().block_on()?; WorkingCopyFreshness::check_stale(locked_ws.locked_wc(), &wc_commit, &repo).block_on()? (on Updated(op): repo = repo.reload_at(&op).block_on()?); (new_tree, _stats) = locked_ws.locked_wc().snapshot(&SnapshotOptions{base_ignores, progress: None, start_tracking_matcher: &EverythingMatcher, force_tracking_matcher: &NothingMatcher, max_new_file_size}).block_on()?; if new_tree.tree_ids_and_labels() != wc_commit.tree().tree_ids_and_labels() { tx = repo.start_transaction(); tx.set_is_snapshot(true); new_wc = tx.repo_mut().rewrite_commit(&wc_commit).set_tree(new_tree.clone()).write().block_on()?; tx.repo_mut().set_wc_commit(ws_name.clone(), new_wc.id().clone())?; tx.repo_mut().rebase_descendants().block_on()?; git::update_intent_to_add(tx.repo(), &ws_root, &wc_commit.tree(), &new_wc.tree()).block_on()?; git::export_refs(tx.repo_mut())?; repo = tx.commit("snapshot working copy").block_on()?; } locked_ws.finish(repo.op_id().clone()).block_on()?;  // finish even when tree unchanged, to persist stat cache
[D] IMPORT GIT REFS (cli_util.rs:1462-1494): tx = repo.start_transaction(); git::import_refs(tx.repo_mut(), &GitImportOptions{abandon_unreachable_commits, record_synthetic_predecessors, remote_auto_track_bookmarks}).block_on()?; if has_changes: rebase_descendants; then finish as [E].
[E] MUTATE + FINISH (finish_transaction, cli_util.rs:2340-2427): tx = repo.start_transaction(); ...gt mutations (rewrite_commit/set_local_bookmark_target/...)...; tx.repo_mut().rebase_descendants().block_on()?; new_wc_commit = store.get_commit(tx.repo().view().get_wc_commit_id(&ws_name).unwrap())?; git::reset_head(tx.repo_mut(), &ws_name, &ws_root, &new_wc_commit).block_on()?; git::export_refs(tx.repo_mut())?; repo = tx.commit(description).block_on()?; workspace.check_out(repo.op_id().clone(), Some(&old_wc_commit.tree()), &new_wc_commit).block_on()?.
FETCH: inside [E]'s tx before the reset/export tail: { git_repo = git::get_git_repo(store)?; (_ignored, bookmark_expr) = git::load_default_fetch_bookmarks(remote, &git_repo)?; specs = git::expand_fetch_refspecs(remote, GitFetchRefExpression{bookmark: bookmark_expr, tag: StringExpression::all()})?; let mut gf = GitFetch::new(tx.repo_mut(), subprocess_options.clone(), &import_options)?; gf.fetch(remote, specs, &mut cb, None)?; stats = gf.import_refs().block_on()?; default = gf.get_default_branch(remote)?; } then rebase_descendants + reset_head + export_refs + commit.
PUSH: inside [E]'s tx: stats = git::push_refs(tx.repo_mut(), subprocess_options_with_askpass_env, remote, &GitPushRefTargets{bookmarks: vec![(name, Diff{before: expected_remote_target, after: Some(new_id)})], tags: vec![]}, &mut cb, &GitPushOptions::default())?; check stats.all_ok(); then reset_head/export_refs/commit as usual.
COLOCATED INIT ON EXISTING CLONE (init.rs:235-316): (workspace, repo) = Workspace::init_external_git(&settings, root, &root.join(".git")).block_on()?; tx1 { git::import_refs(tx.repo_mut(), &GitImportOptions{abandon_unreachable_commits: false, record_synthetic_predecessors: false, remote_auto_track_bookmarks}).block_on()?; git::export_refs(tx.repo_mut())?; repo = tx.commit("import git refs").block_on()?; } then run [B] (import head + check_out + locked_wc.reset + finish); done — do NOT materialize files with Workspace::check_out here, the clone's files are already on disk.

## Gotchas
- Nothing in jj-lib 0.45.1 exports to git automatically: grep confirms export_refs/reset_head have zero callers outside git.rs. Every colocated transaction gt commits must end with the tail `reset_head(tx.repo_mut(), &ws_name, &ws_root, &new_wc_commit).block_on(); export_refs(tx.repo_mut()); tx.commit(...)` — reset_head FIRST, export_refs SECOND (jj-cli cli_util.rs:2392-2407); both on tx.repo_mut() strictly BEFORE tx.commit(). Exception: push_refs updates git + view itself; and a pure snapshot uses update_intent_to_add+export_refs instead of reset_head (wc parent unchanged).
- LockedWorkspace lifetime pattern: for snapshot/head-import the lock is taken BEFORE the transaction and finished AFTER tx.commit with locked_ws.finish(repo.op_id().clone()) — repo here is the Arc<ReadonlyRepo> returned by tx.commit. Do not drop the LockedWorkspace without finish(): changes to wc state are lost and the next mutation sees a stale old_operation_id. Also finish() even when the snapshot found no tree change (jj-cli does, cli_util.rs:2268-2273) to persist file-stat cache.
- reset_head rewrites the whole git index to the wc parent tree (including intent-to-add for wc-added files) and deletes MERGE_HEAD/rebase-merge/etc state (git.rs:1884-1923) — do not run it if you want to preserve an in-progress git merge; jj-cli treats GitResetHeadError::UpdateHeadRef as a mere warning because a racing `git checkout` can move HEAD (cli_util.rs:2742-2750).
- Colocated init/head-adoption uses LockedWorkingCopy::reset(&wc_commit) (working_copy.rs:130), NOT check_out: reset repoints wc state without touching files. Using check_out after init_external_git would rewrite every file in the clone (old_tree is the empty root tree). Same primitive when adopting a HEAD moved by plain git.
- GitFetch::get_default_branch is sync but hits the network (`git remote show`) — it also uses subprocess_options env, so askpass injection applies to it too; call it once during `gt init`/clone and cache the result (jj-cli sets the repo-level trunk() alias from it). GitFetch borrows tx.repo_mut() until dropped: scope it in a block or tx.commit won't compile.
- Auth env is per-call state: GitSubprocessOptions is consumed by value by GitFetch::new and push_refs, so build it once (GitSettings::from_settings(settings)?.to_subprocess_options()), insert GIT_ASKPASS + GIT_TERMINAL_PROMPT=0 into .environment, and clone per call. Environment values are applied after jj's LC_* scrubbing and can therefore override anything; stdin is null — the askpass program must be non-interactive. jj-cli 0.45.1 itself never populates environment, so there is no config key for it: it exists precisely for lib consumers like gt.
- Ordering nuance for `gt sync`-style commands: run import_head BEFORE snapshot (a git commit/checkout may have moved HEAD; snapshot would otherwise commit the new files onto the old parent) and import_refs AFTER snapshot (ref import can rebase the wc commit) — this is jj-cli's snapshot_impl order (cli_util.rs:1331-1359).
- tx.commit on a tx with zero changes still writes an operation; jj-cli guards every import/fetch tx with `if !tx.repo().has_changes() { return }` — copy that guard or the op log fills with empty ops. MutableRepo::has_changes is sync (view comparison).
