# repo-txn-oplog

## Summary

jj-lib 0.36's repo/transaction subsystem is a pure op-log database layer. RepoLoader (constructed from UserSettings + repo path (.jj/repo) + StoreFactories) loads a ReadonlyRepo at an op-log head or a specific Operation. ReadonlyRepo::start_transaction gives a Transaction wrapping a MutableRepo; you mutate its in-memory View (bookmarks, heads, wc_commit_ids, git_refs) and index, then Transaction::commit(description) performs exactly WRITE #1 of the three-writes story: it serializes the View to the op store (write_view), writes an op_store::Operation pointing at it (write_operation), writes the index, and atomically swaps the op-heads file (update_op_heads) — nothing else. transaction.rs imports neither working_copy/workspace nor git modules; op_store::View.wc_commit_ids is explicitly documented as only the commit that *should be* checked out, with .jj/working_copy as the separate source of truth. WRITE #2 (files on disk + .jj/working_copy state) must be done by the consumer via Workspace::check_out(operation_id, old_tree, commit) or start_working_copy_mutation()+LockedWorkspace::finish(op_id) in workspace.rs. WRITE #3 (git refs in the colocated .git) must be done via git::export_refs(mut_repo) (git.rs:967) inside a transaction before committing it. Concurrent op heads are reconciled lazily at load time: load_at_head resolves multiple heads by merging their operations into a new "reconcile divergent operations" merge op (views merged three-way via MutableRepo::merge), so gt sync/submit racing another jj process never corrupts anything.

## Key APIs

### RepoLoader::init_from_file_system  (/Users/lita/src/jj/lib/src/repo.rs:695)
```rust
pub fn init_from_file_system(settings: &UserSettings, repo_path: &Path, store_factories: &StoreFactories) -> Result<Self, StoreLoadError>
```
repo_path is the .jj/repo dir (workspace root/.jj/repo). Reads <repo_path>/{store,op_store,op_heads,index,submodule_store}/type files and picks factories by name. StoreFactories::default() (repo.rs:424) already registers GitBackend (behind 'git' feature, repo.rs:433-441), SimpleOpStore, SimpleOpHeadsStore, DefaultIndexStore — so for gt just pass &StoreFactories::default(). Normally you don't call this directly: Workspace::load gives you a Workspace whose .repo_loader() returns &RepoLoader (workspace.rs:405).

### RepoLoader::load_at_head  (/Users/lita/src/jj/lib/src/repo.rs:756)
```rust
pub fn load_at_head(&self) -> Result<Arc<ReadonlyRepo>, RepoLoaderError>
```
Calls op_heads_store::resolve_op_heads(...) with resolver = self.resolve_op_heads (repo.rs:835) which merges divergent op heads (see merge_operations). Then reads op.view()? and builds ReadonlyRepo via finish_load (loads index at that op).

### RepoLoader::load_at  (/Users/lita/src/jj/lib/src/repo.rs:767)
```rust
pub fn load_at(&self, op: &Operation) -> Result<Arc<ReadonlyRepo>, RepoLoaderError>
```
Load repo at a specific operation (time travel / undo). ReadonlyRepo::reload_at (repo.rs:336) and reload_at_head (repo.rs:331) are thin wrappers: `self.loader().load_at(operation)` / `self.loader().load_at_head()` — they return a NEW Arc<ReadonlyRepo>, they do not mutate self.

### RepoLoader::merge_operations  (/Users/lita/src/jj/lib/src/repo.rs:805)
```rust
pub fn merge_operations(&self, operations: Vec<Operation>, tx_description: Option<&str>) -> Result<Operation, RepoLoaderError>
```
For >1 ops: loads repo at first op, start_transaction, then for each other op: tx.merge_operation(other_op)? + tx.repo_mut().rebase_descendants()?; writes result with tx.write(desc)?.leave_unpublished(). Used by resolve_op_heads with description "reconcile divergent operations" (repo.rs:837).

### ReadonlyRepo::start_transaction  (/Users/lita/src/jj/lib/src/repo.rs:326)
```rust
pub fn start_transaction(self: &Arc<Self>) -> Result-free: Transaction  // body: MutableRepo::new(self.clone(), self.readonly_index(), &self.view); Transaction::new(mut_repo, self.settings())
```
Takes &Arc<Self>. Infallible. Other ReadonlyRepo accessors: op_id() repo.rs:289, operation() :293, view() :297, loader() :285, op_heads_store() :314, settings() :322.

### Transaction::repo_mut / repo / base_repo / set_tag / set_is_snapshot  (/Users/lita/src/jj/lib/src/transaction.rs:94)
```rust
pub fn repo_mut(&mut self) -> &mut MutableRepo; pub fn repo(&self) -> &MutableRepo (line 90); pub fn base_repo(&self) -> &Arc<ReadonlyRepo> (line 82); pub fn set_tag(&mut self, key: String, value: String) (line 86); pub fn set_is_snapshot(&mut self, is_snapshot: bool) (line 115)
```
All mutation goes through tx.repo_mut() -> &mut MutableRepo.

### Transaction::commit  (/Users/lita/src/jj/lib/src/transaction.rs:120)
```rust
pub fn commit(self, description: impl Into<String>) -> Result<Arc<ReadonlyRepo>, TransactionCommitError>
```
commit = self.write(description)?.publish(). RETURNS the new Arc<ReadonlyRepo> at the new operation — reassign your repo variable to it (`repo = tx.commit("...")?;`). WRITES (transaction.rs:130-167 write + 224-230 publish), in order: (1) asserts !mut_repo.has_rewrites() — you MUST call repo_mut().rebase_descendants() before commit if you rewrote/abandoned commits, else PANIC; (2) op_store().write_view(view.store_view()) -> ViewId; (3) op_store().write_operation(&op_store::Operation { view_id, parents, metadata, commit_predecessors }) -> OperationId; (4) index_store().write_index(mut_index, &operation); (5) publish(): op_heads_store.lock(), then update_op_heads(parent_ids, new_op_id) — atomically replaces parent head(s) with the new op head. THAT IS ALL. It does NOT touch files on disk, NOT .jj/working_copy state, NOT any .git ref. TransactionCommitError enum (transaction.rs:45): IndexStore | OpHeadsStore | OpStore.

### Transaction::write / UnpublishedOperation  (/Users/lita/src/jj/lib/src/transaction.rs:130)
```rust
pub fn write(mut self, description: impl Into<String>) -> Result<UnpublishedOperation, TransactionCommitError>; UnpublishedOperation::publish(self) -> Result<Arc<ReadonlyRepo>, TransactionCommitError> (line 224); leave_unpublished(self) -> Arc<ReadonlyRepo> (line 232); operation(&self) -> &Operation (line 220)
```
write() stores op+view but does not move op heads; op invisible at head until publish(). Useful talk beat: op-log commit is itself two-phase.

### Transaction::merge_operation  (/Users/lita/src/jj/lib/src/transaction.rs:98)
```rust
pub fn merge_operation(&mut self, other_op: Operation) -> Result<(), RepoLoaderError>
```
Finds closest common ancestor op via dag_walk, loads base/other repos, pushes other_op onto self.parent_ops (so committed op becomes a MERGE op with 2+ parents), and calls mut_repo.merge(&base_repo, &other_repo) (repo.rs:1831) which merges index + three-way-merges Views (merge_view repo.rs:1853: wc commits via trivial_merge keeping self on conflict, heads, bookmarks via merge_ref_targets).

### MutableRepo::new_commit  (/Users/lita/src/jj/lib/src/repo.rs:948)
```rust
pub fn new_commit(&mut self, parents: Vec<CommitId>, tree: MergedTree) -> CommitBuilder<'_>
```
Then .set_description(msg) etc. and .write() -> BackendResult<Commit>. For gt create: new_commit(vec![parent_id], tree).set_description(msg).write().

### MutableRepo::rewrite_commit  (/Users/lita/src/jj/lib/src/repo.rs:954)
```rust
pub fn rewrite_commit(&mut self, predecessor: &Commit) -> CommitBuilder<'_>
```
For gt modify (amend): rewrite_commit(&old).set_tree_id/set_description(...).write(); records rewrite in parent_mapping, so MUST call rebase_descendants() afterward (repo.rs:1428: pub fn rebase_descendants(&mut self) -> BackendResult<usize>) or Transaction::write panics.

### MutableRepo::set_local_bookmark_target  (/Users/lita/src/jj/lib/src/repo.rs:1676)
```rust
pub fn set_local_bookmark_target(&mut self, name: &RefName, target: RefTarget)
```
Adds target.added_ids() as view heads, then view.set_local_bookmark_target (view.rs:176 — absent target deletes the bookmark), marks view dirty. Getter: get_local_bookmark(&self, name: &RefName) -> RefTarget (repo.rs:1672, returns owned clone; RefTarget::absent() if missing). RefTarget constructors: RefTarget::normal(id: CommitId) op_store.rs:82, ::absent() :64, .as_normal() -> Option<&CommitId> :104, .is_present() :115. RefName is unsized str-like: use RefName::new("feat-1") / "feat-1".as_ref().

### MutableRepo remote-bookmark + git-ref methods  (/Users/lita/src/jj/lib/src/repo.rs:1699)
```rust
pub fn get_remote_bookmark(&self, symbol: RemoteRefSymbol<'_>) -> RemoteRef (1699); pub fn set_remote_bookmark(&mut self, symbol: RemoteRefSymbol<'_>, remote_ref: RemoteRef) (1704); pub fn track_remote_bookmark(&mut self, symbol: RemoteRefSymbol<'_>) -> IndexResult<()> (1724); pub fn get_git_ref(&self, name: &GitRefName) -> RefTarget (1796); pub fn set_git_ref_target(&mut self, name: &GitRefName, target: RefTarget) (1800); pub fn git_head(&self) -> RefTarget (1818); pub fn set_git_head_target(&mut self, target: RefTarget) (1822)
```
RemoteRefSymbol { name: &RefName, remote: &RemoteName }. RemoteRef { target: RefTarget, state: RemoteRefState } (op_store.rs:139). After a push, gt submit should set_remote_bookmark to the pushed target with state Tracked (jj's git push plumbing does this for you if you use git::push_branches). These only edit the in-memory View — the real .git refs are written by git::export_refs.

### MutableRepo::set_wc_commit / check_out / edit  (/Users/lita/src/jj/lib/src/repo.rs:1458)
```rust
pub fn set_wc_commit(&mut self, name: WorkspaceNameBuf, commit_id: CommitId) -> Result<(), RewriteRootCommit> (1458); pub fn check_out(&mut self, name: WorkspaceNameBuf, commit: &Commit) -> Result<Commit, CheckOutCommitError> (1514); pub fn edit(&mut self, name: WorkspaceNameBuf, commit: &Commit) -> Result<(), EditCommitError> (1526)
```
check_out creates a NEW empty working-copy commit on top of `commit` (jj model: WC is always a commit) and points view.wc_commit_ids[name] at it. THIS IS STILL ONLY THE VIEW — the files on disk are untouched until Workspace::check_out. WorkspaceName::DEFAULT is the default workspace name ("default"); WorkspaceNameBuf via .to_owned().

### View (wrapper) + op_store::View (data)  (/Users/lita/src/jj/lib/src/op_store.rs:250)
```rust
pub struct View { pub head_ids: HashSet<CommitId>, pub local_bookmarks: BTreeMap<RefNameBuf, RefTarget>, pub local_tags: BTreeMap<RefNameBuf, RefTarget>, pub remote_views: BTreeMap<RemoteNameBuf, RemoteView>, pub git_refs: BTreeMap<GitRefNameBuf, RefTarget>, pub git_head: RefTarget, pub wc_commit_ids: BTreeMap<WorkspaceNameBuf, CommitId> }
```
op_store.rs:261-263 comment is the smoking gun for the talk: wc_commit_ids is 'The commit that *should be* checked out in the workspace. Note that the working copy (.jj/working_copy/) has the source of truth about which commit *is* checked out'. Wrapper view.rs:43 View{data}: accessors wc_commit_ids() view.rs:54, get_wc_commit_id(name)->Option<&CommitId> :58, heads()->&HashSet<CommitId> :76, local_bookmarks()->impl Iterator<Item=(&RefName,&RefTarget)> :140, local_bookmarks_for_commit(commit_id) :149, get_local_bookmark(name)->&RefTarget :168, git_refs() :96, git_head() :100, remote_bookmarks/local_remote_bookmarks :198/:269, store_view()->&op_store::View :560.

### op_store::Operation + OperationMetadata  (/Users/lita/src/jj/lib/src/op_store.rs:361)
```rust
pub struct Operation { pub view_id: ViewId, pub parents: Vec<OperationId>, pub metadata: OperationMetadata, pub commit_predecessors: Option<BTreeMap<CommitId, Vec<CommitId>>> }; pub struct OperationMetadata { pub time: TimestampRange, pub description: String, pub hostname: String, pub username: String, pub is_snapshot: bool, pub tags: HashMap<String, String> } (op_store.rs:417)
```
An operation = pointer to a View snapshot + parent op(s) + metadata. Wrapper operation.rs:40 Operation{op_store, id, data}: id() :97, view_id() :101, parent_ids() -> &[OperationId] :105, parents() -> iterator of OpStoreResult<Operation> :109, view() -> OpStoreResult<View> :117, metadata() :122.

### OpStore trait  (/Users/lita/src/jj/lib/src/op_store.rs:462)
```rust
#[async_trait] pub trait OpStore: Any + Send + Sync + Debug { fn name(&self) -> &str; fn root_operation_id(&self) -> &OperationId; async fn read_view(&self, id: &ViewId) -> OpStoreResult<View>; async fn write_view(&self, contents: &View) -> OpStoreResult<ViewId>; async fn read_operation(&self, id: &OperationId) -> OpStoreResult<Operation>; async fn write_operation(&self, contents: &Operation) -> OpStoreResult<OperationId>; ... }
```
jj-lib calls these with pollster's .block_on() internally; consumers rarely call OpStore directly.

### OpHeadsStore trait  (/Users/lita/src/jj/lib/src/op_heads_store.rs:56)
```rust
#[async_trait] pub trait OpHeadsStore: Any + Send + Sync + Debug { fn name(&self) -> &str; async fn update_op_heads(&self, old_ids: &[OperationId], new_id: &OperationId) -> Result<(), OpHeadsStoreError>; async fn get_op_heads(&self) -> Result<Vec<OperationId>, OpHeadsStoreError>; async fn lock(&self) -> Result<Box<dyn OpHeadsStoreLock + '_>, OpHeadsStoreError>; }
```
'publish' = update_op_heads(parents, new). SimpleOpHeadsStore stores heads as files in .jj/repo/op_heads/heads/.

### resolve_op_heads (divergent-operation reconciliation)  (/Users/lita/src/jj/lib/src/op_heads_store.rs:88)
```rust
pub fn resolve_op_heads<E>(op_heads_store: &dyn OpHeadsStore, op_store: &Arc<dyn OpStore>, resolver: impl FnOnce(Vec<Operation>) -> Result<Operation, E>) -> Result<Operation, E> where E: From<OpHeadResolutionError> + From<OpHeadsStoreError> + From<OpStoreError>
```
Fast path: exactly 1 head -> return it. Otherwise: take op-heads lock (advisory only, op_heads_store.rs:112-115), re-read heads; filter out heads that are ancestors of other heads (dag_walk::heads_ok, :138) and prune them from the heads set; if 1 remains return it; else sort remaining by metadata().time.end.timestamp (:159) and call resolver — which in RepoLoader is merge_operations => a new merge Operation with all heads as parents and a merged View, published via update_op_heads(old_heads+parents, merged_id). So multiple op heads (from concurrent jj/gt processes) are harmless and self-heal on next load_at_head().

### Workspace::check_out — WRITE #2 (files + .jj/working_copy)  (/Users/lita/src/jj/lib/src/workspace.rs:428)
```rust
pub fn check_out(&mut self, operation_id: OperationId, old_tree: Option<&MergedTree>, commit: &Commit) -> Result<CheckoutStats, CheckoutError>
```
This is the call that actually materializes files on disk AND records the new working-copy state (op id + tree) in .jj/working_copy. Body: start_working_copy_mutation() (workspace.rs:418 -> LockedWorkspace) -> locked_wc.check_out(commit).block_on() -> locked_ws.finish(operation_id). Pass old_tree=Some(prev wc commit's tree) to detect ConcurrentCheckout (workspace.rs:439-443), or None to skip. operation_id = the op returned by tx.commit(). Call AFTER Transaction::commit — the op-log write and the working-copy write are two separate, ordered writes.

### git::export_refs — WRITE #3 (colocated .git refs)  (/Users/lita/src/jj/lib/src/git.rs:967)
```rust
pub fn export_refs(mut_repo: &mut MutableRepo) -> Result<GitExportStats, GitExportError>
```
Writes jj's view (local bookmarks etc.) out to real git refs in the colocated .git dir and records the exported state in view.git_refs. Call it INSIDE the transaction (on tx.repo_mut()) after mutating bookmarks, BEFORE tx.commit(), so the exported git_refs state is captured in the same operation. Counterpart git::import_refs(mut_repo, &GitSettings) at git.rs:473 for gt sync after `git fetch`. Neither is called by Transaction::commit.

## Minimal flow
// gt command skeleton (colocated repo), demonstrating the THREE writes:
let settings: UserSettings = /* UserSettings::from_config(...) */;
// Load workspace (gives both RepoLoader and WorkingCopy):
let mut workspace = Workspace::load(&settings, Path::new("."), &StoreFactories::default(), &default_working_copy_factories())?;
let repo: Arc<ReadonlyRepo> = workspace.repo_loader().load_at_head()?;            // repo.rs:756 (resolves/merges op heads)

// -- mutate --
let mut tx: Transaction = repo.start_transaction();                                // repo.rs:326
let mut_repo: &mut MutableRepo = tx.repo_mut();                                    // transaction.rs:94
let commit: Commit = mut_repo.new_commit(vec![parent_id], tree)                    // repo.rs:948
        .set_description(msg).write()?;
mut_repo.set_local_bookmark_target("feat-1".as_ref(), RefTarget::normal(commit.id().clone()))?; // repo.rs:1676
mut_repo.check_out(WorkspaceName::DEFAULT.to_owned(), &commit)?;                   // repo.rs:1514 — view only!
mut_repo.rebase_descendants()?;                                                    // repo.rs:1428 — required if any rewrite_commit/abandon happened
git::export_refs(mut_repo)?;                                                       // git.rs:967 — WRITE #3 staged into view + .git

// -- WRITE #1: op log (view blob + operation + index + op-heads swap) --
let repo: Arc<ReadonlyRepo> = tx.commit("gt create feat-1")?;                      // transaction.rs:120 — writes ONLY op store/op heads/index

// -- WRITE #2: working copy files + .jj/working_copy state --
let wc_commit_id = repo.view().get_wc_commit_id(WorkspaceName::DEFAULT).unwrap();  // view.rs:58
let wc_commit = repo.store().get_commit(wc_commit_id)?;
let stats = workspace.check_out(repo.op_id().clone(), Some(&old_tree), &wc_commit)?; // workspace.rs:428

// gt sync concurrent-safety: another process may have committed ops meanwhile;
// next load_at_head() auto-reconciles via resolve_op_heads (op_heads_store.rs:88)
// -> RepoLoader::merge_operations "reconcile divergent operations" (repo.rs:805,837).

## Gotchas
- Transaction::commit consumes self and returns a NEW Arc<ReadonlyRepo>; the old repo Arc is stale. Reassign: `repo = tx.commit(desc)?;` then feed repo.op_id() to Workspace::check_out.
- Transaction::write PANICS (assert at transaction.rs:136-139: 'BUG: Descendants have not been rebased after the last rewrites.') if you used rewrite_commit/record_abandoned_commit without calling repo_mut().rebase_descendants() first. gt modify MUST call rebase_descendants() before commit — that call is also what moves bookmarks/wc pointers off rewritten commits.
- Proof for the talk that commit != checkout: transaction.rs's entire import list (lines 17-40) contains no working-copy, workspace, or git module; write() touches only op_store().write_view/write_operation, index_store().write_index, and publish() only op_heads_store.update_op_heads. And op_store.rs:261-263 documents wc_commit_ids as the commit that *should be* checked out, with .jj/working_copy/ as the source of truth for what *is*.
- MutableRepo::check_out (repo.rs:1514) vs Workspace::check_out (workspace.rs:428) are different layers with the same name: the former edits the in-memory View (and creates the new empty WC commit), the latter writes files to disk and saves .jj/working_copy state. gt checkout needs BOTH, in that order, with tx.commit between them.
- git::export_refs takes &mut MutableRepo, so it must run inside the transaction (it both writes real .git refs immediately AND records exported state in view.git_refs); if you commit the transaction without exporting, the colocated .git silently diverges from jj's view — exactly the third write the talk highlights.
- Order on failure: git refs in .git are written before the op commit (export_refs runs pre-commit); the working copy is written after. jj tolerates a crash between writes because working-copy state records the op id it was updated at, and stale checkouts are detected via old_tree comparison (workspace.rs:439-443 -> CheckoutError::ConcurrentCheckout).
- jj-lib traits (OpStore, OpHeadsStore, LockedWorkingCopy::check_out) are async; jj drives them with pollster's .block_on(). LockedWorkspace::finish and locked_wc.check_out(commit) need .block_on() (import pollster::FutureExt as _).
- Multiple op heads are NORMAL, not corruption: SimpleOpHeadsStore just leaves both head files; resolution is lazy at next load_at_head, creating a merge operation whose View is a 3-way merge (bookmarks via merge_ref_targets, wc pointer keeps self side on conflict per merge_wc_commit repo.rs:1478-1504). The op-heads lock (op_heads_store.rs:74) is explicitly 'not needed for correctness'.
- start_transaction is infallible and takes &Arc<ReadonlyRepo> (self: &Arc<Self>), so you need the repo in an Arc (loaders already return Arc).
- RefName/WorkspaceName/RemoteName/GitRefName are unsized str newtypes (ref_name module); owned variants are RefNameBuf/WorkspaceNameBuf/etc. Map lookups and setters take &RefName — construct with RefName::new("main") or "main".as_ref().
- get_local_bookmark on a missing name returns RefTarget::absent(), not an Option — check .is_present()/.as_normal(). Setting an absent target DELETES the bookmark (view.rs:176-188) — that is how gt sync removes merged branches.
- set_local_bookmark_target auto-adds the target commits as view heads (repo.rs:1678-1680), but new_commit().write() does NOT update wc pointer — a new commit only becomes 'checked out' via mut_repo.check_out/edit/set_wc_commit.
- ReadonlyRepo::reload_at_head/reload_at return a new Arc and are just loader shortcuts (repo.rs:331-338); there is no in-place refresh anywhere in the API.
- For a colocated repo, gt init should go through Workspace::init_colocated_git or git_backend paths rather than raw ReadonlyRepo::init (repo.rs:202) — init's backend_initializer decides SimpleBackend vs GitBackend, and StoreFactories::default() only includes GitBackend when jj-lib is built with the 'git' feature.


---

# workspace-workingcopy

## Summary

jj-lib's workspace layer (lib/src/workspace.rs, working_copy.rs, local_working_copy.rs) is write #1 and #2 of the three-writes story: committing a Transaction only updates the op log/repo view; the consumer must SEPARATELY (a) mutate the files on disk (LockedWorkingCopy::check_out / snapshot) and (b) persist the workspace's working-copy state (.jj/working_copy: tree_state + checkout proto recording the OperationId) via LockedWorkingCopy::finish(op_id) / LockedWorkspace::finish(op_id). (Write #3, git refs, lives in git.rs — out of scope here.) Workspace = repo_loader + Box<dyn WorkingCopy>. Workspace::init_colocated_git creates a colocated repo (git backend stored so .git sits in the workspace root) and checks out the root commit; Workspace::load needs UserSettings + StoreFactories + WorkingCopyFactories (use default_working_copy_factories()). Mutation is lock-scoped: WorkingCopy::start_mutation takes a file lock (.jj/working_copy/working_copy.lock), re-reads state, and returns Box<dyn LockedWorkingCopy> with snapshot/check_out/reset/recover; nothing is persisted until finish(operation_id). Snapshot returns a MergedTree which the consumer must fold into the working-copy commit themselves via MutableRepo::rewrite_commit(...).set_tree(...).write() then rebase_descendants() then tx.commit(), then finish() the locked wc with the NEW op id — forgetting any step yields a stale working copy, detected by WorkingCopyFreshness::check_stale comparing the wc-recorded OperationId against the repo op head.

## Key APIs

### Workspace::init_colocated_git  (/Users/lita/src/jj/lib/src/workspace.rs:216)
```rust
#[cfg(feature = "git")] pub fn init_colocated_git(user_settings: &UserSettings, workspace_root: &Path) -> Result<(Self, Arc<ReadonlyRepo>), WorkspaceInitError>
```
THE init for gt init. Builds a GitBackend::init_colocated backend (git repo shares the working copy — .git in workspace_root), Signer::from_settings, then init_with_backend. Returns the Workspace plus the repo already at op 'add workspace' with root commit checked out (init_working_copy at workspace.rs:129 runs a transaction check_out(workspace_name, root_commit) and writes .jj/working_copy/type). Errors if .jj already exists (DestinationExists). Sibling variants: init_simple (workspace.rs:187), init_internal_git (workspace.rs:200, bare git in .jj/repo/store/git), init_external_git(user_settings, workspace_root, git_repo_path: &Path) (workspace.rs:248), init_with_backend (workspace.rs:331), init_with_factories (workspace.rs:284, 10 args incl. WorkingCopyFactory + WorkspaceNameBuf), init_workspace_with_existing_repo (workspace.rs:351).

### Workspace::load  (/Users/lita/src/jj/lib/src/workspace.rs:382)
```rust
pub fn load(user_settings: &UserSettings, workspace_path: &Path, store_factories: &StoreFactories, working_copy_factories: &WorkingCopyFactories) -> Result<Self, WorkspaceLoadError>
```
workspace_path = workspace root (dir containing .jj). Pass &StoreFactories::default() (registers git backend when the 'git' feature is on) and &default_working_copy_factories() (workspace.rs:605, HashMap {"local" -> LocalWorkingCopyFactory}); pub type WorkingCopyFactories = HashMap<String, Box<dyn WorkingCopyFactory>> (workspace.rs:536). Internally DefaultWorkspaceLoader reads .jj/repo (dir, or file containing a relative path for secondary workspaces), RepoLoader::init_from_file_system, then factory.load_working_copy(store, workspace_root, .jj/working_copy, settings). Workspace accessors: workspace_root() :393, workspace_name() :397, repo_path() :401, repo_loader() :405, settings() :410, working_copy() -> &dyn WorkingCopy :414.

### Workspace::start_working_copy_mutation  (/Users/lita/src/jj/lib/src/workspace.rs:418)
```rust
pub fn start_working_copy_mutation(&mut self) -> Result<LockedWorkspace<'_>, WorkingCopyStateError>
```
Entry point for BOTH snapshot and checkout. Wraps WorkingCopy::start_mutation. LockedWorkspace (workspace.rs:456): .locked_wc(&mut self) -> &mut dyn LockedWorkingCopy (:462); .finish(self, operation_id: OperationId) -> Result<(), WorkingCopyStateError> (:466) — calls locked_wc.finish(op_id).block_on() and swaps the new WorkingCopy back into the Workspace. No discard method: dropping LockedWorkspace releases the lock and abandons unsaved state.

### Workspace::check_out  (/Users/lita/src/jj/lib/src/workspace.rs:428)
```rust
pub fn check_out(&mut self, operation_id: OperationId, old_tree: Option<&MergedTree>, commit: &Commit) -> Result<CheckoutStats, CheckoutError>
```
Convenience for gt checkout / post-transaction file update: locks, compares old_tree (pass Some(&old wc commit tree) you loaded before the tx; mismatch -> CheckoutError::ConcurrentCheckout), runs locked_wc.check_out(commit), then finish(operation_id). operation_id MUST be the op id of the freshly committed repo (repo.op_id().clone() from the Arc<ReadonlyRepo> returned by tx.commit(..)). This is writes #1+#2 in one call, AFTER the transaction commit.

### trait WorkingCopy  (/Users/lita/src/jj/lib/src/working_copy.rs:53)
```rust
pub trait WorkingCopy: Any + Send { fn name(&self) -> &str; fn workspace_name(&self) -> &WorkspaceName; fn operation_id(&self) -> &OperationId; fn tree(&self) -> Result<&MergedTree, WorkingCopyStateError>; fn sparse_patterns(&self) -> Result<&[RepoPathBuf], WorkingCopyStateError>; fn start_mutation(&self) -> Result<Box<dyn LockedWorkingCopy>, WorkingCopyStateError>; }
```
operation_id() is the op the wc was last synced to — the anchor for staleness detection. tree() is the last checked-out/snapshotted tree. dyn WorkingCopy::downcast_ref::<T>() at working_copy.rs:80 (e.g. to LocalWorkingCopy).

### trait LockedWorkingCopy (async_trait)  (/Users/lita/src/jj/lib/src/working_copy.rs:110)
```rust
#[async_trait] pub trait LockedWorkingCopy: Any + Send { fn old_operation_id(&self) -> &OperationId; fn old_tree(&self) -> &MergedTree; async fn snapshot(&mut self, options: &SnapshotOptions) -> Result<(MergedTree, SnapshotStats), SnapshotError>; async fn check_out(&mut self, commit: &Commit) -> Result<CheckoutStats, CheckoutError>; fn rename_workspace(&mut self, new_workspace_name: WorkspaceNameBuf); async fn reset(&mut self, commit: &Commit) -> Result<(), ResetError>; async fn recover(&mut self, commit: &Commit) -> Result<(), ResetError>; fn sparse_patterns(&self) -> Result<&[RepoPathBuf], WorkingCopyStateError>; async fn set_sparse_patterns(&mut self, new_sparse_patterns: Vec<RepoPathBuf>) -> Result<CheckoutStats, CheckoutError>; async fn finish(self: Box<Self>, operation_id: OperationId) -> Result<Box<dyn WorkingCopy>, WorkingCopyStateError>; }
```
All async methods: drive with pollster::FutureExt::block_on() (jj-lib does this internally too). snapshot = files->tree (write direction store); check_out = tree->files (writes disk files + records new tree); reset = record new tree WITHOUT touching files (used for gt modify-style 'the commit changed but files already match', and for stale recovery); recover = reset that doesn't assume old tree exists. finish(op_id) persists tree_state + checkout proto — the ONLY persist point.

### SnapshotOptions  (/Users/lita/src/jj/lib/src/working_copy.rs:212)
```rust
pub struct SnapshotOptions<'a> { pub base_ignores: Arc<GitIgnoreFile>, pub progress: Option<&'a SnapshotProgress<'a>>, pub start_tracking_matcher: &'a dyn Matcher, pub force_tracking_matcher: &'a dyn Matcher, pub max_new_file_size: u64 }
```
No Default impl — construct literally. Minimal demo values: base_ignores: GitIgnoreFile::empty() (gitignore.rs:53) or .chain_with_file("", workspace_root.join(".gitignore")) (gitignore.rs:108, sig: pub fn chain_with_file(self: &Arc<Self>, prefix: &str, file: PathBuf) -> Result<Arc<Self>, GitIgnoreError>); progress: None; start_tracking_matcher/force_tracking_matcher: &EverythingMatcher / &NothingMatcher (crate::matchers); max_new_file_size: u64::MAX. SnapshotStats (working_copy.rs:240): { untracked_paths: BTreeMap<RepoPathBuf, UntrackedReason> }. CheckoutStats (working_copy.rs:262): updated_files/added_files/removed_files/skipped_files: u32.

### WorkingCopyFreshness::check_stale  (/Users/lita/src/jj/lib/src/working_copy.rs:363)
```rust
pub fn check_stale(locked_wc: &dyn LockedWorkingCopy, wc_commit: &Commit, repo: &ReadonlyRepo) -> Result<Self, OpStoreError>
```
enum WorkingCopyFreshness (working_copy.rs:347): Fresh | Updated(Box<Operation>) | WorkingCopyStale | SiblingOperation. Logic: locked_wc.old_operation_id() == repo.op_id() -> Fresh; else walk op DAG to common ancestor: ancestor==repo op -> Updated (RELOAD repo at wc's op: repo.reload_at(&op)); ancestor==wc op -> stale unless trees already equal; else SiblingOperation. Call this at the top of every command after snapshotting lock, with wc_commit = repo.store().get_commit(repo.view().get_wc_commit_id(ws_name).unwrap()). Recovery for a truly broken wc: create_and_check_out_recovery_commit(locked_wc: &mut dyn LockedWorkingCopy, repo: &Arc<ReadonlyRepo>, workspace_name: WorkspaceNameBuf, description: &str) -> Result<(Arc<ReadonlyRepo>, Commit), RecoverWorkspaceError> (working_copy.rs:425); routine stale recovery = locked_wc.check_out(&desired_commit) or .recover(...) then finish(repo.op_id()).

### LocalWorkingCopy + LocalWorkingCopyFactory  (/Users/lita/src/jj/lib/src/local_working_copy.rs:2469)
```rust
pub fn init(store: Arc<Store>, working_copy_path: PathBuf, state_path: PathBuf, operation_id: OperationId, workspace_name: WorkspaceNameBuf, user_settings: &UserSettings) -> Result<Self, WorkingCopyStateError>  /  pub fn load(store: Arc<Store>, working_copy_path: PathBuf, state_path: PathBuf, user_settings: &UserSettings) -> Result<Self, WorkingCopyStateError>
```
init at :2539, load at :2577, name() -> "local" at :2532, state_path() :2599, file_states() :2624. You normally never call these directly — Workspace::init_*/load do it via LocalWorkingCopyFactory (local_working_copy.rs:2657) which implements WorkingCopyFactory (working_copy.rs:86: init_working_copy(store, working_copy_path, state_path, operation_id, workspace_name, settings) / load_working_copy(store, working_copy_path, state_path, settings) -> Box<dyn WorkingCopy>). start_mutation impl (local_working_copy.rs:2499): FileLock::lock(state_path/"working_copy.lock"), RE-READS CheckoutState and lazily re-reads TreeState after locking, returns LockedLocalWorkingCopy{old_operation_id, old_tree, tree_state_dirty:false, _lock}.

### LockedLocalWorkingCopy::finish (persist semantics)  (/Users/lita/src/jj/lib/src/local_working_copy.rs:2777)
```rust
async fn finish(mut self: Box<Self>, operation_id: OperationId) -> Result<Box<dyn WorkingCopy>, WorkingCopyStateError>
```
asserts tree_state_dirty || old_tree ids == current tree ids (i.e. you must not have swapped trees without a dirtying op); if tree_state_dirty -> TreeState::save() (atomic temp-file rename of .jj/working_copy/tree_state proto, local_working_copy.rs:1128); if op id changed -> CheckoutState::save() rewrites .jj/working_copy/checkout proto {operation_id, workspace_name} (local_working_copy.rs:2446). snapshot impl (:2716) delegates to TreeState::snapshot(&mut self, options) -> (bool /*is_dirty*/, SnapshotStats) at :1241 and returns tree_state.current_tree().clone(); check_out impl (:2726) no-ops if tree ids already equal, else TreeState::check_out(&mut self, new_tree: &MergedTree) -> Result<CheckoutStats, CheckoutError> (:2069) which diffs old->new tree through the sparse matcher and writes/deletes files on disk.

### MutableRepo working-copy-commit APIs (write #0, the op-log side)  (/Users/lita/src/jj/lib/src/repo.rs:1514)
```rust
pub fn check_out(&mut self, name: WorkspaceNameBuf, commit: &Commit) -> Result<Commit, CheckOutCommitError>  /  pub fn edit(&mut self, name: WorkspaceNameBuf, commit: &Commit) -> Result<(), EditCommitError> (repo.rs:1526)  /  pub fn set_wc_commit(&mut self, name: WorkspaceNameBuf, commit_id: CommitId) -> Result<(), RewriteRootCommit> (repo.rs:1458)  /  pub fn new_commit(&mut self, parents: Vec<CommitId>, tree: MergedTree) -> CommitBuilder<'_> (repo.rs:948)  /  pub fn rewrite_commit(&mut self, predecessor: &Commit) -> CommitBuilder<'_> (repo.rs:954)  /  pub fn rebase_descendants(&mut self) -> BackendResult<usize> (repo.rs:1428)
```
The 'working-copy commit' is view state: view.get_wc_commit_id(workspace_name). MutableRepo::check_out creates a NEW empty commit on top of `commit` and edits it (jj's @ semantics — right for gt checkout); edit() points @ at an existing commit (abandoning the old wc commit if empty+unreferenced). After snapshot, update the wc commit with tx.repo_mut().rewrite_commit(&wc_commit).set_tree(new_tree).write()? (CommitBuilder::set_tree at commit_builder.rs:88/320); descendants auto-follow only after rebase_descendants(); Transaction::commit(description) (transaction.rs:120) -> Arc<ReadonlyRepo> asserts !has_rewrites (transaction.rs:137) so CALL rebase_descendants() first whenever you used rewrite_commit.

## Minimal flow
// ===== gt init =====
let settings = UserSettings::from_config(config)?;
let (mut ws, repo) = Workspace::init_colocated_git(&settings, workspace_root)?;   // workspace.rs:216
// repo already contains op "add workspace"; wc commit = empty commit on root; files untouched (empty tree)

// ===== load (every other command) =====
let mut ws = Workspace::load(&settings, cwd, &StoreFactories::default(), &default_working_copy_factories())?; // workspace.rs:382
let repo: Arc<ReadonlyRepo> = ws.repo_loader().load_at_head()?;                    // op head

// ===== snapshot (start of create/modify/submit/sync) =====
let mut locked_ws = ws.start_working_copy_mutation()?;                             // workspace.rs:418 — takes working_copy.lock
let wc_commit_id = repo.view().get_wc_commit_id(ws.workspace_name()).unwrap().clone();
let wc_commit = repo.store().get_commit(&wc_commit_id)?;
match WorkingCopyFreshness::check_stale(locked_ws.locked_wc(), &wc_commit, &repo)? { // working_copy.rs:363
    Fresh => {}
    Updated(op) => { repo = repo.reload_at(&op)?; /* re-fetch wc_commit */ }
    WorkingCopyStale => { /* locked_ws.locked_wc().check_out(&wc_commit).block_on()?; then finish(repo.op_id().clone()) — or bail */ }
    SiblingOperation => bail!(),
}
let opts = SnapshotOptions { base_ignores: GitIgnoreFile::empty().chain_with_file("", root.join(".gitignore"))?,
    progress: None, start_tracking_matcher: &EverythingMatcher, force_tracking_matcher: &NothingMatcher,
    max_new_file_size: u64::MAX };
let (new_tree, _stats) = locked_ws.locked_wc().snapshot(&opts).block_on()?;        // working_copy.rs:118 → (MergedTree, SnapshotStats)
let mut tx = repo.start_transaction();
if new_tree.tree_ids_and_labels() != wc_commit.tree().tree_ids_and_labels() {
    let new_wc = tx.repo_mut().rewrite_commit(&wc_commit).set_tree(new_tree).write()?;  // fold dirty files into @
    tx.repo_mut().rebase_descendants()?;                                            // repo.rs:1428 — required before commit
    wc_commit = new_wc;
}

// ===== gt create -m msg (still inside tx) =====
let new_commit = tx.repo_mut().new_commit(vec![wc_commit.id().clone()], wc_commit.tree()) // stack on top; empty diff for now
    .set_description(msg).write()?;                                                // then set bookmark:
tx.repo_mut().set_local_bookmark_target(&branch_name, RefTarget::normal(wc_commit.id().clone())); // (view API, git.rs scope)
tx.repo_mut().edit(ws.workspace_name().to_owned(), &new_commit)?;                  // repo.rs:1526 — @ = new empty commit

// ===== WRITE 1 of 3: commit the transaction (op log + view) =====
let repo = tx.commit("gt create")?;                                                // transaction.rs:120 → Arc<ReadonlyRepo>

// ===== WRITE 2 of 3: working-copy files + state =====
locked_ws.locked_wc().check_out(&repo.store().get_commit(repo.view().get_wc_commit_id(ws.workspace_name()).unwrap())?).block_on()?; // files on disk (working_copy.rs:124)
// (use .reset(&commit) instead when the tree content is identical and only the commit id moved — no file I/O)
locked_ws.finish(repo.op_id().clone())?;                                           // workspace.rs:466 — persists tree_state + checkout(op id); releases lock

// ===== WRITE 3 of 3: git refs (colocated) — git.rs (out of this file's scope): git::export_refs / reset HEAD =====

// ===== gt checkout main (no snapshot-needed variant) =====
let stats = ws.check_out(repo.op_id().clone(), Some(&old_wc_tree), &target_wc_commit)?; // workspace.rs:428 — lock+check_out+finish in one

## Gotchas
- Three-writes core: Transaction::commit() only writes the op log/view. Files on disk change ONLY via LockedWorkingCopy::check_out/reset, and .jj/working_copy state (tree_state + checkout op id) is persisted ONLY inside LockedWorkingCopy::finish(operation_id) (local_working_copy.rs:2777). Skip finish → wc records the OLD op id → next command sees WorkingCopyStale. Skip check_out → op log says @ moved but disk files still show the old tree.
- finish() must receive the NEW repo's op id (repo.op_id().clone() from the Arc<ReadonlyRepo> returned by tx.commit), not the old one. Passing the old id silently re-saves staleness.
- Lock ordering used by jj-cli: take the working-copy lock (start_working_copy_mutation) and snapshot BEFORE opening the transaction; keep it held across tx.commit; finish() after. Two nested 'transactions' — wc lock outside, repo tx inside.
- There is NO discard() on LockedWorkingCopy/LockedWorkspace in 0.36 — dropping releases the file lock and throws away in-memory tree-state changes. Danger: locked_wc.check_out() has ALREADY mutated files on disk before finish(); dropping after check_out leaves disk files updated but state recorded at the old tree, so the next snapshot will absorb the diff into the old wc commit.
- All the interesting LockedWorkingCopy methods (snapshot, check_out, reset, recover, finish) are async — jj-lib itself drives them with pollster::FutureExt::block_on(); do the same, no tokio needed.
- Transaction::commit asserts !mut_repo.has_rewrites() (transaction.rs:137): after any rewrite_commit()/abandon you MUST call tx.repo_mut().rebase_descendants() (repo.rs:1428) or commit will panic. MutableRepo::check_out/edit/new_commit alone don't set rewrites.
- SnapshotOptions has no Default; you must fill all 5 fields. Matchers live in jj_lib::matchers (EverythingMatcher, NothingMatcher). base_ignores must be built by the consumer (GitIgnoreFile::empty() / chain_with_file) — TreeState does NOT read the user's global gitignore for you; it does handle in-tree .gitignore files during traversal.
- LockedLocalWorkingCopy::snapshot returns (MergedTree, SnapshotStats) where the tree may be IDENTICAL to the wc commit's tree — always compare tree_ids_and_labels() before rewriting the wc commit, or you'll create no-op rewrites every command.
- Workspace::check_out(op_id, old_tree, commit) errors with CheckoutError::ConcurrentCheckout if the on-disk recorded tree differs from old_tree you pass — pass Some(tree of wc commit as of the repo you loaded) to get this protection, or None to skip it.
- start_mutation re-reads checkout state and lazily reloads the whole TreeState from disk after taking the lock (local_working_copy.rs:2506-2517) — cheap-looking calls on Workspace::working_copy() before locking may not match what you see after locking.
- WorkingCopyFreshness::Updated(op) means the REPO you loaded is old, not the wc: reload the repo at that op (repo.reload_at(&op)) and continue; do not 'fix' the working copy.
- init_colocated_git checks out the ROOT commit into an empty tree and creates an empty wc commit; the .git dir ends up in the workspace root (GitBackend::init_colocated with store-relative path). init_internal_git is NOT colocated (bare git under .jj/repo/store/git).
- On-disk layout of .jj/working_copy (state_path): 'type' file = working-copy impl name ('local', written by init_working_copy at workspace.rs:152); 'checkout' = proto {operation_id, workspace_name} (CheckoutState, local_working_copy.rs:2419); 'tree_state' = proto {tree_ids (multiple when conflicted), conflict_labels, file_states (path→{file_type, mtime, size, materialized_conflict_data}), sparse_patterns, watchman_clock} (TreeState::save local_working_copy.rs:1128); 'working_copy.lock' = FileLock taken by start_mutation. Both protos are written via NamedTempFile + atomic rename.
- TreeState::load falls back to TreeState::init (empty tree) if the tree_state file is missing (local_working_copy.rs:1063) — a deleted tree_state silently looks like 'everything is new'; that's what recover() is for.
- WorkspaceName::DEFAULT is the default workspace name; view().get_wc_commit_id(name) returns Option<&CommitId> — a workspace can exist without a wc commit (error case RecoverWorkspaceError::WorkspaceMissingWorkingCopy).


---

# git-integration

## Summary

jj-lib's git subsystem (lib/src/git.rs + git_backend.rs + git_subprocess.rs) is the bridge between jj's op-store view and the actual Git repo. It is the second and third of the "three writes": committing a Transaction only records the new view in the op log; to make a COLOCATED repo consistent you must ALSO (a) call git::reset_head + git::export_refs on tx.repo_mut() BEFORE tx.commit() so Git HEAD/index/refs match the new view, and (b) after tx.commit(), separately check out / reset the workspace working copy (LockedWorkingCopy) so files on disk and the workspace state match. Nothing in Transaction::commit does any of this for you — jj's own CLI wires it up in finish_transaction (cli/src/cli_util.rs:2084-2119: reset_head at 2094, export_refs at 2103, tx.commit at 2107, update_working_copy at 2114). Network I/O (fetch/push) is done by spawning the real `git` CLI as a subprocess (git_subprocess.rs), NOT libgit2/gix transport, so credentials come from git's own credential machinery. Ref import (fetch side) is likewise split: GitFetch::fetch only moves refs/remotes/* inside the .git dir; GitFetch::import_refs() then copies them into the MutableRepo view (remote bookmarks + optional auto-tracked local bookmarks). Colocated init = gix init WithWorktree at the workspace root (.git lives at <workspace_root>/.git), recorded via a `git_target` file in the jj store.

## Key APIs

### GitBackend::init_colocated  (/Users/lita/src/jj/lib/src/git_backend.rs:227)
```rust
pub fn init_colocated(settings: &UserSettings, store_path: &Path, workspace_root: &Path) -> Result<Self, Box<GitBackendInitError>>
```
Creates the Git repo with gix::ThreadSafeRepository::init_opts(canonical_workspace_root, gix::create::Kind::WithWorktree, ...) so `.git` is a normal directory at <workspace_root>/.git (line 238-245). Then init_with_repo (line 273) writes store_path/git_target containing the (relative) path to .git and creates store_path/extra TableStore. HEAD after init is whatever gix leaves: an unborn symbolic HEAD -> refs/heads/<init.defaultBranch or 'main'>; jj does not touch HEAD at init. Don't call this directly for gt init — use Workspace::init_colocated_git which handles path relativization and .jj creation.

### Workspace::init_colocated_git  (/Users/lita/src/jj/lib/src/workspace.rs:216)
```rust
pub fn init_colocated_git(user_settings: &UserSettings, workspace_root: &Path) -> Result<(Self, Arc<ReadonlyRepo>), WorkspaceInitError>
```
This is the gt-init entry point (jj CLI uses it at cli/src/commands/git/init.rs:191 for `jj git init --colocate`). Internally computes store-relative workspace root and calls GitBackend::init_colocated (workspace.rs:232), then init_with_backend -> init_with_factories which creates .jj/repo, .jj/working-copy, and the initial working copy. Requires jj-lib feature "git". For a fresh colocated repo no import_head/import_refs is needed (nothing to import).

### GitBackend accessors  (/Users/lita/src/jj/lib/src/git_backend.rs:336)
```rust
pub fn git_repo(&self) -> gix::Repository  /  pub fn git_repo_path(&self) -> &Path  /  pub fn git_workdir(&self) -> Option<&Path>
```
git_repo() (line 336) returns a fresh thread-local gix::Repository. git_repo_path() (line 341) is the .git dir (or bare repo dir). git_workdir() (line 346) is None for bare (internal) backends — the CLI's colocation check is_colocated_git_workspace (cli/src/git_util.rs:60) compares git_workdir to workspace_root. Get the backend via jj_lib::git::get_git_backend(store) (git.rs:330) or the gix repo via get_git_repo(store) (git.rs:335).

### GitSettings  (/Users/lita/src/jj/lib/src/git.rs:86)
```rust
pub struct GitSettings { pub auto_local_bookmark: bool, pub abandon_unreachable_commits: bool, pub executable_path: PathBuf, pub write_change_id_header: bool }  /  pub fn from_settings(settings: &UserSettings) -> Result<Self, ConfigGetError>  (git.rs:95)
```
executable_path (config git.executable-path, defaults to "git") is what fetch/push spawn. auto_local_bookmark=true makes import_refs auto-track (and thus create local bookmarks for) every fetched remote bookmark — handy for a demo gt sync.

### GitImportOptions  (/Users/lita/src/jj/lib/src/git.rs:427)
```rust
pub struct GitImportOptions { pub auto_local_bookmark: bool, pub abandon_unreachable_commits: bool, pub remote_auto_track_bookmarks: HashMap<RemoteNameBuf, StringMatcher> }
```
No constructor in lib; jj CLI builds it from GitSettings + remotes.<name>.auto-track-bookmarks (cli/src/git_util.rs:287 load_git_import_options). StringMatcher is crate::str_util::StringMatcher (StringExpression::to_matcher()). Auto-track decision is default_remote_ref_state_for (git.rs:789): Tracked if remote == REMOTE_NAME_FOR_LOCAL_GIT_REPO ("git", git.rs:76) OR auto_local_bookmark OR the per-remote matcher matches; otherwise state New (untracked, no local bookmark created). Tags always Tracked.

### import_refs / import_some_refs  (/Users/lita/src/jj/lib/src/git.rs:473)
```rust
pub fn import_refs(mut_repo: &mut MutableRepo, options: &GitImportOptions) -> Result<GitImportStats, GitImportError>  /  pub fn import_some_refs(mut_repo: &mut MutableRepo, options: &GitImportOptions, git_ref_filter: impl Fn(GitRefKind, RemoteRefSymbol<'_>) -> bool) -> Result<GitImportStats, GitImportError>  (git.rs:484)
```
Reads refs from the gix repo, bulk-imports missing commits into the backend/index, then mutates the view: mut_repo.set_git_ref_target(full_name, target) for every changed git ref (git.rs:556), mut_repo.set_remote_bookmark/set_remote_tag for name@remote refs (git.rs:574/592), mut_repo.merge_local_bookmark(name, base, new) for TRACKED refs only (git.rs:570) — this is the 3-way ref merge that moves/conflicts local bookmarks. Optionally abandons commits that became unreachable (abandon_unreachable_commits). GitImportStats (git.rs:438): abandoned_commits: Vec<CommitId>, changed_remote_bookmarks/changed_remote_tags: Vec<(RemoteRefSymbolBuf, (RemoteRef, RefTarget))>, failed_ref_names: Vec<BString>.

### import_head  (/Users/lita/src/jj/lib/src/git.rs:847)
```rust
pub fn import_head(mut_repo: &mut MutableRepo) -> Result<(), GitImportError>
```
Reads Git HEAD commit id, imports it into the backend if missing, mut_repo.add_head, and mut_repo.set_git_head_target(RefTarget::resolved(id)). Does NOT move the working-copy commit; jj CLI's import_git_head (cli/src/cli_util.rs:1190-1232) follows it with mut_repo.check_out(workspace_name, &head_commit) + locked_wc().reset(&wc_commit) to adopt an external `git checkout`. For gt you only need this if you want to notice HEAD moves made by raw git.

### export_refs / export_some_refs  (/Users/lita/src/jj/lib/src/git.rs:967)
```rust
pub fn export_refs(mut_repo: &mut MutableRepo) -> Result<GitExportStats, GitExportError>  /  pub fn export_some_refs(mut_repo: &mut MutableRepo, git_ref_filter: impl Fn(GitRefKind, RemoteRefSymbol<'_>) -> bool) -> Result<GitExportStats, GitExportError>  (git.rs:971)
```
Write #2 (git refs). Diffs view bookmarks/tags against view.git_refs() (last-seen git state) and writes refs/heads/<name> for local bookmarks, refs/remotes/<remote>/<name> for remote-tracking refs, refs/tags/* (lightweight) into the .git repo via gix ref transactions with compare-and-swap on old oid. If HEAD symbolically points at a branch being moved/deleted, HEAD is first detached at its current oid (git.rs:991-1030) so the branch update doesn't touch the worktree. Also updates the view: set_git_ref_target for each exported ref and copies exported local bookmarks into the "git" remote view (git.rs:1035-1046). MUST be called on tx.repo_mut() BEFORE tx.commit — jj CLI calls it inside finish_transaction (cli/src/cli_util.rs:2103) after reset_head. Refs that changed concurrently in Git are skipped and reported, not errors: GitExportStats { failed_bookmarks, failed_tags }: Vec<(RemoteRefSymbolBuf, FailedRefExportReason)> (git.rs:931); FailedRefExportReason (git.rs:901) variants: InvalidGitName, ConflictedOldState, OnRootCommit, DeletedInJjModifiedInGit, AddedInJjAddedInGit, ModifiedInJjDeletedInGit, FailedToDelete(err), FailedToSet(err). GitExportError (git.rs:886) is only Git(...)|UnexpectedBackend.

### reset_head  (/Users/lita/src/jj/lib/src/git.rs:1415)
```rust
pub fn reset_head(mut_repo: &mut MutableRepo, wc_commit: &Commit) -> Result<(), GitResetHeadError>
```
Colocated-HEAD maintenance. Sets Git HEAD DETACHED at wc_commit's FIRST PARENT (jj convention: HEAD = parent of @, so `git status` shows @'s diff as staged); if the parent is the root commit, HEAD becomes a symref to the placeholder refs/jj/root (unborn) via update_git_head (git.rs:1356). Uses PreviousValue::MustExistAndMatch when previously detached to detect races -> GitResetHeadError::UpdateHeadRef (treat as warning, like the CLI does at cli_util.rs:2096). Also clears git operation state (MERGE_HEAD, rebase-* dirs; git.rs:1462) and rewrites the Git index to wc_commit's parent tree with conflict stages + preserved stat info (reset_index, git.rs:1494). Caller: jj CLI finish_transaction whenever working_copy_shared_with_git and there is a new wc commit (cli/src/cli_util.rs:2094), i.e. call it in EVERY gt transaction that can move @, before export_refs, before tx.commit.

### GitFetch  (/Users/lita/src/jj/lib/src/git.rs:2569)
```rust
pub struct GitFetch<'a> { /* mut_repo, git_repo, git_ctx, import_options, fetched */ }  /  pub fn new(mut_repo: &'a mut MutableRepo, git_settings: &'a GitSettings, import_options: &'a GitImportOptions) -> Result<Self, UnexpectedGitBackendError>  (git.rs:2578)
```
Borrows the transaction's MutableRepo mutably for its whole lifetime. Sequence: new -> fetch(...) one or more times -> import_refs(). get_default_branch(remote_name: &RemoteName) -> Result<Option<RefNameBuf>, GitFetchError> (git.rs:2670) runs `git remote show` — use for gt sync to find main.

### GitFetch::fetch  (/Users/lita/src/jj/lib/src/git.rs:2602)
```rust
pub fn fetch(&mut self, remote_name: &RemoteName, refspecs: ExpandedFetchRefSpecs, mut callbacks: RemoteCallbacks, depth: Option<NonZeroU32>, fetch_tags_override: Option<FetchTagsOverride>) -> Result<(), GitFetchError>
```
(The 2nd param is pattern-destructured in source: `ExpandedFetchRefSpecs { bookmark_expr, refspecs: mut remaining_refspecs, negative_refspecs }`.) Spawns `git --git-dir <path> fetch --prune --no-write-fetch-head [--progress] [--depth=N] [--tags|--no-tags] -- <remote> <+refs/heads/X:refs/remotes/<remote>/X>...` (git_subprocess.rs:164-207). Retries dropping refspecs the remote doesn't have, then prunes those remote-tracking branches. Only updates refs/remotes/* in .git — the jj view is untouched until GitFetch::import_refs(&mut self) -> Result<GitImportStats, GitImportError> (git.rs:2694), which runs import_some_refs filtered to the fetched remote+branch matcher (tags always imported). Build refspecs with expand_fetch_refspecs(remote: &RemoteName, bookmark_expr: StringExpression) -> Result<ExpandedFetchRefSpecs, GitRefExpansionError> (git.rs:2300) — e.g. StringExpression::pattern(StringPattern::everything()) or StringExpression::exact("main") (str_util.rs:380) — or use the remote's configured refspecs via expand_default_fetch_refspecs(remote_name: &RemoteName, git_repo: &gix::Repository) -> Result<(IgnoredRefspecs, ExpandedFetchRefSpecs), GitDefaultRefspecError> (git.rs:2472). GitFetchError (git.rs:2257): NoSuchRemote(RemoteNameBuf) | RemoteName(GitRemoteNameError) | Subprocess(GitSubprocessError).

### push_branches  (/Users/lita/src/jj/lib/src/git.rs:2744)
```rust
pub fn push_branches(mut_repo: &mut MutableRepo, git_settings: &GitSettings, remote: &RemoteName, targets: &GitBranchPushTargets, callbacks: RemoteCallbacks) -> Result<GitPushStats, GitPushError>
```
The gt-submit primitive. Maps each (RefNameBuf, BookmarkPushUpdate) to GitRefUpdate{qualified_name: refs/heads/<name>, expected_current_target: old_target, new_target}, calls push_updates, and IF push_stats.all_ok() updates the view: set_git_ref_target("refs/remotes/<remote>/<name>") + set_remote_bookmark(name@remote, RemoteRef{target, state: Tracked}) (git.rs:2769-2784) — i.e. the remote-tracking refs move as part of your transaction. If any ref was rejected the view is NOT updated at all. BookmarkPushUpdate (lib/src/refs.rs:204): { pub old_target: Option<CommitId>, pub new_target: Option<CommitId> } — old_target must be the current remote-tracking position (None = expect absent on remote, i.e. new branch); use classify_bookmark_push_action(LocalAndRemoteRef) -> BookmarkPushAction (refs.rs:220) to compute it safely. GitBranchPushTargets (git.rs:2729): { pub branch_updates: Vec<(RefNameBuf, BookmarkPushUpdate)> }.

### push_updates  (/Users/lita/src/jj/lib/src/git.rs:2790)
```rust
pub fn push_updates(repo: &dyn Repo, git_settings: &GitSettings, remote_name: &RemoteName, updates: &[GitRefUpdate], mut callbacks: RemoteCallbacks) -> Result<GitPushStats, GitPushError>
```
Lower-level; does NOT touch the view. GitRefUpdate (git.rs:2733): { pub qualified_name: GitRefNameBuf, pub expected_current_target: Option<CommitId>, pub new_target: Option<CommitId> } (new_target None = delete). Force-with-lease: every non-delete refspec is `<new-oid-hex>:<qualified_name>` and the subprocess adds one `--force-with-lease=<qualified_name>:<expected-oid-or-empty>` per ref (RefToPush::to_git_lease git.rs:260; spawn_push git_subprocess.rs:261-295 runs `git push --porcelain --no-verify [--progress] --force-with-lease=... -- <remote> <refspecs-without-+>`). Empty expected oid = lease requires ref absent. GitPushStats (git.rs:135): { pushed: Vec<GitRefNameBuf>, rejected: Vec<(GitRefNameBuf, Option<String>)> /* lease failures */, remote_rejected: Vec<(GitRefNameBuf, Option<String>)> }, all_ok() (git.rs:145).

### RemoteCallbacks / Progress / FetchTagsOverride  (/Users/lita/src/jj/lib/src/git.rs:2842)
```rust
#[non_exhaustive] #[derive(Default)] pub struct RemoteCallbacks<'a> { pub progress: Option<&'a mut dyn FnMut(&Progress)>, pub sideband_progress: Option<&'a mut dyn FnMut(&[u8])>, pub get_ssh_keys: Option<&'a mut dyn FnMut(&str) -> Vec<PathBuf>>, pub get_password: Option<&'a mut dyn FnMut(&str, &str) -> Option<String>>, pub get_username_password: Option<&'a mut dyn FnMut(&str) -> Option<(String, String)>> }
```
RemoteCallbacks::default() is fine for gt. Only `progress` (parsed from git's stderr percentages; Progress { bytes_downloaded: Option<u64>, overall: f32 } git.rs:2851) and `sideband_progress` (raw remote stderr lines) are consumed by the subprocess implementation (git_subprocess.rs:702). The three auth callbacks are vestigial from the libgit2 era and are NEVER called on the subprocess path. FetchTagsOverride (git.rs:2860): AllTags | NoTags.

### git_subprocess execution model / auth  (/Users/lita/src/jj/lib/src/git_subprocess.rs:80)
```rust
pub(crate) struct GitSubprocessContext<'a> { git_dir: PathBuf, git_executable_path: &'a Path }
```
All network ops are `git` CLI subprocesses: create_command (git_subprocess.rs:101) runs `<git.executable-path> -c core.fsmonitor=false -c submodule.recurse=false --git-dir <path> ...` with LC_ALL=C, stdin null, stderr piped, environment otherwise INHERITED. Minimum git 2.40.4 (git_subprocess.rs:49). HTTPS-token auth implication for gt submit: jj-lib never handles credentials — the spawned git uses its normal credential stack (credential.helper, GIT_ASKPASS/SSH_ASKPASS, ~/.git-credentials, macOS keychain, or a token embedded in the remote URL). Easiest demo setups: `gh auth setup-git`, or remote URL https://x-access-token:<TOKEN>@github.com/owner/repo.git, or pass env GIT_ASKPASS to a helper script — but note stdin is null, so interactive prompting inside the subprocess will fail; make sure credentials are non-interactive.

### ref_name.rs name types  (/Users/lita/src/jj/lib/src/ref_name.rs:45)
```rust
pub struct GitRefNameBuf(String); pub struct GitRefName(str); pub struct RefNameBuf(String); pub struct RefName(str); pub struct RemoteNameBuf(String); pub struct RemoteName(str); pub struct WorkspaceNameBuf(String); pub struct WorkspaceName(str);
```
Borrowed types are #[repr(transparent)] str newtypes with `pub const fn new(name: &str) -> &Self` (ref-cast), `pub const fn as_str(&self) -> &str`, `pub fn as_symbol(&self) -> &RefSymbol` (Display with quoting) — all generated by impl_name_type! (ref_name.rs:145, applied at 302-307). Buf types: From<String>/From<&str>/From<&Borrowed>, into_string(). GitRefName = fully-qualified ("refs/heads/main"); RefName = bare bookmark/tag name ("main"); RemoteName = remote ("origin"; the reserved local pseudo-remote is REMOTE_NAME_FOR_LOCAL_GIT_REPO = RemoteName::new("git"), git.rs:76). RefName::to_remote_symbol(&'a self, remote: &'a RemoteName) -> RemoteRefSymbol<'a> (ref_name.rs:311). RemoteRefSymbol<'a> { pub name: &'a RefName, pub remote: &'a RemoteName } (Copy; ref_name.rs:367) with .to_owned() -> RemoteRefSymbolBuf { name: RefNameBuf, remote: RemoteNameBuf } (ref_name.rs:345, .as_ref() back). WorkspaceName::DEFAULT = "default" (ref_name.rs:318). parse_git_ref(full_name: &GitRefName) -> Option<(GitRefKind, RemoteRefSymbol<'_>)> (git.rs:274) maps refs/heads/* -> name@git, refs/remotes/r/* -> name@r, refs/tags/*.

### MutableRepo view mutators used by this subsystem  (/Users/lita/src/jj/lib/src/repo.rs:1676)
```rust
pub fn set_local_bookmark_target(&mut self, name: &RefName, target: RefTarget)  /  pub fn set_remote_bookmark(&mut self, symbol: RemoteRefSymbol<'_>, remote_ref: RemoteRef) (repo.rs:1704)  /  pub fn track_remote_bookmark(&mut self, symbol: RemoteRefSymbol<'_>) -> IndexResult<()> (repo.rs:1724)  /  pub fn set_git_ref_target(&mut self, name: &GitRefName, target: RefTarget) (repo.rs:1800)  /  pub fn set_git_head_target(&mut self, target: RefTarget) (repo.rs:1822)
```
gt create uses set_local_bookmark_target(RefName::new(branch), RefTarget::normal(commit_id)); export_refs then materializes it as refs/heads/<branch>. If a fetched remote bookmark came in untracked (state New), call track_remote_bookmark(name.to_remote_symbol(RemoteName::new("origin"))) to merge it into the local bookmark.

## Minimal flow
// ---- gt init (colocated) ----
let settings: UserSettings = /* from config */;
let (workspace, repo) = Workspace::init_colocated_git(&settings, workspace_root)?;   // creates <root>/.git (WithWorktree), .jj/, initial op
// fresh repo: nothing to import; .git HEAD is unborn symref to init.defaultBranch

// ---- every mutating command (create/modify/checkout/sync) shares this "three writes" epilogue ----
// (0) snapshot working copy first:
let mut locked_ws = workspace.start_working_copy_mutation()?;
let new_tree_id = locked_ws.locked_wc().snapshot(&SnapshotOptions{..})?;             // write #0: read disk -> store
//   ...amend @ with new_tree_id inside a Transaction if changed...
let mut tx = repo.start_transaction();
//   ... mutate: tx.repo_mut().new_commit(...)/rewrite_commit(...), tx.repo_mut().set_local_bookmark_target(RefName::new("feat-1"), RefTarget::normal(id)), tx.repo_mut().check_out(WorkspaceName::DEFAULT.to_owned(), &commit)?, tx.repo_mut().rebase_descendants()? ...
// WRITE #2a: git HEAD+index (colocated only), BEFORE commit:
let wc_commit = tx.repo().store().get_commit(tx.repo().view().get_wc_commit_id(WorkspaceName::DEFAULT).unwrap())?;
jj_lib::git::reset_head(tx.repo_mut(), &wc_commit)?;                                  // HEAD = detached @ parent-of-@, index = parent tree
// WRITE #2b: git refs, BEFORE commit:
let export_stats = jj_lib::git::export_refs(tx.repo_mut())?;                          // refs/heads/* etc.; check export_stats.failed_bookmarks
// WRITE #1: op log:
let repo = tx.commit("gt create feat-1")?;                                            // Arc<ReadonlyRepo> at new operation
// WRITE #3: working-copy files + workspace state:
locked_ws.locked_wc().check_out(&wc_commit)?  /* or .reset(&wc_commit) if files already match */;
locked_ws.finish(repo.op_id().clone())?;

// ---- gt submit ----
let git_settings = GitSettings::from_settings(&settings)?;
let mut tx = repo.start_transaction();
let mut updates = Vec::new();
for name /* RefNameBuf */ in stack_bookmarks {
    let targets = LocalAndRemoteRef { local_target: tx.repo().view().get_local_bookmark(&name),
                                      remote_ref: tx.repo().view().get_remote_bookmark(name.to_remote_symbol(RemoteName::new("origin"))) };
    if let BookmarkPushAction::Update(update) = classify_bookmark_push_action(targets) { updates.push((name, update)); }
}
let stats: GitPushStats = jj_lib::git::push_branches(tx.repo_mut(), &git_settings, RemoteName::new("origin"),
        &GitBranchPushTargets { branch_updates: updates }, RemoteCallbacks::default())?;   // subprocess: git push --porcelain --no-verify --force-with-lease=...
if !stats.all_ok() { /* report stats.rejected / stats.remote_rejected; view not updated */ }
let repo = tx.commit("gt submit")?;   // records moved refs/remotes/origin/* in view; no wc change, but still export_refs/reset_head if bookmarks moved
// then create/update PRs via GitHub API using pushed branch names (outside jj-lib)

// ---- gt sync ----
let import_options = GitImportOptions { auto_local_bookmark: false, abandon_unreachable_commits: true, remote_auto_track_bookmarks: HashMap::new() };
let mut tx = repo.start_transaction();
{
    let mut gf = GitFetch::new(tx.repo_mut(), &git_settings, &import_options)?;
    let refspecs = expand_fetch_refspecs(RemoteName::new("origin"), StringExpression::pattern(StringPattern::everything()))?;
    gf.fetch(RemoteName::new("origin"), refspecs, RemoteCallbacks::default(), None, None)?;  // subprocess: git fetch --prune --no-write-fetch-head
    let default_branch: Option<RefNameBuf> = gf.get_default_branch(RemoteName::new("origin"))?;
    let import_stats: GitImportStats = gf.import_refs()?;      // NOW the view sees origin/main; merged PR heads may be in abandoned_commits
}
tx.repo_mut().track_remote_bookmark(RefName::new("main").to_remote_symbol(RemoteName::new("origin")))?; // if not already tracked
// rebase stack onto new main target: rebase_commit(...)/rewrite loop + tx.repo_mut().rebase_descendants()?;
// delete bookmarks of merged PRs: tx.repo_mut().set_local_bookmark_target(&name, RefTarget::absent());
// then the same epilogue: reset_head -> export_refs -> tx.commit -> locked_ws check_out/finish

## Gotchas
- Transaction::commit writes ONLY the op log. In a colocated repo you must yourself call git::reset_head (HEAD+index) and git::export_refs (refs/heads/*) on tx.repo_mut() BEFORE tx.commit, and update the working copy (LockedWorkingCopy::check_out/reset + LockedWorkingCopy/LockedWorkspace finish with the new op id) AFTER tx.commit. Reference implementation: cli/src/cli_util.rs:2084-2119.
- reset_head sets HEAD detached at the FIRST PARENT of the working-copy commit, not at the wc commit itself (git.rs:1418). If the parent is root, HEAD becomes a symref to placeholder refs/jj/root (git.rs:1377). GitResetHeadError::UpdateHeadRef indicates a concurrent HEAD move and is treated by jj as a warning, not fatal.
- export_refs never returns per-ref errors as Err — refs that raced with git changes land in GitExportStats::failed_bookmarks with FailedRefExportReason and are silently skipped; a following import will mark them conflicted. Check the stats.
- export_refs also mutates the view (records exported refs in view.git_refs() and mirrors local bookmarks to the "git" pseudo-remote), so it must run inside the same transaction, before commit.
- GitFetch::fetch only updates refs/remotes/* inside .git; the jj view is unchanged until GitFetch::import_refs(). Forgetting the second call makes fetch appear to do nothing.
- Auto-tracking: with default settings a newly fetched bookmark (e.g. origin/main on the very first fetch into a repo created by gt init) gets RemoteRefState::New — NO local bookmark is created and merge_local_bookmark is skipped. Set GitImportOptions.auto_local_bookmark=true, add a remote_auto_track_bookmarks matcher, or call MutableRepo::track_remote_bookmark(symbol) (repo.rs:1724) explicitly.
- All fetch/push goes through the system `git` CLI (min version 2.40.4, path from git.executable-path), spawned with stdin=null and env inherited. RemoteCallbacks' get_password/get_username_password/get_ssh_keys are NEVER invoked on this path — HTTPS token auth must come from git's own credential machinery (gh auth setup-git, credential.helper, GIT_ASKPASS pointing at a non-interactive script, or token-in-URL). Interactive prompts inside the subprocess will hang/fail because stdin is null.
- push semantics: every update is force-with-lease against BookmarkPushUpdate.old_target (None lease = ref must not exist on remote — right for a first gt submit of a new branch). Wrong old_target => stats.rejected. push_branches updates remote-tracking view state only when ALL refs succeed; on partial failure your transaction view is untouched even though some refs actually landed on the remote — refetch to reconcile.
- Remote names: validate_remote_name (git.rs:116) rejects the reserved remote "git" (REMOTE_NAME_FOR_LOCAL_GIT_REPO) and any remote containing '/'. RemoteName::new()/RefName::new() are zero-cost casts and do no validation.
- GitFetch::new borrows the MutableRepo mutably for the struct's lifetime — finish all fetch()/import_refs() calls (drop the GitFetch) before touching tx.repo_mut() again.
- fetch branch patterns go through StringPattern -> glob; names containing INVALID_REFSPEC_CHARS (or literal '*') are rejected with GitRefExpansionError::InvalidBranchPattern. Use StringExpression::exact/pattern from str_util (StringExpression::exact at str_util.rs:380).
- GitBackend::init_colocated's workspace_root argument is interpreted RELATIVE TO store_path (it does store_path.join(workspace_root), git_backend.rs:233) — use Workspace::init_colocated_git which precomputes the store-relative path, rather than calling the backend directly.
- abandon_unreachable_commits=true in GitImportOptions is what makes gt sync auto-abandon merged/deleted branch commits after fetch (import_some_refs -> abandon_unreachable_commits, git.rs:595-600); pair with tx.repo_mut().rebase_descendants() to evict them from the stack.
- jj-lib's git module is behind the "git" cargo feature (Workspace::init_colocated_git is #[cfg(feature = "git")]); jj-lib 0.36.0 enables it by default but keep it in mind if you set default-features=false.


---

# cli-per-command

## Summary

cli/src/cli_util.rs is the layer of jj's CLI that silently performs, around EVERY command's own logic, exactly the "three writes" story. BEFORE command logic (CommandHelper::workspace_helper, cli_util.rs:432 -> workspace_helper_with_stats:445 -> workspace_helper_no_snapshot:474 -> maybe_snapshot_impl:1118): (1) load Workspace (load_workspace:501); (2) resolve_operation:659 resolves op heads — if multiple heads exist it auto-merges them in its OWN transaction ('reconcile divergent operations', tx.write(..)?.leave_unpublished(), :689-693); (3) load repo at that op (:480); (4) WorkspaceCommandHelper::new:1045 computes may_update_working_copy = loaded_at_head && !--ignore-working-copy (:1055) and working_copy_shared_with_git = git_util::is_colocated_git_workspace (:1057, git_util.rs:60); (5) maybe_snapshot_impl:1118 — skip everything if !may_update_working_copy; else take the colocated file lock .jj/repo/git_import_export.lock (lock_git_import_export:1102), reload repo if op heads moved while waiting (:1131-1149), if colocated import_git_head:1190 (own tx: git::import_head, check_out new head commit, locked_wc().reset(&wc_commit) to move wc STATE without touching files :1215, tx.commit("import git head")), then snapshot_working_copy:1895 (wc lock via start_working_copy_mutation, staleness check via handle_stale_working_copy:2602/WorkingCopyFreshness::check_stale working_copy.rs:363, locked_wc().snapshot(); if tree changed: a NEW transaction flagged set_is_snapshot(true) :1936 that rewrite_commit().set_tree().write(), set_wc_commit, rebase_descendants, and — colocated — export_working_copy_changes_to_git:2366 = git::update_intent_to_add + git::export_refs, then tx.commit("snapshot working copy") :1968; finally locked_ws.finish(op_id) :1972 records the new op in working-copy state), then if colocated import_git_refs:1244 (third possible tx: git::import_refs + rebase_descendants + finish_transaction). AFTER a mutation (WorkspaceCommandTransaction::finish:2481 -> finish_transaction:2041): has_changes() guard ('Nothing changed.'), re-take git_import_export lock (:2488), rebase_descendants (:2048), re-check_out if wc commit became immutable (:2053-2068), capture old/new wc commit from base vs mutated view (:2070-2082), THEN — still inside the transaction, before commit — git::reset_head(tx.repo_mut(), wc_commit) :2094 (HEAD + git index; UpdateHeadRef failure downgraded to warning) and git::export_refs(tx.repo_mut()) :2103 [WRITE 3: git refs], then tx.commit(description) :2107 [WRITE 0: op log — the only write people think about], then update_working_copy:1978 -> free fn update_working_copy:2934 -> Workspace::check_out(repo.op_id(), old_tree, new_commit) (workspace.rs:428) which does locked_wc.check_out(commit) [WRITE 1: files on disk] and LockedWorkspace::finish(op_id) (workspace.rs:466) [WRITE 2: workspace/working-copy state binding wc to the new operation], then prints 'Working copy  (@) now at: ' (print_updated_working_copy_stats:1994, literal at :2005, note the double space, plus 'Parent commit (@-)      : ' per parent :2011) and checkout stats (print_checkout_stats:2874). Colocation is detected purely structurally (GitBackend + non-bare + workdir == workspace root, git_util.rs:60-76) and adds per command: the file lock, op-head reload, import_head+import_refs before, reset_head+export_refs at finish, and update_intent_to_add+export_refs after each snapshot. So a single ordinary colocated jj command routinely commits TWO-to-FOUR operations to the op log (snapshot op, optional import-head op, optional import-refs op, command op), and a gt built on jj-lib must reproduce this whole sandwich or the working copy/git repo drift stale.

## Key APIs

### CommandHelper::workspace_helper (BEFORE-pipeline entry)  (/Users/lita/src/jj/cli/src/cli_util.rs:432)
```rust
pub fn workspace_helper(&self, ui: &Ui) -> Result<WorkspaceCommandHelper, CommandError>
```
= workspace_helper_with_stats(:445) + print_snapshot_stats(:2844). with_stats catches SnapshotWorkingCopyError::StaleWorkingCopy and, if config snapshot.auto-update-stale, calls recover_stale_working_copy(:534).

### CommandHelper::workspace_helper_no_snapshot (load only)  (/Users/lita/src/jj/cli/src/cli_util.rs:474)
```rust
pub fn workspace_helper_no_snapshot(&self, ui: &Ui) -> Result<WorkspaceCommandHelper, CommandError>
```
load_workspace(:501) -> resolve_operation(:659) -> repo_loader().load_at(&op_head)(:480) -> WorkspaceCommandHelper::new(:483).

### CommandHelper::resolve_operation (op-head merge)  (/Users/lita/src/jj/cli/src/cli_util.rs:659)
```rust
pub fn resolve_operation(&self, ui: &Ui, repo_loader: &RepoLoader) -> Result<Operation, CommandError>
```
No --at-op: op_heads_store::resolve_op_heads(...); on multiple heads prints 'Concurrent modification detected, resolving automatically.', merges each head with tx.merge_operation + rebase_descendants, writes tx.write("reconcile divergent operations")?.leave_unpublished() (:689-693). An extra hidden operation.

### CommandHelper::for_workable_repo  (/Users/lita/src/jj/cli/src/cli_util.rs:703)
```rust
pub fn for_workable_repo(&self, ui: &Ui, workspace: Workspace, repo: Arc<ReadonlyRepo>) -> Result<WorkspaceCommandHelper, CommandError>
```
Forces loaded_at_head=true; used when you constructed workspace+repo yourself (e.g. right after init/clone) and the view is in sync with the wc.

### WorkspaceCommandHelper::new (private)  (/Users/lita/src/jj/cli/src/cli_util.rs:1045)
```rust
fn new(ui: &Ui, workspace: Workspace, repo: Arc<ReadonlyRepo>, env: WorkspaceCommandEnvironment, loaded_at_head: bool) -> Result<Self, CommandError>
```
Sets may_update_working_copy = loaded_at_head && !ignore_working_copy (:1055-1056); working_copy_shared_with_git = crate::git_util::is_colocated_git_workspace(&workspace,&repo) (:1057-1058). Private — a library consumer replicates these two booleans.

### is_colocated_git_workspace (colocation detection)  (/Users/lita/src/jj/cli/src/git_util.rs:60)
```rust
pub fn is_colocated_git_workspace(workspace: &Workspace, repo: &ReadonlyRepo) -> bool
```
true iff git::get_git_backend(repo.store()) ok AND git_backend.git_workdir() Some (non-bare) AND workdir == workspace.workspace_root() (or canonicalized .git parent matches).

### WorkspaceCommandHelper::maybe_snapshot (public BEFORE hook)  (/Users/lita/src/jj/cli/src/cli_util.rs:1174)
```rust
pub fn maybe_snapshot(&mut self, ui: &Ui) -> Result<(), CommandError>
```
Wraps maybe_snapshot_impl(:1118). Impl order: (1) no-op if !may_update_working_copy; (2) lock_git_import_export(:1102) — FileLock on workspace.repo_path().join("git_import_export.lock"), colocated only; (3) colocated: reload repo at op heads if they moved while waiting for lock (:1131-1149); (4) colocated: import_git_head (:1151-1155); (5) snapshot_working_copy (:1160); (6) colocated: import_git_refs (:1163-1167).

### WorkspaceCommandHelper::import_git_head (private)  (/Users/lita/src/jj/cli/src/cli_util.rs:1190)
```rust
fn import_git_head(&mut self, ui: &Ui, git_import_export_lock: &GitImportExportLock) -> Result<(), CommandError>
```
Own tx: jj_lib::git::import_head(tx.repo_mut()) (git.rs:847 pub fn import_head(mut_repo: &mut MutableRepo) -> Result<(), GitImportError>). If HEAD moved: tx.repo_mut().check_out(workspace_name, &new_git_head_commit), then locked_ws.locked_wc().reset(&wc_commit).block_on() (:1215) — resets wc STATE, not files, because git already updated files — rebase_descendants, tx.commit("import git head") (:1217), locked_ws.finish(op_id) (:1218).

### WorkspaceCommandHelper::import_git_refs (private)  (/Users/lita/src/jj/cli/src/cli_util.rs:1244)
```rust
fn import_git_refs(&mut self, ui: &Ui, git_import_export_lock: &GitImportExportLock) -> Result<(), CommandError>
```
Own tx: git::import_refs(tx.repo_mut(), &import_options) (git.rs:473 pub fn import_refs(mut_repo: &mut MutableRepo, options: &GitImportOptions) -> Result<GitImportStats, GitImportError>), rebase_descendants, then finish_transaction(ui, tx, "import git refs", lock) — i.e. full AFTER pipeline including wc update, since import can rebase the wc commit.

### WorkspaceCommandHelper::snapshot_working_copy (private)  (/Users/lita/src/jj/cli/src/cli_util.rs:1895)
```rust
fn snapshot_working_copy(&mut self, ui: &Ui) -> Result<SnapshotStats, SnapshotWorkingCopyError>
```
start_working_copy_mutation (wc lock) :1909; handle_stale_working_copy(:2602) may reload repo (Updated) or error (WorkingCopyStale/SiblingOperation); locked_ws.locked_wc().snapshot(&options).block_on() :1927-1931; if new_tree != wc_commit tree: start_repo_transaction + tx.set_is_snapshot(true) :1936; mut_repo.rewrite_commit(&wc_commit).set_tree(new_tree).write() :1938-1942; mut_repo.set_wc_commit(workspace_name, commit.id().clone()) :1943-1945; rebase_descendants :1948; colocated: export_working_copy_changes_to_git(ui, mut_repo, &old_tree, &new_tree) :1963; tx.commit("snapshot working copy") :1968; ALWAYS locked_ws.finish(self.user_repo.repo.op_id().clone()) :1972-1974 (wc state write, even when tree unchanged).

### export_working_copy_changes_to_git  (/Users/lita/src/jj/cli/src/cli_util.rs:2366)
```rust
pub fn export_working_copy_changes_to_git(ui: &Ui, mut_repo: &mut MutableRepo, old_tree: &MergedTree, new_tree: &MergedTree) -> Result<(), CommandError>
```
= jj_lib::git::update_intent_to_add(repo, old_tree, new_tree) (git.rs:1668, keeps git index intent-to-add entries in sync so `git status` looks right) + jj_lib::git::export_refs(mut_repo) (git.rs:967 -> Result<GitExportStats, GitExportError>). Called inside the snapshot transaction before its commit.

### WorkspaceCommandHelper::start_transaction  (/Users/lita/src/jj/cli/src/cli_util.rs:2031)
```rust
pub fn start_transaction(&mut self) -> WorkspaceCommandTransaction<'_>
```
Wraps start_repo_transaction(self.repo(), string_args) (:2567) which sets op tag "args" to the shell-escaped command line.

### WorkspaceCommandTransaction::finish (AFTER-pipeline entry)  (/Users/lita/src/jj/cli/src/cli_util.rs:2481)
```rust
pub fn finish(self, ui: &Ui, description: impl Into<String>) -> Result<(), CommandError>
```
Early-return 'Nothing changed.' if !tx.repo().has_changes(); re-acquires git_import_export lock (:2488) so HEAD export is atomic with op commit; delegates to finish_transaction. into_inner() (:2497) opts out — caller then owns rebase/export/wc-update.

### WorkspaceCommandHelper::finish_transaction (private, THE after-sequence)  (/Users/lita/src/jj/cli/src/cli_util.rs:2041)
```rust
fn finish_transaction(&mut self, ui: &Ui, mut tx: Transaction, description: impl Into<String>, _git_import_export_lock: &GitImportExportLock) -> Result<(), CommandError>
```
Order: (1) tx.repo_mut().rebase_descendants() :2048; (2) per-workspace immutable-wc fixup: repo_mut().check_out(name, &wc_commit) :2053-2068; (3) old/new wc commit from tx.base_repo().view().get_wc_commit_id(name) vs tx.repo().view() :2070-2082; (4) colocated, pre-commit: jj_lib::git::reset_head(tx.repo_mut(), wc_commit) :2094 (git.rs:1415; GitResetHeadError::UpdateHeadRef only warns :2096-2099) then jj_lib::git::export_refs(tx.repo_mut()) :2103 [git-refs write]; (5) tx.commit(description) :2107 [op-log write; Transaction::commit = write()?.publish(), transaction.rs:120]; (6) if may_update_working_copy: self.update_working_copy(ui, old, new) :2112-2114 [file + wc-state writes]; (7) report_repo_changes :2121; (8) user.name/email warnings :2123-2151.

### WorkspaceCommandHelper::update_working_copy (private) + free update_working_copy  (/Users/lita/src/jj/cli/src/cli_util.rs:1978 and 2934)
```rust
fn update_working_copy(&mut self, ui: &Ui, maybe_old_commit: Option<&Commit>, new_commit: &Commit) -> Result<(), CommandError> / pub fn update_working_copy(repo: &Arc<ReadonlyRepo>, workspace: &mut Workspace, old_commit: Option<&Commit>, new_commit: &Commit) -> Result<CheckoutStats, CommandError>
```
Free fn: old_tree = old_commit.map(|c| c.tree()); workspace.check_out(repo.op_id().clone(), old_tree.as_ref(), new_commit). Method then calls print_updated_working_copy_stats(:1994).

### Workspace::check_out (jj-lib; writes 1+2)  (/Users/lita/src/jj/lib/src/workspace.rs:428)
```rust
pub fn check_out(&mut self, operation_id: OperationId, old_tree: Option<&MergedTree>, commit: &Commit) -> Result<CheckoutStats, CheckoutError>
```
start_working_copy_mutation(); if old_tree given and its tree_ids_and_labels() != locked_wc.old_tree()'s -> CheckoutError::ConcurrentCheckout; locked_wc.check_out(commit).block_on() [files on disk]; locked_ws.finish(operation_id) (workspace.rs:466) [wc state <- new op id].

### WorkingCopyFreshness::check_stale (jj-lib)  (/Users/lita/src/jj/lib/src/working_copy.rs:363)
```rust
pub fn check_stale(locked_wc: &dyn LockedWorkingCopy, wc_commit: &Commit, repo: &ReadonlyRepo) -> Result<Self, OpStoreError>
```
locked_wc.old_operation_id()==repo.op_id() -> Fresh; else op-DAG common ancestor: ancestor==repo op -> Updated(wc_op) (caller must repo.reload_at(&wc_op), see handle_stale_working_copy cli_util.rs:2620-2629); ancestor==wc op -> WorkingCopyStale (unless trees already equal -> Fresh); else SiblingOperation.

### handle_stale_working_copy / update_stale_working_copy (cli helpers)  (/Users/lita/src/jj/cli/src/cli_util.rs:2602 and 2661)
```rust
fn handle_stale_working_copy(locked_wc: &mut dyn LockedWorkingCopy, repo: Arc<ReadonlyRepo>, workspace_name: &WorkspaceName) -> Result<Option<(Arc<ReadonlyRepo>, Commit)>, SnapshotWorkingCopyError> / fn update_stale_working_copy(mut locked_ws: LockedWorkspace, op_id: OperationId, stale_commit: &Commit, new_commit: &Commit) -> Result<CheckoutStats, CommandError>
```
Former: Ok(None) means workspace deleted from view — skip snapshot. Latter (used by recover_stale_working_copy:534): verifies stale_commit tree == locked_wc.old_tree(), locked_wc.check_out(new_commit), locked_ws.finish(op_id).

### 'Working copy now at' output  (/Users/lita/src/jj/cli/src/cli_util.rs:2005)
```rust
write!(formatter, "Working copy  (@) now at: ")  /  write!(formatter, "Parent commit (@-)      : ")
```
In print_updated_working_copy_stats(:1994): printed only when Some(new_commit) != maybe_old_commit, rendered via templates.commit_summary; followed by print_checkout_stats(:2874) 'Added {} files, modified {} files, removed {} files' and a conflicted-paths warning if new_commit.has_conflict().

### GitImportExportLock / lock_git_import_export  (/Users/lita/src/jj/cli/src/cli_util.rs:1005 and 1102)
```rust
pub struct GitImportExportLock { _lock: Option<FileLock> } / fn lock_git_import_export(&self) -> Result<GitImportExportLock, CommandError>
```
Colocated only: FileLock::lock(workspace.repo_path().join("git_import_export.lock")) (.jj/repo/git_import_export.lock). Held across import->snapshot->export (maybe_snapshot_impl) and across finish_transaction; empty token when not colocated.

### Transaction::commit / write / set_is_snapshot (jj-lib)  (/Users/lita/src/jj/lib/src/transaction.rs:120)
```rust
pub fn commit(self, description: impl Into<String>) -> Result<Arc<ReadonlyRepo>, TransactionCommitError>  // = self.write(description)?.publish()
```
write() stores view+operation without publishing (used for op-head reconcile); publish() advances op heads. set_is_snapshot(true) (:115) marks the snapshot op's metadata so `jj op log` can label it.

## Minimal flow
// gt command skeleton for a COLOCATED repo, mirroring jj CLI's hidden sandwich.
// == LOAD ==
let mut workspace = Workspace::load(&settings, &ws_root, &store_factories, &wc_factories)?;
let colocated = /* GitBackend + non-bare + workdir == ws_root; git_util.rs:60 */;
let op = op_heads_store::resolve_op_heads(loader.op_heads_store().as_ref(), loader.op_store(), |heads| {
    // merge divergent op heads in their own tx (cli_util.rs:667-696)
    let base = loader.load_at(&heads[0])?; let mut tx = base.start_transaction();
    for h in heads.into_iter().skip(1) { tx.merge_operation(h)?; tx.repo_mut().rebase_descendants()?; }
    Ok(tx.write("reconcile divergent operations")?.leave_unpublished().operation().clone())
})?;
let mut repo: Arc<ReadonlyRepo> = loader.load_at(&op)?;
// == BEFORE (what maybe_snapshot_impl does, cli_util.rs:1118) ==
let _giel = FileLock::lock(workspace.repo_path().join("git_import_export.lock"))?; // colocated only
// (re-resolve op heads under the lock and reload repo if they moved, :1131-1149)
// 1. import git HEAD (own op, :1190):
let mut tx = repo.start_transaction();
jj_lib::git::import_head(tx.repo_mut())?;
if tx.repo().has_changes() {
    let mut tx = tx.into_inner();
    if let Some(head_id) = tx.repo().view().git_head().as_normal().cloned() {
        let head_commit = tx.repo().store().get_commit(&head_id)?;
        let wc_commit = tx.repo_mut().check_out(ws_name.clone(), &head_commit)?;
        let mut locked_ws = workspace.start_working_copy_mutation()?;
        locked_ws.locked_wc().reset(&wc_commit).block_on()?;   // state only; git already moved the files
        tx.repo_mut().rebase_descendants()?;
        repo = tx.commit("import git head")?;
        locked_ws.finish(repo.op_id().clone())?;               // WRITE 2
    }
}
// 2. snapshot working copy (own op, :1895):
let mut locked_ws = workspace.start_working_copy_mutation()?;
let wc_commit = repo.store().get_commit(repo.view().get_wc_commit_id(&ws_name).unwrap())?;
match WorkingCopyFreshness::check_stale(locked_ws.locked_wc(), &wc_commit, &repo)? {
    Fresh => {}, Updated(wc_op) => repo = repo.reload_at(&wc_op)?,
    WorkingCopyStale | SiblingOperation => return Err("stale working copy"),
}
let (new_tree, _stats) = locked_ws.locked_wc().snapshot(&snapshot_options).block_on()?;
if new_tree.tree_ids_and_labels() != wc_commit.tree().tree_ids_and_labels() {
    let mut tx = repo.start_transaction(); tx.set_is_snapshot(true);
    let commit = tx.repo_mut().rewrite_commit(&wc_commit).set_tree(new_tree).write()?;
    tx.repo_mut().set_wc_commit(ws_name.clone(), commit.id().clone())?;
    tx.repo_mut().rebase_descendants()?;
    jj_lib::git::update_intent_to_add(tx.base_repo().as_ref(), &wc_commit.tree(), &commit.tree())?;
    jj_lib::git::export_refs(tx.repo_mut())?;                  // WRITE 3 (early, for `git status` sanity)
    repo = tx.commit("snapshot working copy")?;                // WRITE 0 (op #1)
}
locked_ws.finish(repo.op_id().clone())?;                       // WRITE 2 — even if tree unchanged
// 3. import git refs (own op, :1244): tx = repo.start_transaction(); git::import_refs(tx.repo_mut(), &opts)?;
//    if changed: rebase_descendants + run the AFTER sequence below with description "import git refs".
// == COMMAND LOGIC ==
let mut tx = repo.start_transaction();
/* gt create: let c = tx.repo_mut().new_commit(vec![parent_id], tree_id).write()?;
   tx.repo_mut().check_out(ws_name.clone(), &c)?; tx.repo_mut().set_local_bookmark_target(&name, RefTarget::normal(...)); */
// == AFTER (finish_transaction, cli_util.rs:2041) ==
if !tx.repo().has_changes() { println!("Nothing changed."); return Ok(()); }
tx.repo_mut().rebase_descendants()?;
let old_wc = tx.base_repo().view().get_wc_commit_id(&ws_name).map(|id| tx.base_repo().store().get_commit(id)).transpose()?;
let new_wc = tx.repo().view().get_wc_commit_id(&ws_name).map(|id| tx.repo().store().get_commit(id)).transpose()?;
if colocated {
    if let Some(wc) = &new_wc { jj_lib::git::reset_head(tx.repo_mut(), wc)?; } // git HEAD+index -> wc parent
    jj_lib::git::export_refs(tx.repo_mut())?;                  // WRITE 3: bookmarks -> refs/heads/*
}
let repo = tx.commit("gt create ...")?;                        // WRITE 0 (op #2): op log
if let Some(new_wc) = &new_wc {
    let old_tree = old_wc.as_ref().map(|c| c.tree());
    let stats = workspace.check_out(repo.op_id().clone(), old_tree.as_ref(), new_wc)?; // WRITE 1 (files) + WRITE 2 (wc state)
    println!("Working copy  (@) now at: {}", summarize(new_wc)); // cli_util.rs:2005
    println!("Added {} files, modified {} files, removed {} files", stats.added_files, stats.updated_files, stats.removed_files);
}

## Gotchas
- Two-plus operations per command: snapshot_working_copy commits its own operation ('snapshot working copy', tx.set_is_snapshot(true), cli_util.rs:1936/1968) BEFORE the command's transaction; colocated repos may add 'import git head' (:1217) and 'import git refs' (:1270) operations too. gt must not assume one op per command.
- Ordering inside finish_transaction is load-bearing: git::reset_head + git::export_refs run on tx.repo_mut() BEFORE tx.commit (cli_util.rs:2094-2107) because export mutates the view's recorded ref state, so it must be part of the committed operation; workspace.check_out runs AFTER tx.commit because LockedWorkspace::finish needs the NEW operation id. Doing export after commit silently loses the exported-ref bookkeeping; doing checkout before commit records a wrong op id and every later command sees a stale working copy.
- locked_ws.finish(op_id) must be called even when the snapshot found no changes (cli_util.rs:1972-1974) — it advances the working-copy state's operation pointer; skipping it makes the next command think the wc is stale.
- LockedWorkspace::finish takes the op id of the repo you just committed, not the pre-transaction repo: snapshot does tx.commit first, updates self.user_repo, then locked_ws.finish(self.user_repo.repo.op_id().clone()). The wc lock (start_working_copy_mutation) is held ACROSS the transaction commit — that overlap is intentional.
- Colocated-only file lock: .jj/repo/git_import_export.lock (workspace.repo_path().join(...), cli_util.rs:1104) is taken around the whole import->snapshot->export cycle AND again around finish_transaction (:2488); after acquiring it, jj re-resolves op heads and reloads the repo (:1131-1149) to avoid divergent operations. A gt that skips this can race with concurrent git/jj processes.
- import_git_head updates working-copy STATE without touching files: locked_wc().reset(&wc_commit) (cli_util.rs:1215) — the assumption is git itself already rewrote the files. Using check_out here would double-apply changes.
- Two distinct staleness mechanisms: (a) at snapshot time, WorkingCopyFreshness::check_stale (working_copy.rs:363) — Updated(op) means the REPO is behind and you must repo.reload_at(&wc_op) (handle_stale_working_copy, cli_util.rs:2620), while WorkingCopyStale is an error unless snapshot.auto-update-stale triggers recover_stale_working_copy (:534); (b) at checkout time, Workspace::check_out compares your expected old tree with locked_wc.old_tree() and returns CheckoutError::ConcurrentCheckout (workspace.rs:439-445).
- reset_head failures of kind GitResetHeadError::UpdateHeadRef are deliberately downgraded to warnings (cli_util.rs:2096-2099) — a concurrent `git checkout` may have moved HEAD; the truth is re-imported on next snapshot. Other reset_head errors abort.
- export_refs is called from TWO places per command in a colocated repo: inside the snapshot transaction (via export_working_copy_changes_to_git, cli_util.rs:1963/2374, paired with update_intent_to_add so `git status` stays sane) and inside finish_transaction (:2103). Bookmarks moved by your command only reach refs/heads/* via the second call.
- may_update_working_copy = loaded_at_head && !--ignore-working-copy (cli_util.rs:1055): when false, maybe_snapshot_impl is a complete no-op (no git import either) and finish_transaction skips the checkout — the three writes collapse to one (op log only). check_working_copy_writable (:1082) is the guard that errors instead.
- The exact status string has TWO spaces: "Working copy  (@) now at: " and parents print as "Parent commit (@-)      : " (cli_util.rs:2005/2011); it's only printed when the wc commit actually changed (Some(new) != old) and is followed by conflict warnings if new_commit.has_conflict().
- WorkspaceCommandHelper::new, snapshot_working_copy, import_git_head/refs, finish_transaction, and update_working_copy(method) are all PRIVATE; the public surface is workspace_helper()/maybe_snapshot()/start_transaction()/WorkspaceCommandTransaction::finish()/into_inner() plus the free fns update_working_copy (:2934) and export_working_copy_changes_to_git (:2366). A standalone gt binary that links only jj-lib (not jj-cli) must reimplement the private ones; if you link jj-cli as a library, drive everything through CommandHelper/WorkspaceCommandTransaction instead.
- Multiple op heads are auto-merged at load (resolve_operation, cli_util.rs:667-696) in an unpublished 'reconcile divergent operations' operation — yet another op-log write a naive consumer never sees.
- finish_transaction also re-checks whether the wc commit became immutable and creates a new empty commit on top (cli_util.rs:2053-2068), and it rebases descendants FIRST (:2048) — tx.commit asserts !has_rewrites (transaction.rs:136-139), so forgetting rebase_descendants panics.


---

# commits-rebase-revsets

## Summary

jj-lib 0.36 in-memory mutation layer: all graph edits (new commit, amend, rebase, bookmark moves, abandons) happen on MutableRepo inside a Transaction, then tx.commit() writes ONLY the op-store/view (write #1 of three). Transaction::write (lib/src/transaction.rs:130) hard-asserts `!mut_repo.has_rewrites()` ("BUG: Descendants have not been rebased after the last rewrites.", transaction.rs:136-139), so descendant rebasing is NEVER automatic at commit — the consumer must call rebase_descendants() explicitly first. During rebase_descendants/transform_*, update_rewritten_references (repo.rs:1125) automatically drags local bookmarks and workspace wc-commit pointers along to rewritten commits (that is the ONLY place bookmarks follow rewrites). Working-copy files on disk (write #2, via workspace/working-copy checkout APIs) and git refs in the colocated repo (write #3, via git::export_refs) are separate explicit steps not covered by these files. Revsets can be built and evaluated fully programmatically from CommitIds with zero parse context; index().is_ancestor covers merge detection (but not squash-merges — use EmptyBehavior::AbandonAllEmpty during the sync rebase to auto-drop those).

## Key APIs

### MutableRepo::new_commit  (/Users/lita/src/jj/lib/src/repo.rs:948)
```rust
pub fn new_commit(&mut self, parents: Vec<CommitId>, tree: MergedTree) -> CommitBuilder<'_>
```
No separate tree_id param — takes a MergedTree. For an EMPTY commit on parent P pass P's own tree: `let parent = repo.store().get_commit(&main_id)?; mut_repo.new_commit(vec![main_id], parent.tree())`. Commit::tree() -> MergedTree is at commit.rs:124; Commit::tree_ids() -> &Merge<TreeId> at commit.rs:132. parents must be non-empty (asserted, commit_builder.rs:196). Description defaults to "".

### CommitBuilder (attached)  (/Users/lita/src/jj/lib/src/commit_builder.rs:42)
```rust
pub fn set_description(mut self, description: impl Into<String>) -> Self  /* :116 */; pub fn set_parents(mut self, parents: Vec<CommitId>) -> Self /* :66 */; pub fn set_tree(mut self, tree: MergedTree) -> Self /* :88 */; pub fn generate_new_change_id(mut self) -> Self /* :107 */; pub fn clear_rewrite_source(mut self) -> Self /* :57 */; pub fn write(self) -> BackendResult<Commit> /* :163 */; pub fn abandon(self) /* :169 */
```
Builder methods consume self (chain style). write() (via DetachedCommitBuilder::write, commit_builder.rs:399) adds the commit as a head (add_head), records predecessors, and — iff builder came from rewrite_commit — records old→new in parent_mapping via mut_repo.set_rewritten_commit (commit_builder.rs:421-423). write() errors if the produced commit id already exists in the index (commit_builder.rs:404-418), so amend-with-no-change fails — guard by comparing fields first.

### MutableRepo::rewrite_commit (amend)  (/Users/lita/src/jj/lib/src/repo.rs:954)
```rust
pub fn rewrite_commit(&mut self, predecessor: &Commit) -> CommitBuilder<'_>
```
The public entry to CommitBuilder::for_rewrite_from (commit_builder.rs:227, pub(crate) — you cannot call it directly). Builder is pre-populated with predecessor's parents/tree/description/change_id; committer reset to current user, predecessors=[old id]. Amend description: rewrite_commit(&wc).set_description(msg).write()?. Amend tree (gt modify): rewrite_commit(&wc).set_tree(new_tree).write()?. Keeps same change id => not divergent because old commit becomes hidden.

### MutableRepo::set_rewritten_commit / record_abandoned_commit / record_abandoned_commit_with_parents  (/Users/lita/src/jj/lib/src/repo.rs:971,1005,1019)
```rust
pub fn set_rewritten_commit(&mut self, old_id: CommitId, new_id: CommitId); pub fn record_abandoned_commit(&mut self, old_commit: &Commit); pub fn record_abandoned_commit_with_parents(&mut self, old_id: CommitId, new_parent_ids: impl IntoIterator<Item = CommitId>)
```
NOTE: there is NO `record_rewritten_commit` in 0.36 — the docstring at repo.rs:970 still references that old name but the method is set_rewritten_commit. These populate `parent_mapping`; nothing else happens until a rebase_descendants/transform call. record_abandoned_commit rebases children onto the abandoned commit's parents and moves (or deletes, per RewriteRefsOptions) its bookmarks.

### MutableRepo::rebase_descendants  (/Users/lita/src/jj/lib/src/repo.rs:1428)
```rust
pub fn rebase_descendants(&mut self) -> BackendResult<usize>
```
Rebases all descendants of everything in parent_mapping, then clears parent_mapping. MUST be called before tx.commit() whenever you used rewrite_commit/record_abandoned/set_rewritten — Transaction::write asserts !has_rewrites() and panics otherwise (transaction.rs:136-139). Internally runs update_rewritten_references (repo.rs:1125) which (a) merges every local bookmark pointing at an old commit onto its replacement (update_local_bookmarks repo.rs:1144 — this is how bookmarks 'follow' rewrites), (b) repoints workspace wc-commits (update_wc_commits repo.rs:1179 — for abandoned wc commit it creates a fresh empty commit on the new parents), (c) fixes visible heads. Safe to call even when parent_mapping is empty (returns 0).

### MutableRepo::rebase_descendants_with_options  (/Users/lita/src/jj/lib/src/repo.rs:1396)
```rust
pub fn rebase_descendants_with_options(&mut self, options: &RebaseOptions, mut progress: impl FnMut(Commit, RebasedCommit)) -> BackendResult<()>
```
Same but with RebaseOptions { empty: EmptyBehavior, rewrite_refs: RewriteRefsOptions, simplify_ancestor_merge: bool } (rewrite.rs:431) and a progress callback (old_commit, RebasedCommit). EmptyBehavior (rewrite.rs:410): Keep | AbandonNewlyEmpty | AbandonAllEmpty. RebasedCommit (rewrite.rs:350): Rewritten(Commit) | Abandoned { parent_id: CommitId }. KEY FOR gt sync: set empty: AbandonNewlyEmpty — commits whose changes already landed in new main become empty on rebase and are auto-abandoned; catches squash-merged PRs that is_ancestor cannot detect. RewriteRefsOptions { delete_abandoned_bookmarks: bool } (rewrite.rs:441): true = delete bookmark of merged/abandoned commit (what gt sync wants); false (default) = move bookmark to parent.

### MutableRepo::transform_descendants  (/Users/lita/src/jj/lib/src/repo.rs:1318)
```rust
pub fn transform_descendants(&mut self, roots: Vec<CommitId>, callback: impl AsyncFnMut(CommitRewriter) -> BackendResult<()>) -> BackendResult<()>
```
Visits roots and all their not-yet-rewritten descendants in topo order, callback gets CommitRewriter with new_parents pre-resolved via parent_mapping. Typical body: `if rewriter.parents_changed() { rewriter.rebase().await?.write()?; } Ok(())`. Variant transform_descendants_with_options(&mut self, roots: Vec<CommitId>, new_parents_map: &HashMap<CommitId, Vec<CommitId>>, options: &RewriteRefsOptions, callback) at repo.rs:1334 lets you force specific commits onto new parents (e.g. stack-root -> [new_main]) — this is the direct 'rebase stack onto new main' tool. transform_commits at repo.rs:1352 rewrites only the given Vec<Commit> without their descendants. All of these call update_rewritten_references at the end but do NOT clear parent_mapping (transform_*) — rebase_descendants* DOES clear it.

### rewrite::rebase_commit  (/Users/lita/src/jj/lib/src/rewrite.rs:145)
```rust
pub async fn rebase_commit(mut_repo: &mut MutableRepo, old_commit: Commit, new_parents: Vec<CommitId>) -> BackendResult<Commit>
```
ASYNC — drive with pollster::FutureExt::block_on() like jj itself does (`rebase_commit(repo, c, parents).block_on()?`). Content-aware 3-way tree rebase (old_base_tree, new_base_tree, old_tree merge), records rewrite in parent_mapping; then call mut_repo.rebase_descendants() for the rest of the stack. Simplest one-commit rebase primitive.

### rewrite::CommitRewriter  (/Users/lita/src/jj/lib/src/rewrite.rs:156)
```rust
pub fn new(mut_repo: &'repo mut MutableRepo, old_commit: Commit, new_parents: Vec<CommitId>) -> Self /* :164 */; pub fn parents_changed(&self) -> bool /* :219 */; pub fn set_new_parents(&mut self, new_parents: Vec<CommitId>) /* :192 */; pub async fn rebase(self) -> BackendResult<CommitBuilder<'repo>> /* :335 */; pub async fn rebase_with_empty_behavior(self, empty: EmptyBehavior) -> BackendResult<Option<CommitBuilder<'repo>>> /* :248 (None = abandoned) */; pub fn reparent(self) -> CommitBuilder<'repo> /* :342, keeps tree unchanged */; pub fn abandon(self) /* :239 */
```
rebase() returns a CommitBuilder — you must still .write()? it. rebase_commit_with_options(rewriter, &RebaseOptions) -> BackendResult<RebasedCommit> at rewrite.rs:355 wraps rebase_with_empty_behavior+write. rebase_to_dest_parent(repo: &dyn Repo, sources: &[Commit], destination: &Commit) -> BackendResult<MergedTree> at rewrite.rs:387 only computes a tree (used for duplicate-detection), not a graph rebase.

### Bookmark APIs on MutableRepo  (/Users/lita/src/jj/lib/src/repo.rs:1672-1734)
```rust
pub fn get_local_bookmark(&self, name: &RefName) -> RefTarget /* :1672 */; pub fn set_local_bookmark_target(&mut self, name: &RefName, target: RefTarget) /* :1676 */; pub fn merge_local_bookmark(&mut self, name: &RefName, base_target: &RefTarget, other_target: &RefTarget) -> IndexResult<()> /* :1685 */; pub fn get_remote_bookmark(&self, symbol: RemoteRefSymbol<'_>) -> RemoteRef /* :1699 */; pub fn set_remote_bookmark(&mut self, symbol: RemoteRefSymbol<'_>, remote_ref: RemoteRef) /* :1704 */; pub fn track_remote_bookmark(&mut self, symbol: RemoteRefSymbol<'_>) -> IndexResult<()> /* :1724 */; pub fn untrack_remote_bookmark(&mut self, symbol: RemoteRefSymbol<'_>) /* :1734 */
```
There is no plain `set_local_bookmark` — the method is set_local_bookmark_target, and it also registers target ids as visible heads. Create bookmark: `mut_repo.set_local_bookmark_target("feat-1".as_ref(), RefTarget::normal(commit_id))` (str implements AsRef<RefName>, ref_name.rs:212; or RefName::new("feat-1"), ref_name.rs:157). track_remote_bookmark merges remote target into local (3-way via tracked_target as base) and sets state=Tracked — call after first push or after fetch to link main@origin to main. After a successful push, update the remote view yourself: set_remote_bookmark(symbol, RemoteRef { target: RefTarget::normal(pushed_id), state: RemoteRefState::Tracked }). Bookmarks follow rewritten commits ONLY when rebase_descendants/transform_* runs (via update_local_bookmarks repo.rs:1144); setting a bookmark then rewriting the commit in the same tx works because the rewrite pass merges old→new into the bookmark.

### RefTarget / RemoteRef / RemoteRefSymbol  (/Users/lita/src/jj/lib/src/op_store.rs:52,139,191 and /Users/lita/src/jj/lib/src/ref_name.rs:367)
```rust
pub fn normal(id: CommitId) -> Self /* op_store.rs:82 */; pub fn absent() -> Self /* :64 */; pub fn as_normal(&self) -> Option<&CommitId> /* :104 */; pub fn is_present(&self) -> bool /* :115 */; pub fn has_conflict(&self) -> bool /* :120 */; pub struct RemoteRef { pub target: RefTarget, pub state: RemoteRefState } /* :139 */; pub enum RemoteRefState { New, Tracked } /* :191 */; pub fn tracked_target(&self) -> &RefTarget /* :180 */; pub struct RemoteRefSymbol<'a> { pub name: &'a RefName, pub remote: &'a RemoteName } /* ref_name.rs:367 */
```
RefTarget is a Merge<Option<CommitId>> — always check has_conflict()/as_normal() before pushing. Build a symbol: `RefName::new("feat-1").to_remote_symbol(RemoteName::new("origin"))` (ref_name.rs:311) or RemoteRefSymbol { name: "feat-1".as_ref(), remote: "origin".as_ref() }. refs::classify_bookmark_push_action(LocalAndRemoteRef { local_target, remote_ref }) -> BookmarkPushAction (refs.rs:220; struct at :198) computes what a push should do: Update(BookmarkPushUpdate { old_target: Option<CommitId>, new_target: Option<CommitId> }) | AlreadyMatches | LocalConflicted | RemoteConflicted | RemoteUntracked — ideal for gt submit to compute per-bookmark force-with-lease pushes.

### Programmatic revsets (no parse context needed)  (/Users/lita/src/jj/lib/src/revset.rs:369,511,577,651)
```rust
pub fn commits(commit_ids: Vec<CommitId>) -> Arc<Self> /* :369, also commit() :365, root() :361 */; pub fn descendants(self: &Arc<Self>) -> Arc<Self> /* :511 */; pub fn ancestors(self: &Arc<Self>) -> Arc<Self> /* :466 */; pub fn range(self: &Arc<Self>, heads: &Arc<Self>) -> Arc<Self> /* :577 == self..heads */; pub fn minus/union/intersection /* :612/:597/:607 */; pub fn roots()/heads() /* :456/:451 */; pub fn connected() /* :563 */; impl ResolvedRevsetExpression { pub fn evaluate<'index>(self: Arc<Self>, repo: &'index dyn Repo) -> Result<Box<dyn Revset + 'index>, RevsetEvaluationError> /* :651 */ }
```
RevsetExpression::commits/root are generic over expression state, so type them as Arc<ResolvedRevsetExpression> and call .evaluate(repo) directly — no parsing, no SymbolResolver, no RevsetParseContext. Stack between trunk and leaf: `RevsetExpression::commits(vec![main_id]).range(&RevsetExpression::commits(vec![leaf_id])).evaluate(mut_repo)?` (works on MutableRepo since it implements Repo; jj-lib itself does exactly this in repo.rs:1246 find_descendants_for_rebase). Revset trait (revset.rs:3345): iter() yields Result<CommitId,_> in topo order CHILDREN BEFORE PARENTS (:3347) — collect + reverse for bottom-up stack order; .commits(store) adapter via RevsetIteratorExt (:3387); containing_fn() -> Box<dyn Fn(&CommitId) -> Result<bool,_>> (:3378). String parsing (only if you want user-supplied revsets): parse(&mut RevsetDiagnostics, revset_str, &RevsetParseContext) -> Arc<UserRevsetExpression> (:1375); RevsetParseContext (:3459) needs aliases_map: &RevsetAliasesMap, local_variables: HashMap, user_email: &str, date_pattern_context, default_ignored_remote: Option<&RemoteName>, use_glob_by_default: bool, extensions: &RevsetExtensions (RevsetExtensions::new() :3432), workspace: Option<RevsetWorkspaceContext>; then expr.resolve_user_expression(repo, &SymbolResolver::new(repo, extensions.symbol_resolvers())) (:640, SymbolResolver::new :2847) before evaluate. For gt, skip all of that.

### Index ancestry queries  (/Users/lita/src/jj/lib/src/index.rs:98,116,120,125,139)
```rust
fn has_id(&self, commit_id: &CommitId) -> IndexResult<bool> /* :116 */; fn is_ancestor(&self, ancestor_id: &CommitId, descendant_id: &CommitId) -> IndexResult<bool> /* :120 */; fn common_ancestors(&self, set1: &[CommitId], set2: &[CommitId]) -> IndexResult<Vec<CommitId>> /* :125 */; fn heads(&self, candidates: &mut dyn Iterator<Item = &CommitId>) -> IndexResult<Vec<CommitId>> /* :139 */
```
Reached via repo.index() (Repo trait). gt sync merged-detection: after fetch+track, `let new_main = repo.get_local_bookmark("main".as_ref()).as_normal().unwrap().clone(); if repo.index().is_ancestor(stack_commit_id, &new_main)? { mut_repo.record_abandoned_commit(&commit); }` — catches true-merge/fast-forward landings only; squash merges need the EmptyBehavior::AbandonAllEmpty rebase path instead (which also removes their bookmarks when delete_abandoned_bookmarks=true). dag_walk (lib/src/dag_walk.rs) offers generic topo_order_reverse (:149) / topo_order_forward (:84) / heads (:563) / closest_common_node (:622) if you want raw walks, but revsets + index cover gt's needs.

### Working-copy commit management on MutableRepo  (/Users/lita/src/jj/lib/src/repo.rs:1458,1514,1526)
```rust
pub fn set_wc_commit(&mut self, name: WorkspaceNameBuf, commit_id: CommitId) -> Result<(), RewriteRootCommit> /* :1458 */; pub fn check_out(&mut self, name: WorkspaceNameBuf, commit: &Commit) -> Result<Commit, CheckOutCommitError> /* :1514 */; pub fn edit(&mut self, name: WorkspaceNameBuf, commit: &Commit) -> Result<(), EditCommitError> /* :1526 */
```
check_out = jj-style checkout: writes a NEW empty commit whose parent is `commit` (tree = commit.tree()) and edits that — use for `gt checkout main` (returns the new wc commit). edit = make `commit` itself the wc commit (use after gt create writes the named commit); it auto-abandons the previous wc commit iff discardable (empty + no description + head + unreferenced, maybe_abandon_wc_commit repo.rs:1532). set_wc_commit is the raw view update — no add_head, no abandon; prefer edit/check_out. Workspace name: WorkspaceName::DEFAULT (ref_name.rs:318), owned via .to_owned() -> WorkspaceNameBuf. IMPORTANT for the three-writes story: these only move the in-view pointer — the on-disk working copy is NOT touched; after tx.commit() you must separately check out the tree with the workspace's WorkingCopy (jj-lib workspace API, outside these files).

### Transaction commit semantics  (/Users/lita/src/jj/lib/src/transaction.rs:120,130,136)
```rust
pub fn commit(self, description: impl Into<String>) -> Result<Arc<ReadonlyRepo>, TransactionCommitError> /* :120 */; pub fn write(mut self, description: impl Into<String>) -> Result<UnpublishedOperation, TransactionCommitError> /* :130 */
```
commit = write + publish. write() asserts `!mut_repo.has_rewrites()` with message "BUG: Descendants have not been rebased after the last rewrites." (:136-139) — i.e. rebase_descendants is NOT run for you; forgetting it is a panic, not an error. Entry point: `let mut tx = repo.start_transaction();` (repo.rs:326), mutate via tx.repo_mut() (:94), read via tx.repo() (:90). commit() writes ONLY view+operation to the op store — writes #2 (working-copy files) and #3 (git refs, git::export_refs for colocated) remain the caller's job.

## Minimal flow
// ---- gt create --all -m msg (after snapshotting WC via workspace API, out of scope here) ----
let mut tx = repo.start_transaction();
let r = tx.repo_mut();
let leaf_id: CommitId = r.view().get_wc_commit_id(WorkspaceName::DEFAULT).unwrap().clone();
let wc = r.store().get_commit(&leaf_id)?;                      // current (snapshotted) wc commit
let named = r.rewrite_commit(&wc).set_description(msg).write()?; // name the wc commit
r.set_local_bookmark_target("feat-1".as_ref(), RefTarget::normal(named.id().clone()));
let new_wc = r.new_commit(vec![named.id().clone()], named.tree()).write()?; // empty commit on top
r.edit(WorkspaceName::DEFAULT.to_owned(), &new_wc)?;
r.rebase_descendants()?;                                       // REQUIRED before commit (panics otherwise)
let repo: Arc<ReadonlyRepo> = tx.commit("gt create")?;         // WRITE 1: op log/view only
// WRITE 2: workspace working-copy checkout (locked_working_copy.check_out(&new_wc)...) — separate API
// WRITE 3: git::export_refs(...) to materialize refs/heads/* in the colocated .git — separate API

// ---- gt modify --all (amend snapshot already folded into wc commit by snapshotting) ----
// if description/tree change needed explicitly:
let r = tx.repo_mut();
let wc = r.store().get_commit(r.view().get_wc_commit_id(WorkspaceName::DEFAULT).unwrap())?;
r.rewrite_commit(&wc).set_tree(new_tree).write()?;             // records old->new in parent_mapping
r.rebase_descendants()?;                                       // auto-rebases stack above; bookmarks follow
tx.commit("gt modify")?;

// ---- stack enumeration (gt submit / log): main..leaf ----
let main_id = repo.view().get_local_bookmark("main".as_ref()).as_normal().unwrap().clone();
let stack: Vec<Commit> = RevsetExpression::commits(vec![main_id.clone()])
    .range(&RevsetExpression::commits(vec![leaf_id]))          // main..leaf
    .evaluate(repo.as_ref())?                                  // Box<dyn Revset>
    .iter().commits(repo.store()).try_collect()?;              // children-first; .reverse() for bottom-up
// per bookmark push decision:
let action = refs::classify_bookmark_push_action(LocalAndRemoteRef {
    local_target: &repo.view().get_local_bookmark(name),
    remote_ref: &repo.view().get_remote_bookmark(RemoteRefSymbol { name, remote: "origin".as_ref() }),
});
// after push succeeds:
r.set_remote_bookmark(symbol, RemoteRef { target: RefTarget::normal(id), state: RemoteRefState::Tracked });

// ---- gt sync (after git fetch + import_refs updated main@origin) ----
let r = tx.repo_mut();
r.track_remote_bookmark(RemoteRefSymbol { name: "main".as_ref(), remote: "origin".as_ref() })?; // fold remote into local main
let new_main = r.get_local_bookmark("main".as_ref()).as_normal().unwrap().clone();
// abandon truly-merged commits:
for c in &stack { if r.index().is_ancestor(c.id(), &new_main)? { r.record_abandoned_commit(c); } }
// rebase stack root onto new main; squash-merged commits become empty and are abandoned:
let mut new_parents_map = HashMap::new();
new_parents_map.insert(stack_root_id, vec![new_main.clone()]);
r.transform_descendants_with_options(
    vec![stack_root_id], &new_parents_map,
    &RewriteRefsOptions { delete_abandoned_bookmarks: true },
    async |rewriter| {
        if rewriter.parents_changed() {
            if let Some(b) = rewriter.rebase_with_empty_behavior(EmptyBehavior::AbandonNewlyEmpty).await? { b.write()?; }
        }
        Ok(())
    })?;
r.rebase_descendants()?;   // flush any remaining parent_mapping entries (transform_* does not clear it)
tx.commit("gt sync")?;     // then again: working-copy checkout + git export = writes 2 and 3

## Gotchas
- Transaction::write/commit PANICS (assert, transaction.rs:136-139) if any rewrite/abandon was recorded without a subsequent rebase_descendants()/rebase_descendants_with_options()/reparent_descendants() — rebasing is never implicit at commit time.
- transform_descendants/transform_commits run update_rewritten_references but do NOT clear parent_mapping (repo.rs:1369-1377 comment); only rebase_descendants* clears it — so after a transform_* pass, still call rebase_descendants() (cheap no-op-ish) or tx.commit will panic if entries remain unprocessed... specifically it panics only if has_rewrites(), and transform leaves entries in parent_mapping, so the trailing rebase_descendants() is mandatory.
- There is no MutableRepo::record_rewritten_commit or set_local_bookmark in 0.36 — the real names are set_rewritten_commit (repo.rs:971) and set_local_bookmark_target (repo.rs:1676); repo.rs:970's docstring still mentions the old name.
- rebase_commit, CommitRewriter::rebase, rebase_with_empty_behavior, merge_commit_trees, duplicate_commits are async fns; transform_descendants' callback is AsyncFnMut (edition-2024 async closure). jj-lib drives them with pollster::FutureExt::block_on() — add pollster or an async runtime.
- CommitBuilder::write() fails with BackendError::Other("Newly-created commit ... already exists") if the rewritten commit is byte-identical to an existing one (commit_builder.rs:404-418) — a no-op amend (same tree, description, committer timestamp second) can trip this; check for actual changes before rewriting.
- Bookmarks follow rewritten commits ONLY via update_rewritten_references during a rebase/transform pass in the SAME transaction chain; RefTarget is a Merge and bookmark updates use 3-way merge semantics (merge_local_bookmark), so concurrent moves produce conflicted targets — always check RefTarget::has_conflict()/as_normal() before using a bookmark id for push.
- Revset::iter() yields children BEFORE parents (revset.rs:3346-3347); a Graphite stack bottom-up needs the collected list reversed.
- index().is_ancestor detects merge/fast-forward landings only; GitHub squash-merges leave the original commits non-ancestral — handle them with EmptyBehavior::AbandonNewlyEmpty/AbandonAllEmpty during the sync rebase (rewrite.rs:410-422) plus RewriteRefsOptions { delete_abandoned_bookmarks: true } (rewrite.rs:441-447) to also delete their bookmarks; default (false) moves the bookmark to the parent instead, which would wrongly leave a stale stack bookmark on main.
- MutableRepo::check_out creates a NEW empty working-copy commit on top of the target (jj semantics, repo.rs:1514-1524); edit() makes the target itself the wc commit and silently abandons the previous wc commit if discardable (empty+no description). new_commit asserts parents is non-empty.
- Everything on MutableRepo mutates only the in-memory view: tx.commit() persists op-store/view (write #1). The on-disk working copy (write #2, workspace/LockedWorkingCopy::check_out) and colocated git refs (write #3, jj_lib::git::export_refs / GitBackend) must be updated explicitly afterward — none of the APIs in these files touch them.
- MutableRepo implements the Repo trait, so revset .evaluate(mut_repo), index(), store(), view() all work mid-transaction; UserSettings must have user name/email configured or commits get placeholder signatures (for_rewrite_from patches placeholder authors, commit_builder.rs:238-247).


---

# impulse-usage

## Summary

Impulse (Tauri app, jj-lib 0.40 from crates.io) wraps all jj operations in a `Jj` trait (src-tauri/src/jj.rs, 9633 lines) — the single most useful reference for gt. Its universal per-command wrapper is: per-workspace tokio Mutex -> tokio::task::spawn_blocking -> build minimal UserSettings from StackedConfig -> DefaultWorkspaceLoaderFactory.create(root).load(...) -> pollster::block_on(async { repo_loader().load_at_head() -> repo.start_transaction() -> mutate MutableRepo -> repo_mut().rebase_descendants() -> jj_lib::git::export_refs(tx.repo_mut()) [WRITE: git refs, INSIDE the tx before commit] -> tx.commit("msg") [WRITE: op log] -> workspace.check_out(new_op.operation().id(), None, &wc_commit) or workspace.start_working_copy_mutation()?.finish(op_id) [WRITE: working-copy files + state] }). It also performs the CLI-implicit WRITE ZERO — snapshot_workspace_blocking() — at the start of any command that reads @, which itself is a full transaction (locked_wc().snapshot -> rewrite_commit(set_tree) -> rebase_descendants -> tx.commit -> locked_ws.finish(new_op_id)). This maps 1:1 onto the talk's three-writes story, and impulse's own comments say so explicitly ("This is what jj's CLI does implicitly at the start of every command", "successive transactions don't update the working copy"). Stack detection is done TWO ways: commit-graph side via collect_chain (post-order walk from branch tip down to first ancestor of onto, using index().is_ancestor) for previews/rebases, and PR-metadata side (GitHub baseRefName->headRefName edges from its SQLite mirror) for the merged-parent -> rebase-children -> force-push-substack sync engine in stack.rs. GitHub is pure GraphQL v4 (POST https://api.github.com/graphql, Bearer token from a device-flow OAuth app, token in OS keyring): createPullRequest mutation with {repositoryId, baseRefName, headRefName, title, body, draft}; stacked base = parent PR's headRefName, falling back to project default branch; updatePullRequest only for title/body (impulse never retargets base — it relies on rebase+force-push and GitHub auto-retarget after branch delete). Fetch/push go through jj-lib's git subprocess machinery with a temp GIT_ASKPASS script injecting the OAuth token (x-access-token / token). All jj-lib 0.40 calls it makes are async and driven by pollster::block_on inside spawn_blocking; in our 0.36 reference nearly all of those same calls are sync (exceptions: LockedWorkingCopy::snapshot/check_out and CommitRewriter::rebase are async in 0.36 too), so gt's code will be simpler than impulse's.

## Key APIs

### impulse: minimal UserSettings construction (Q1)  (/Users/lita/src/issues/src-tauri/src/jj.rs:851)
```rust
fn user_settings_with_overrides(force_git_markers: bool) -> Result<UserSettings>
```
let mut config = StackedConfig::with_defaults(); let mut defaults = ConfigLayer::empty(ConfigSource::Default); defaults.set_value("user.name", ...)?; set user.email, operation.hostname, operation.username; config.add_layer(defaults); optionally config.load_file(ConfigSource::User, ~/.jjconfig.toml); optionally top ConfigLayer::empty(ConfigSource::CommandArg) with ui.conflict-marker-style="git"; UserSettings::from_config(config). operation.hostname/username are REQUIRED for op-log entries. StackedConfig::with_defaults() supplies git.executable-path etc. needed by GitSettings::from_settings. Same API exists in 0.36 (jj_lib::config::{ConfigLayer, ConfigSource, StackedConfig}, jj_lib::settings::UserSettings).

### impulse: workspace load boilerplate (Q1)  (/Users/lita/src/issues/src-tauri/src/jj.rs:914-924)
```rust
let loader = DefaultWorkspaceLoaderFactory.create(&workspace_root)?; let workspace = loader.load(&settings, &StoreFactories::default(), &default_working_copy_factories())?;
```
Imports: jj_lib::workspace::{DefaultWorkspaceLoaderFactory, Workspace, WorkspaceLoaderFactory, default_working_copy_factories, default_working_copy_factory}; jj_lib::repo::StoreFactories. Identical in 0.36. loader.repo_path() resolves .jj/repo without opening the repo (cheap identity checks, jj.rs:1370).

### impulse: snapshot working copy = jj CLI's implicit write-at-command-start (Q2, talk gold)  (/Users/lita/src/issues/src-tauri/src/jj.rs:3839-3953)
```rust
fn snapshot_workspace_blocking(workspace_root: &Path) -> Result<()>
```
Recipe: load repo AT THE WORKING COPY'S OPERATION, not head (wc_op_id = workspace.working_copy().operation_id(); load_operation + load_at) 'so its old_tree comparison is valid'; get wc commit via view().get_wc_commit_id(&workspace_name); locked_ws = workspace.start_working_copy_mutation()?; (new_tree,_) = locked_ws.locked_wc().snapshot(&SnapshotOptions{ base_ignores: GitIgnoreFile::empty(), progress: None, start_tracking_matcher: &EverythingMatcher, force_tracking_matcher: &NothingMatcher, max_new_file_size: 64*1024*1024 }).await?; if new_tree.tree_ids_and_labels().0 == wc_commit.tree_ids() { locked_ws.finish(wc_op_id) /* persists stat-cache + releases lock via documented API not Drop */ } else { tx = repo.start_transaction(); tx.repo_mut().rewrite_commit(&wc_commit).set_tree(new_tree).write()?; tx.repo_mut().rebase_descendants()?; new_op = tx.commit("snapshot working copy")?; locked_ws.finish(new_op.operation().id().clone()) }. 0.36 diff: MergedTree::tree_ids_and_labels exists; Commit::tree_ids() -> &Merge<TreeId> (commit.rs:132); CommitBuilder::write is sync; snapshot is still async (working_copy.rs:118) so pollster still needed for that one call.

### impulse: import git refs into jj view (Q2/Q3)  (/Users/lita/src/issues/src-tauri/src/jj.rs:904-958 and 3736-3750)
```rust
let stats = jj_lib::git::import_refs(tx.repo_mut(), &GitImportOptions{ auto_local_bookmark: bool, abandon_unreachable_commits: false, remote_auto_track_bookmarks: Default::default() }).await?; tx.commit("import refs").await?;
```
Load at HEAD op, not wc op — comment: 'successive transactions don't update the working copy, so using working_copy().operation_id() would replay ancient state'. auto_local_bookmark:true for initial setup (every origin ref gets a local bookmark), false after fetch. 0.36: pub fn import_refs(mut_repo: &mut MutableRepo, options: &GitImportOptions) -> Result<GitImportStats, GitImportError> — SYNC (git.rs:473); GitImportOptions has the same three fields (git.rs:427). GitImportOptions holds a non-Send matcher closure — build it INSIDE spawn_blocking (jj.rs:900-903).

### impulse: git fetch via jj-lib (Q3)  (/Users/lita/src/issues/src-tauri/src/jj.rs:3615-3664)
```rust
let mut fetch = jj_lib::git::GitFetch::new(tx.repo_mut(), subprocess_options, &import_options)?; fetch.fetch(RemoteName::new("origin"), expand_fetch_refspecs(remote, GitFetchRefExpression{ bookmark: StringExpression::all(), tag: StringExpression::none() })?, &mut callback, None, None)?; let stats = fetch.import_refs().await?; if imported>0 { tx.commit("fetch origin").await?; }
```
0.40 shape. 0.36 (git.rs:2578,2602,2694,2300): GitFetch::new(mut_repo: &'a mut MutableRepo, git_settings: &'a GitSettings, import_options: &'a GitImportOptions) -> Result<Self, UnexpectedGitBackendError>; fetch(&mut self, remote_name: &RemoteName, ExpandedFetchRefSpecs{..}: ExpandedFetchRefSpecs, callbacks: RemoteCallbacks, depth: Option<NonZeroU32>, fetch_tags_override: Option<FetchTagsOverride>) -> Result<(), GitFetchError>; expand_fetch_refspecs(remote: &RemoteName, bookmark_expr: StringExpression) -> Result<ExpandedFetchRefSpecs, GitRefExpansionError>; import_refs(&mut self) -> Result<GitImportStats, GitImportError> SYNC. GitSettings::from_settings(&UserSettings) (git.rs:95). RemoteCallbacks<'a> is a plain struct of Option<&mut dyn FnMut> — RemoteCallbacks::default() works (git.rs:2842). Note: even a pure fetch ends in tx.commit — a fetch IS an op-log write.

### impulse: git push via jj-lib with compare-and-swap (Q4)  (/Users/lita/src/issues/src-tauri/src/jj.rs:3666-3734)
```rust
for each branch: new_target = view().get_local_bookmark(name).as_normal(); old_target = view().get_remote_bookmark(name.to_remote_symbol("origin".as_ref())).tracked_target().as_normal(); then jj_lib::git::push_refs(tx.repo_mut(), subprocess_options, "origin".as_ref(), &GitPushRefTargets{bookmarks: vec![(name, Diff::new(old, Some(new)))]}, &mut callback, &GitPushOptions::default())?; if !stats.all_ok() bail; tx.commit("push bookmarks").await?;
```
old_target is the CAS expectation (safe force-push). Push mutates the view's remote-tracking bookmark on success, so tx.commit afterwards is mandatory — a push is ALSO an op-log write. 0.36 equivalent (git.rs:2744): pub fn push_branches(mut_repo: &mut MutableRepo, git_settings: &GitSettings, remote: &RemoteName, targets: &GitBranchPushTargets, callbacks: RemoteCallbacks) -> Result<GitPushStats, GitPushError>; GitBranchPushTargets{ branch_updates: Vec<(RefNameBuf, BookmarkPushUpdate{old_target: Option<CommitId>, new_target: Option<CommitId>})> } (git.rs:2729); it updates view + refs/remotes/origin/* git refs itself when push_stats.all_ok().

### impulse: git subprocess auth via GIT_ASKPASS token script (Q4)  (/Users/lita/src/issues/src-tauri/src/jj.rs:3780-3828)
```rust
fn git_subprocess_options(settings: &UserSettings, oauth_token: Option<&str>) -> Result<(jj_lib::git::GitSubprocessOptions, Option<tempfile::TempDir>)>
```
0.40: GitSubprocessOptions::from_settings(settings), then options.environment.insert(GIT_TERMINAL_PROMPT=0, GIT_ASKPASS=<temp sh script>, ISSUES_GIT_TOKEN=<token>); script prints 'x-access-token' for Username prompts and the token otherwise. 0.36 HAS NO GitSubprocessOptions/environment field — the git subprocess inherits the parent process env verbatim (only sets LC_ALL=C, /Users/lita/src/jj/lib/src/git_subprocess.rs:135). For gt on 0.36: set GIT_ASKPASS via std::env::set_var on your own process before fetch/push, or rely on the user's normal git credential helper (fine for a demo).

### impulse: init colocated jj on existing git repo (gt init)  (/Users/lita/src/issues/src-tauri/src/jj.rs:982-1025 and 3965-3984)
```rust
Workspace::init_external_git(&settings, &workspace_root, &git_repo_path).await // 0.36: pub fn init_external_git(user_settings: &UserSettings, workspace_root: &Path, git_repo_path: &Path) -> Result<(Self, Arc<ReadonlyRepo>), WorkspaceInitError> (workspace.rs:248, SYNC)
```
git_repo_path = <root>/.git (resolve 'gitdir:' pointer files for worktrees — resolve_git_repo_path jj.rs:3965). CRITICAL comment (stack.rs:137-140): 'Workspace::init_external_git only initialises the jj store against the existing .git; it does NOT import refs' — you must run git::import_refs in a follow-up transaction or main@origin etc. are invisible. is_colocated check = .jj is_dir && .git exists (jj.rs:976).

### impulse: checkout a workspace onto a ref (gt checkout main) — full three-writes exemplar  (/Users/lita/src/issues/src-tauri/src/jj.rs:1234-1309)
```rust
let repo = workspace.repo_loader().load_at_head().await?; let mut tx = repo.start_transaction(); let base_id = resolve(...); let base_commit = tx.repo_mut().store().get_commit(&base_id)?; let wc_commit = tx.repo_mut().check_out(workspace_name.clone(), &base_commit).await?; tx.repo_mut().rebase_descendants().await?; jj_lib::git::export_refs(tx.repo_mut())?; let new_op = tx.commit("checkout ...").await?; workspace.check_out(new_op.operation().id().clone(), None, &wc_commit)?;
```
This is the cleanest per-command shape: mutate -> rebase_descendants -> export_refs -> tx.commit -> workspace.check_out. 0.36 signatures: MutableRepo::check_out(&mut self, name: WorkspaceNameBuf, commit: &Commit) -> Result<Commit, CheckOutCommitError> SYNC (repo.rs:1514); rebase_descendants(&mut self) -> BackendResult<usize> SYNC (repo.rs:1428); export_refs(mut_repo: &mut MutableRepo) -> Result<GitExportStats, GitExportError> (git.rs:967); Transaction::commit(self, description: impl Into<String>) -> Result<Arc<ReadonlyRepo>, TransactionCommitError> SYNC (transaction.rs:120) — the returned Arc<ReadonlyRepo> has .operation(); Workspace::check_out(&mut self, operation_id: OperationId, old_tree: Option<&MergedTree>, commit: &Commit) -> Result<CheckoutStats, CheckoutError> SYNC (workspace.rs:428). Passing None as old_tree skips the ConcurrentCheckout guard.

### impulse: set/create bookmark at working copy + advance wc op WITHOUT touching files (gt create's bookmark half)  (/Users/lita/src/issues/src-tauri/src/jj.rs:4647-4724)
```rust
async fn ensure_bookmark_at_wc_inner_with_options(mut workspace: Workspace, name: &str, force_move: bool) -> Result<String>
```
Pattern: snapshot first (caller), load_at_head, tx.repo_mut().set_local_bookmark_target(RefName::new(name), RefTarget::normal(target_id)); git::export_refs; new_op = tx.commit(...); then workspace.start_working_copy_mutation()?.finish(new_op.operation().id().clone()) — this THIRD write advances the wc's recorded operation so 'jj log in that worktree sees the bookmark move immediately' without checking out files. Target = closest_non_empty_wc_ancestor (jj.rs:4726): walk first parents skipping commits where commit.is_empty(repo) — never point a bookmark at jj's empty placeholder @ ('pushing it would blank out the head bookmark on origin'). 0.36: set_local_bookmark_target at repo.rs:1676, LockedWorkspace::finish(self, operation_id: OperationId) -> Result<(), WorkingCopyStateError> SYNC (workspace.rs:466), Commit::is_empty(&self, repo: &dyn Repo) -> BackendResult<bool> SYNC (commit.rs:155).

### impulse: describe/amend a commit (gt modify's describe half)  (/Users/lita/src/issues/src-tauri/src/jj.rs:4522-4593)
```rust
let new_commit = tx.repo_mut().rewrite_commit(&old_commit).set_description(new_description).write().await?; tx.repo_mut().rebase_descendants().await?; jj_lib::git::export_refs(tx.repo_mut())?; tx.commit(...).await?;
```
Resolves id as ChangeId::try_from_reverse_hex first (then repo.resolve_change_id(&change_id) -> resolved.visible_with_offsets().next()), falling back to CommitId::try_from_hex. Comment: bookmarks pointing at the old oid move automatically during export_refs 'via the rewrites map'. gt modify --all = snapshot_workspace_blocking (tree amend) alone, or squash @ into parent via rewrite_commit(set_tree)+abandon; impulse only amends @ in place via snapshot. 0.36: rewrite_commit(&mut self, predecessor: &Commit) -> CommitBuilder<'_> (repo.rs:954); CommitBuilder::write(self) -> BackendResult<Commit> SYNC (commit_builder.rs:163); resolve_change_id on Repo trait (repo.rs:131).

### impulse: rebase a branch onto a target (gt sync's rebase engine)  (/Users/lita/src/issues/src-tauri/src/jj.rs:2481-2632)
```rust
async fn rebase_branch_inner(workspace: Workspace, workspace_root: &Path, branch_name: &str, onto_name: &str) -> Result<RebaseOutcome>
```
Algorithm: resolve branch (local->git->origin) and onto; if equal -> no-op; if index.is_ancestor(&branch_id,&onto_id) -> fast-forward = just set_local_bookmark_target + export_refs + commit; else chain = collect_chain(tx.repo_mut(), &branch_id, &onto_id); walk oldest-first, rewriter = CommitRewriter::new(tx.repo_mut(), old_commit, new_parents); builder = rewriter.rebase().await?; new_commit = builder.write().await?; track id_map old->new; conflicts detected via new_commit.has_conflict() (conflicts MATERIALIZE into commits, never abort); then rebase_descendants() ('jj-lib requires any rewritten commit's descendants are also rebased before the transaction commits'); final tip re-read via tx.repo_mut().new_parents(&[last_new_id]) because rebase_descendants may rewrite again; set bookmark; export_refs; commit. 0.36: CommitRewriter::new same, .rebase() IS async (rewrite.rs:335), new_parents(&self, old_ids: &[CommitId]) -> Vec<CommitId> (repo.rs:1041), index().is_ancestor exists.

### impulse: commit-graph stack collection  (/Users/lita/src/issues/src-tauri/src/jj.rs:3556-3612)
```rust
fn collect_chain(mut_repo: &jj_lib::repo::MutableRepo, branch: &CommitId, onto: &CommitId) -> Result<Vec<CommitId>>
```
Iterative post-order DFS from branch tip; prune any commit where index.is_ancestor(&id, onto) (already in onto's history); returns commits to rebase oldest-first, parents always before children even with merges. This + preview_stack_inner (jj.rs:4040: chain plus @ appended if @'s parent is on the chain and @ isn't empty+undescribed) is gt's stack model on the jj side.

### impulse: sync engine (gt sync) — fetch, detect newly merged, rebase children, push substack  (/Users/lita/src/issues/src-tauri/src/stack.rs:127-665)
```rust
pub async fn sync_project(db, seed_cache_dir, github: &GraphQLClient, jj: &dyn Jj, project: &Project, skip_network_fetch: bool, ...) -> Result<SyncOutcome>
```
Order: (1) jj.fetch_origin(local_path, token) [jj-lib GitFetch + import + op commit]; (2) refresh PR metadata from GitHub GraphQL into SQLite; (3) newly_merged = after.merged && !before.merged; (4) PR stack graph from metadata: by_base: HashMap<baseRefName, Vec<PR>> over OPEN PRs; direct children of merged PR's headRefName get jj.rebase_branch_onto(local_path, child_head, default_branch) — ONLY the direct child ('jj moves descendants; rebasing them again would flatten the stack'); (5) push set = collect_substack = child + transitive descendants via headRefName->baseRefName edges, filtered to branches that exist as LOCAL jj bookmarks (remote-only PR branches are skipped with a 'jj bookmark track' hint); (6) jj.push_bookmarks(local_path, refs, token) force-pushes all of them in one call. Conflicted rebases are reported (RebaseFailure + jj conflict_state), not rolled back. Base retargeting of the child PR is left to GitHub (no updatePullRequest baseRefName call anywhere).

### impulse: GitHub PR create (gt submit)  (/Users/lita/src/issues/src-tauri/src/github.rs:3405-3463 and 4648-4660)
```rust
async fn create_pull_request(&self, repo_node_id: &str, base_ref_name: &str, head_ref_name: &str, title: &str, body: &str, draft: bool) -> Result<CreatedPullRequest> // GraphQL: mutation CreatePullRequest($input: CreatePullRequestInput!) { createPullRequest(input: $input) { pullRequest { id number url headRefName baseRefName } } } with input {repositoryId, baseRefName, headRefName, title, body, draft}
```
Transport: POST https://api.github.com/graphql, headers Authorization: Bearer <token>, User-Agent (github.rs:1848-1858). Token from GitHub OAuth device flow (oauth.rs: client_id Ov23liNvK4nZj9gsOLHM, scope 'repo user read:project read:org', endpoints github.com/login/device/code + /login/oauth/access_token, stored in OS keyring). updatePullRequest (github.rs:3781, mutation at 5013) sends only {pullRequestId, title?, body?} — UpdatePullRequestInput does accept baseRefName if gt wants explicit retargeting, impulse just doesn't use it. Docs list common createPullRequest failures (lib.rs:8449-8455): head ref missing on remote, head==base, PR already exists, draft disabled, missing repo scope.

### impulse: end-to-end stacked-PR create command (Q4 flow)  (/Users/lita/src/issues/src-tauri/src/lib.rs:8210-8523)
```rust
async fn create_stack_pull_request(project_id, parent_pr_id: Option<String>, base_ref_name: Option<String>, title, body, head_ref_name, draft, ...)
```
Base resolution precedence: explicit base_ref_name > parent PR's headRefName (from cached PR JSON) > repo default branch. Then: normalize_branch to lowercase ('jj is case-insensitive on bookmark names, GitHub is not', lib.rs:8300); jj.ensure_bookmark_at_working_copy(local_path, head) ; jj.push_bookmarks(local_path, [head], token); pre-flight verify BOTH refs exist on origin via get_ref_oid and head != base (avoids GitHub's opaque 'Head sha can't be blank / No commits between' errors); github.create_pull_request(repo_node_id, base, head, title, body, draft); post-create incremental sync.

### impulse: fetch a PR head that jj can't (refs/pull/N/head)  (/Users/lita/src/issues/src-tauri/src/jj.rs:4397-4420 and 2222-2254)
```rust
fn fetch_pull_request_head_blocking(workspace_root: &Path, number: i64, oauth_token: Option<&str>) -> Result<String>
```
Uses raw git2 with refspec '+refs/pull/{n}/head:refs/heads/{bookmark}' because 'jj_lib::git::expand_fetch_refspecs only builds refspecs for refs/heads/refs/tags patterns, and PR heads live under refs/pull'. MUST be followed by import_git_refs or 'the bookmark is invisible to revset and ancestry queries even though the objects are in the odb' — and that follow-up call is made OUTSIDE the workspace lock because the lock is non-reentrant (deadlock comment jj.rs:2247-2250).

### impulse: concurrency/async harness (Q5)  (/Users/lita/src/issues/src-tauri/src/jj.rs:101-123, 802-839; Cargo.toml:42-43)
```rust
async fn instrument_locked<F,T,Fut>(command: &str, details: String, lock: Arc<TokioMutex<()>>, body: F) -> Result<T>; workspace_locks: Arc<StdMutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>>
```
Every trait method: acquire per-workspace tokio Mutex (key = canonicalized root) -> tokio::task::spawn_blocking(move || { sync setup; pollster::block_on(async { ...jj-lib 0.40 async calls... }) }). 115 pollster/spawn_blocking sites. Tauri commands run on tauri::async_runtime (tokio); jj-lib work never blocks the async threads. Async-in-0.40 (all pollster'd): load_at_head, load_operation/load_at, tx.commit, MutableRepo::check_out, rebase_descendants, CommitBuilder::write, CommitRewriter::rebase, git::import_refs, GitFetch::import_refs, Workspace::init_external_git, init_workspace_with_existing_repo, Workspace::check_out, LockedWorkspace::finish, locked_wc().snapshot, Commit::is_empty. In 0.36 ONLY locked_wc().snapshot()/check_out()/reset() (working_copy.rs:118+) and CommitRewriter::rebase/rebase_commit (rewrite.rs:335,145) are async — everything else is sync, and Workspace::check_out / LockedWorkspace::finish internally .block_on() for you. gt on 0.36 needs pollster (or futures::executor) only around snapshot and CommitRewriter::rebase.

### jj-lib 0.36: transaction anatomy (write #1, op log)  (/Users/lita/src/jj/lib/src/repo.rs:326 and /Users/lita/src/jj/lib/src/transaction.rs:120)
```rust
pub fn start_transaction(self: &Arc<Self>) -> Transaction  //  pub fn commit(self, description: impl Into<String>) -> Result<Arc<ReadonlyRepo>, TransactionCommitError>
```
0.36 start_transaction takes NO settings arg (settings live in RepoLoader). commit = write + publish; returns the new ReadonlyRepo whose .operation().id() feeds the working-copy write. RepoLoader: load_at_head() repo.rs:756, load_at(&Operation) repo.rs:767, load_operation(&OperationId) repo.rs:798 — all sync. Dropping a Transaction without commit discards it (impulse uses this for read-only 'transactions' to get MutableRepo APIs, jj.rs:4052).

### jj-lib 0.36: SnapshotOptions  (/Users/lita/src/jj/lib/src/working_copy.rs:212-232)
```rust
pub struct SnapshotOptions<'a> { pub base_ignores: Arc<GitIgnoreFile>, ... pub start_tracking_matcher: &'a dyn Matcher, pub force_tracking_matcher: &'a dyn Matcher, pub max_new_file_size: u64 }
```
Same fields impulse fills (base_ignores: GitIgnoreFile::empty(), progress: None, start_tracking_matcher: &EverythingMatcher, force_tracking_matcher: &NothingMatcher, max_new_file_size: 64MiB). LockedWorkingCopy::snapshot(&mut self, options) -> (MergedTree, SnapshotStats) is an async trait method in 0.36.

### impulse: bookmark deletion (gt sync cleanup of merged branches)  (/Users/lita/src/issues/src-tauri/src/jj.rs:4814-4865)
```rust
fn delete_local_bookmarks_blocking(workspace_root: &Path, names: &[String]) -> Result<usize>
```
set_local_bookmark_target(ref_name, RefTarget::absent()) per name (skip already-absent for idempotence), then export_refs + tx.commit. RefTarget::absent() is jj's 'deleted' sentinel; export removes refs/heads/<name> from the colocated .git.

### impulse: ref resolution across local/git/origin (colocated model)  (/Users/lita/src/issues/src-tauri/src/jj.rs:2641-2662 and 2954-2995)
```rust
fn resolve_local_or_origin(repo: &dyn jj_lib::repo::Repo, name: &RefName, label: &str) -> Result<CommitId>
```
Try view.get_local_bookmark(name).as_normal(), then remote bookmarks name.to_remote_symbol(RemoteName::new(r)) for r in ["git","origin"]. 'jj treats local git branches as remote tracking from the special "git" remote' in colocated repos. Sibling resolve_origin_or_local prefers origin first (for freshly-fetched bases); resolve_origin_or_local_or_commit falls back to CommitId::try_from_hex.

## Minimal flow
// gt command skeleton on jj-lib 0.36, distilled from impulse's wrapper (sync where 0.36 allows)
// ---- once per process ----
let mut cfg = StackedConfig::with_defaults();
let mut layer = ConfigLayer::empty(ConfigSource::Default);
layer.set_value("user.name", "gt demo")?; layer.set_value("user.email", "gt@example.com")?;
layer.set_value("operation.hostname", "gt")?; layer.set_value("operation.username", "gt")?;   // REQUIRED
cfg.add_layer(layer);
let settings = UserSettings::from_config(cfg)?;

// ---- gt init (once per repo) ----
let (workspace, repo) = Workspace::init_external_git(&settings, root, &root.join(".git"))?;   // does NOT import refs!
let mut tx = repo.start_transaction();
git::import_refs(tx.repo_mut(), &GitImportOptions{ auto_local_bookmark: false, abandon_unreachable_commits: false, remote_auto_track_bookmarks: Default::default() })?;
tx.commit("gt: import refs")?;

// ---- every other command starts here ----
let loader = DefaultWorkspaceLoaderFactory.create(root)?;
let mut ws = loader.load(&settings, &StoreFactories::default(), &default_working_copy_factories())?;
let ws_name = ws.workspace_name().to_owned();

// WRITE 0 — snapshot working-copy files into @ (what `jj` CLI does implicitly):
let wc_op_id = ws.working_copy().operation_id().clone();
let op = ws.repo_loader().load_operation(&wc_op_id)?;                 // wc's op, NOT head — old_tree must match
let repo = ws.repo_loader().load_at(&op)?;
let wc_commit = repo.store().get_commit(repo.view().get_wc_commit_id(&ws_name).unwrap())?;
let mut locked = ws.start_working_copy_mutation()?;
let (new_tree, _) = pollster::block_on(locked.locked_wc().snapshot(&SnapshotOptions{
    base_ignores: GitIgnoreFile::empty(), progress: None,
    start_tracking_matcher: &EverythingMatcher, force_tracking_matcher: &NothingMatcher,
    max_new_file_size: 64 * 1024 * 1024 }))?;
if new_tree.tree_ids_and_labels().0 == *wc_commit.tree_ids() {
    locked.finish(wc_op_id)?;                                          // persist stat-cache, release lock
} else {
    let mut tx = repo.start_transaction();
    tx.repo_mut().rewrite_commit(&wc_commit).set_tree(new_tree).write()?;
    tx.repo_mut().rebase_descendants()?;
    let new_repo = tx.commit("gt: snapshot working copy")?;
    locked.finish(new_repo.operation().id().clone())?;
}
let mut ws = loader.load(&settings, &StoreFactories::default(), &default_working_copy_factories())?;  // reload: snapshot moved @

// ---- the command's real mutation (example: gt create --all -m msg) ----
let repo = ws.repo_loader().load_at_head()?;                           // HEAD op, not wc op
let mut tx = repo.start_transaction();
let wc_id = tx.repo_mut().view().get_wc_commit_id(&ws_name).unwrap().clone();
let wc = tx.repo_mut().store().get_commit(&wc_id)?;
tx.repo_mut().rewrite_commit(&wc).set_description(msg).write()?;      // name the snapshot commit
tx.repo_mut().rebase_descendants()?;
let named_id = tx.repo_mut().view().get_wc_commit_id(&ws_name).unwrap().clone(); // re-read: descendants pass rewrote it
let base = tx.repo_mut().store().get_commit(&named_id)?;
let new_wc = tx.repo_mut().check_out(ws_name.clone(), &base)?;        // fresh empty @ on top
tx.repo_mut().set_local_bookmark_target(RefName::new(&branch), RefTarget::normal(named_id));
tx.repo_mut().rebase_descendants()?;

git::export_refs(tx.repo_mut())?;                                     // WRITE 2 — colocated .git refs (INSIDE tx)
let new_repo = tx.commit(format!("gt create {branch}"))?;             // WRITE 1 — op log
ws.check_out(new_repo.operation().id().clone(), None, &new_wc)?;      // WRITE 3 — files + wc state
// (bookmark-only commands skip file churn: ws.start_working_copy_mutation()?.finish(new_op_id) instead)

// ---- gt submit (per bookmark, bottom of stack up) ----
// jj side: push branch with CAS
let mut tx = repo.start_transaction();
let new_t = tx.repo_mut().view().get_local_bookmark(name).as_normal().cloned().unwrap();
let old_t = tx.repo_mut().view().get_remote_bookmark(name.to_remote_symbol("origin".as_ref())).tracked_target().as_normal().cloned();
git::push_branches(tx.repo_mut(), &GitSettings::from_settings(&settings)?, "origin".as_ref(),
    &GitBranchPushTargets{ branch_updates: vec![(name.to_owned(), BookmarkPushUpdate{ old_target: old_t, new_target: Some(new_t) })] },
    RemoteCallbacks::default())?;
tx.commit("gt: push")?;                                                // push is an op-log write too
// GitHub side: POST https://api.github.com/graphql, Bearer <token>
// createPullRequest(input:{repositoryId, baseRefName: parent_bookmark_or_main, headRefName: bookmark, title, body, draft})
// updatePullRequest(input:{pullRequestId, baseRefName?, body?}) for restacks / stack-comment updates

// ---- gt sync ----
// 1) fetch: tx; GitFetch::new(tx.repo_mut(), &git_settings, &import_opts)?; f.fetch("origin".as_ref(), expand_fetch_refspecs(remote, StringExpression::all())?, RemoteCallbacks::default(), None, None)?; f.import_refs()?; tx.commit("gt: fetch")?
// 2) for each stack branch bottom-up: if index.is_ancestor(branch, main@origin) => merged: delete bookmark (RefTarget::absent);
//    else chain = collect_chain(branch_tip, new_main); rewrite oldest-first with CommitRewriter::new(...).rebase().await/.write();
//    set_local_bookmark_target(branch, final_tip); rebase_descendants(); export_refs; tx.commit; ws.check_out(op, None, &new_wc)

## Gotchas
- THE THREE-WRITES EVIDENCE IS EXPLICIT IN IMPULSE'S COMMENTS: jj.rs:3830-3835 'This is what jj's CLI does implicitly at the start of every command — without it our calls see a stale @ tree'; jj.rs:927-930 'successive transactions don't update the working copy, so using working_copy().operation_id() would replay ancient state'; jj.rs:4643-4646 'git::export_refs runs at the end so the colocated .git mirrors the new bookmark... the workspace operation is advanced so jj log in that worktree sees the bookmark move immediately'. Every mutating function repeats the same 4-beat coda: rebase_descendants -> export_refs -> tx.commit -> workspace.check_out/finish.
- Version skew 0.40 (impulse) vs 0.36 (gt): in 0.36 load_at_head/tx.commit/MutableRepo::check_out/rebase_descendants/CommitBuilder::write/git::import_refs/GitFetch::import_refs/Workspace::init_external_git/Workspace::check_out/LockedWorkspace::finish/Commit::is_empty are all SYNC (no .await); still async in 0.36: LockedWorkingCopy::snapshot/check_out/reset and CommitRewriter::rebase/rebase_commit. 0.40-only APIs impulse uses that DO NOT exist in 0.36: jj_lib::workspace_store::SimpleWorkspaceStore, GitSubprocessOptions (with .environment), GitSubprocessCallback trait, git::push_refs/GitPushRefTargets, GitFetchRefExpression, jj_lib::merge::Diff for push targets. 0.36 equivalents: view().wc_commit_ids(), GitSettings + inherited process env, RemoteCallbacks struct, git::push_branches/GitBranchPushTargets/BookmarkPushUpdate, bare StringExpression to expand_fetch_refspecs.
- Two different load points, both load-bearing: snapshot must load the repo AT THE WORKING COPY'S OPERATION (load_operation + load_at) so locked_wc's old_tree comparison is valid, while every other mutation must load AT HEAD or it silently operates on ancient state (bookmarks from recent fetches missing). Getting either wrong 'replays ancient state'.
- Workspace::init_external_git does NOT import git refs — a freshly colocated repo has an empty jj view until you run git::import_refs in your own transaction (stack.rs:135-140 comment; sync_project always runs an import step even when skipping the network fetch). gt init must do init_external_git + import_refs + tx.commit.
- After rebase_descendants(), ids you wrote earlier in the SAME transaction may have been rewritten again — re-read the final tip via repo_mut().new_parents(&[id]) (jj.rs:2607-2614) or re-read @ via view().get_wc_commit_id (jj.rs:2907-2921 'rebase_descendants is what repoints the workspace's @ at the rewritten commit, so read the final id back from the view rather than assuming'). jj-lib also requires all descendants of rewritten commits be rebased before tx.commit.
- Fetch and push are THEMSELVES op-log transactions: GitFetch/push mutate the MutableRepo view (remote-tracking bookmarks), so each ends with tx.commit — impulse even skips the commit when a fetch imported 0 refs, by just dropping the tx (jj.rs:3656-3658). Dropping an uncommitted Transaction is the sanctioned way to get read-only MutableRepo APIs too (jj.rs:4052-4054).
- jj's empty working-copy placeholder commit is a footgun for push/PR flows: bookmark the closest NON-empty ancestor of @ (walk first parents while commit.is_empty(repo), jj.rs:4726), or the pushed branch points at an empty commit and GitHub rejects the PR with 'Head sha can't be blank / No commits between'. impulse also pre-flights that head and base exist on origin and head != base before calling createPullRequest, because GitHub's error bundle for those cases is useless (lib.rs:8379-8430).
- Colocated ref model: plain git branches (refs/heads/*) appear in jj as remote-tracking bookmarks on the pseudo-remote 'git', NOT as local bookmarks. Resolution helpers must try local -> 'git' -> 'origin' (jj.rs:2634-2662). Also 'jj is case-insensitive on bookmark names, GitHub is not' — impulse lowercases branch names before push/mutation (lib.rs:8300-8317).
- Conflicts never abort: rebased commits carry conflicts (new_commit.has_conflict()), and jj's default materialized markers are diff3+ style that 'most editors can't render', so impulse forces ui.conflict-marker-style=git via a highest-priority ConfigSource::CommandArg layer whenever a transaction might materialize conflicts (jj.rs:845-888).
- Auth for jj-lib fetch/push: jj shells out to the git binary. 0.40 lets you inject env per-call (GitSubprocessOptions.environment: GIT_TERMINAL_PROMPT=0, GIT_ASKPASS=<temp script echoing x-access-token/token>); 0.36 has no such hook — the subprocess inherits your process env (git_subprocess.rs:135 only sets LC_ALL=C), so a 0.36 gt must set GIT_ASKPASS on its own process or lean on the user's credential helper.
- GitImportOptions holds a non-Send closure (remote_auto_track_bookmarks matcher) — construct it inside the worker thread, it cannot cross a spawn_blocking boundary (jj.rs:897-903). Auto_local_bookmark:true floods repos with a local bookmark per origin branch — use false and track selectively.
- jj-lib pain points impulse papered over (talk gold): (1) stale secondary workspaces — no library API, impulse literally shells out to `jj workspace update-stale` (jj.rs:1451-1480); (2) refs/pull/N/head unfetchable via jj (expand_fetch_refspecs only handles refs/heads|tags) — drops to raw git2, then must import_refs or the objects are 'invisible to revset and ancestry queries even though the objects are in the odb' (jj.rs:2244-2251, 4397-4400); (3) that import can't run under the same per-workspace lock — non-reentrant mutex deadlock, documented in a comment; (4) finish() must be called with the ORIGINAL op id even on a no-change snapshot, purely to persist the stat cache 'through jj-lib's documented API instead of an opaque Drop' (jj.rs:3913-3921); (5) hand-rolled iterative post-order collect_chain because naive reversed pre-order breaks on merges (jj.rs:3568-3571); (6) no ready-made 'rebase branch onto' — impulse reimplements it commit-by-commit with CommitRewriter.
- Concurrency: one tokio Mutex per canonicalized workspace root gates EVERY operation ('no file IO from the app while a jj operation is happening'), all jj-lib work inside tokio::task::spawn_blocking, async jj-lib futures driven by pollster::block_on on that blocking thread. jj-lib's own working-copy lock + op-heads store handle cross-process safety; the app lock exists to serialize its own tasks.
- Stacked-PR base management: create with baseRefName = parent PR's headRefName (explicit user choice > parent head > default branch). On parent merge: rebase ONLY the direct child branch in jj (descendants move automatically inside the repo — rebasing each stack member separately 'would flatten the stack'), then force-push the whole substack in one push_bookmarks call; PR base retargeting is left to GitHub's auto-retarget on branch deletion (impulse's updatePullRequest never touches baseRefName — gt can pass baseRefName in UpdatePullRequestInput if it wants explicit Graphite-style retargeting).
- Only branches with LOCAL jj bookmarks participate in stack rebase/push — remote-only PR branches are skipped with a 'jj bookmark track name@origin' hint (stack.rs:696-705). Deleting a bookmark = set_local_bookmark_target(name, RefTarget::absent()) + export_refs, which removes refs/heads/<name> from .git (jj.rs:4807-4864).
- Force-push safety = compare-and-swap: push passes old_target (the remote-tracking bookmark's last-known position) so a concurrent remote update rejects the push instead of clobbering (jj.rs:3703-3712; 0.36 push_branches/GitRefUpdate.expected_current_target does the same).


---

# settings-async-init

## Summary

Settings/config/async subsystem of jj-lib 0.36.0 (edition 2024, default features = ["git"]). UserSettings is an immutable snapshot of a layered StackedConfig; jj-lib ships one built-in Default layer (lib/src/config/misc.toml, wired in at lib/src/config.rs:900-902) that pre-fills EVERY key UserSettings::from_config requires (user.name="", user.email="", operation.hostname="", operation.username="", signing.backend="none", signing.behavior="keep", git.auto-local-bookmark=false, git.abandon-unreachable-commits=true, git.executable-path="git", git.write-change-id-header=true), so StackedConfig::with_defaults() + one overlay layer with real user.name/user.email is the entire config story for gt. There is NO global/one-time init function in jj-lib (lib.rs is just module declarations; the CLI's CliRunner::init only sets up tracing_subscriber and a SIGINT cleanup guard, both optional). The async story is a facade: nearly every I/O API is `async fn` (Backend trait, Store, LockedWorkingCopy, op_store, rewrite/rebase, tree merge/diff), but the concrete backends are synchronous underneath — GitBackend::read_file literally calls read_file_sync and wraps the bytes in Cursor (git_backend.rs:1007-1014), Backend::concurrency() is 1 for git (git_backend.rs:1003), TreeState::snapshot is `async` yet does its filesystem traversal inside rayon::scope (local_working_copy.rs:1280-1307), and jj-lib itself sprinkles pollster's `.block_on()` internally (transaction.rs write_view/write_operation:147,160; workspace.rs check_out:445 and LockedWorkspace::finish:467; commit.rs:137; merged_tree.rs:142,229; tree_builder.rs:115). The idiomatic consumer pattern, used pervasively by the CLI, is `use pollster::FutureExt as _; fut.block_on()` for futures and `futures::executor::block_on_stream(stream)` for TreeDiffStream. This directly feeds the three-writes talk: Transaction::commit writes ONLY the op-store/view + index + publishes an op head (transaction.rs:120-167) — the working-copy files (LockedWorkingCopy::check_out/reset), the workspace's working-copy state file (LockedWorkspace::finish(op_id)), and the colocated git refs (jj_lib::git::export_refs / reset_head) are each separate explicit writes the consumer must perform, exactly as cli_util.rs snapshot_working_copy demonstrates (snapshot -> tx.commit -> locked_ws.finish, cli/src/cli_util.rs:1894-1976).

## Key APIs

### StackedConfig::with_defaults  (/Users/lita/src/jj/lib/src/config.rs:653)
```rust
pub fn with_defaults() -> Self
```
Returns config whose only layer is DEFAULT_CONFIG_LAYERS = [parse(include_str!("config/misc.toml"))] (config.rs:900-902). misc.toml supplies user.name="", user.email="", operation.hostname="", operation.username="", signing.backend="none", signing.behavior="keep", git.* defaults, fsmonitor.backend="none", working-copy.eol-conversion="none". Sufficient for UserSettings::from_config to succeed with no extra keys.

### StackedConfig::add_layer  (/Users/lita/src/jj/lib/src/config.rs:684)
```rust
pub fn add_layer(&mut self, layer: impl Into<Arc<ConfigLayer>>)
```
Layers are inserted per ConfigSource precedence (Default < EnvBase < User < Repo < Workspace < EnvOverrides < CommandArg). Use ConfigSource::User for gt's overlay.

### ConfigLayer::empty / set_value / parse  (/Users/lita/src/jj/lib/src/config.rs:327,422,341)
```rust
pub fn empty(source: ConfigSource) -> Self; pub fn set_value(&mut self, name: impl ToConfigNamePath, new_value: impl Into<ConfigValue>) -> Result<Option<ConfigValue>, ConfigUpdateError>; pub fn parse(source: ConfigSource, text: &str) -> Result<Self, ConfigLoadError>
```
set_value("user.name", "Lita") style. ToConfigNamePath is implemented for &str dotted paths and arrays like ["remotes", name].

### UserSettings::from_config  (/Users/lita/src/jj/lib/src/settings.rs:125)
```rust
pub fn from_config(config: StackedConfig) -> Result<Self, ConfigGetError>
```
Eagerly reads user.name, user.email, operation.hostname, operation.username, signing.behavior (+optional signing.key, debug.* timestamps, debug.randomness-seed). No panics; returns ConfigGetError::NotFound{name} if any of those 5 keys is absent from every layer — impossible when with_defaults() was used. Empty-string name/email are accepted here; enforcement happens elsewhere (cli push refuses placeholder/empty authors at cli/src/commands/git/push.rs:570-576).

### UserSettings::signature  (/Users/lita/src/jj/lib/src/settings.rs:203)
```rust
pub fn signature(&self) -> Signature
```
Author/committer Signature{name, email, timestamp:now} used by CommitBuilder. Placeholder consts USER_NAME_PLACEHOLDER="(no name configured)" (settings.rs:177) and USER_EMAIL_PLACEHOLDER (settings.rs:185); commit_builder.rs:238-247 upgrades empty/placeholder author from committer on rewrite.

### UserSettings::operation_hostname / operation_username  (/Users/lita/src/jj/lib/src/settings.rs:195,199)
```rust
pub fn operation_hostname(&self) -> &str; pub fn operation_username(&self) -> &str
```
Consumed by create_op_metadata (transaction.rs:170-191) for every operation. Library default is "" (misc.toml); the CLI fills them from whoami::fallible::hostname()/username() with $USER fallback in an EnvBase layer (cli/src/config.rs:580-599, keys operation.hostname/operation.username). gt should do the same via the `whoami` crate or hardcode.

### ConfigGetError  (/Users/lita/src/jj/lib/src/config.rs:80-98)
```rust
pub enum ConfigGetError { NotFound { name: String }, Type { name: String, error: Box<dyn Error + Send + Sync>, source_path: Option<PathBuf> } }
```
The only failure mode of settings lookups; .optional() (ConfigGetResultExt) maps NotFound to Ok(None).

### GitSettings::from_settings  (/Users/lita/src/jj/lib/src/git.rs:94-103 (struct at 86-92))
```rust
pub struct GitSettings { pub auto_local_bookmark: bool, pub abandon_unreachable_commits: bool, pub executable_path: PathBuf, pub write_change_id_header: bool } impl GitSettings { pub fn from_settings(settings: &UserSettings) -> Result<Self, ConfigGetError> }
```
Reads exactly git.auto-local-bookmark, git.abandon-unreachable-commits, git.executable-path, git.write-change-id-header. There is NO git.subprocess key in 0.36 — fetch/push always shell out to the git binary at executable_path via GitSubprocessContext (git.rs:44). git.fetch / git.push (default remote names) are CLI-level settings read directly (cli/src/commands/git/fetch.rs:202 KEY="git.fetch", push.rs:736 settings.get_string("git.push")), not part of GitSettings; gt can hardcode "origin". GitBackend load/init construct GitSettings themselves from UserSettings (git_backend.rs:221,327), and gix repos are opened with author/committer config_overrides taken from user.name/user.email (gix_open_opts_from_settings, git_backend.rs:518-534).

### Signer::from_settings  (/Users/lita/src/jj/lib/src/signing.rs:180)
```rust
pub fn from_settings(settings: &UserSettings) -> Result<Self, SignInitError>
```
Needed for Workspace::init_* (called internally by init_colocated_git). With defaults signing.backend="none" => main_backend=None, behavior=Keep => commits unsigned. No keys required.

### Workspace::init_colocated_git  (/Users/lita/src/jj/lib/src/workspace.rs:216-241)
```rust
pub fn init_colocated_git(user_settings: &UserSettings, workspace_root: &Path) -> Result<(Self, Arc<ReadonlyRepo>), WorkspaceInitError>
```
cfg(feature="git"). Creates .jj/ plus a colocated .git via GitBackend::init_colocated; internally builds Signer::from_settings and default op/index/submodule stores + default_working_copy_factory (init_with_backend, workspace.rs:331-349). This is `gt init`.

### Workspace::load  (/Users/lita/src/jj/lib/src/workspace.rs:382-391)
```rust
pub fn load(user_settings: &UserSettings, workspace_path: &Path, store_factories: &StoreFactories, working_copy_factories: &WorkingCopyFactories) -> Result<Self, WorkspaceLoadError>
```
Pass &StoreFactories::default() and &default_working_copy_factories(). StoreFactories::default (repo.rs:424-...) registers SimpleBackend + GitBackend (under feature "git", which is a default feature) + SimpleOpStore/SimpleOpHeadsStore/DefaultIndexStore/DefaultSubmoduleStore — everything a repo created by init_colocated_git needs. default_working_copy_factories() at workspace.rs:605 registers the local working copy; default_working_copy_factory() at workspace.rs:614.

### Workspace accessors  (/Users/lita/src/jj/lib/src/workspace.rs:393-426)
```rust
pub fn workspace_root(&self) -> &Path; pub fn workspace_name(&self) -> &WorkspaceName; pub fn repo_loader(&self) -> &RepoLoader; pub fn settings(&self) -> &UserSettings; pub fn working_copy(&self) -> &dyn WorkingCopy; pub fn start_working_copy_mutation(&mut self) -> Result<LockedWorkspace<'_>, WorkingCopyStateError>
```
start_working_copy_mutation is WRITE #2's entry point (takes the wc lock).

### LockedWorkspace::locked_wc / finish  (/Users/lita/src/jj/lib/src/workspace.rs:462,466-470)
```rust
pub fn locked_wc(&mut self) -> &mut dyn LockedWorkingCopy; pub fn finish(self, operation_id: OperationId) -> Result<(), WorkingCopyStateError>
```
finish() is SYNC for the caller (internally self.locked_wc.finish(op_id).block_on() at workspace.rs:467). It records which operation the working copy is at — WRITE #2 of three. Must be called with the OperationId of the repo returned by tx.commit(), AFTER the transaction commits.

### Workspace::check_out  (/Users/lita/src/jj/lib/src/workspace.rs:428-453)
```rust
pub fn check_out(&mut self, operation_id: OperationId, old_tree: Option<&MergedTree>, commit: &Commit) -> Result<CheckoutStats, CheckoutError>
```
Sync convenience combining locked_wc().check_out(commit).block_on() + finish(op_id): updates working-copy FILES on disk (WRITE #3 of the file kind) and wc state in one call. Returns CheckoutError::ConcurrentCheckout if old_tree mismatches.

### trait LockedWorkingCopy  (/Users/lita/src/jj/lib/src/working_copy.rs:109-156)
```rust
#[async_trait] pub trait LockedWorkingCopy: Any + Send { fn old_operation_id(&self) -> &OperationId; fn old_tree(&self) -> &MergedTree; async fn snapshot(&mut self, options: &SnapshotOptions) -> Result<(MergedTree, SnapshotStats), SnapshotError>; async fn check_out(&mut self, commit: &Commit) -> Result<CheckoutStats, CheckoutError>; async fn reset(&mut self, commit: &Commit) -> Result<(), ResetError>; async fn recover(&mut self, commit: &Commit) -> Result<(), ResetError>; async fn set_sparse_patterns(&mut self, new_sparse_patterns: Vec<RepoPathBuf>) -> Result<CheckoutStats, CheckoutError>; async fn finish(self: Box<Self>, operation_id: OperationId) -> Result<Box<dyn WorkingCopy>, WorkingCopyStateError>; }
```
All async; call with pollster: locked_ws.locked_wc().snapshot(&opts).block_on(). reset() = point wc state at a commit WITHOUT touching files (used after git updated files itself, cli_util.rs:1215); check_out() = also rewrite files.

### SnapshotOptions  (/Users/lita/src/jj/lib/src/working_copy.rs:212-233)
```rust
pub struct SnapshotOptions<'a> { pub base_ignores: Arc<GitIgnoreFile>, pub progress: Option<&'a SnapshotProgress<'a>>, pub start_tracking_matcher: &'a dyn Matcher, pub force_tracking_matcher: &'a dyn Matcher, pub max_new_file_size: u64 }
```
Minimal gt construction: SnapshotOptions { base_ignores: GitIgnoreFile::empty() (gitignore.rs:53, returns Arc<Self>), progress: None, start_tracking_matcher: &EverythingMatcher (matchers.rs:120), force_tracking_matcher: &NothingMatcher (matchers.rs:107), max_new_file_size: u64::MAX }. CLI reads snapshot.max-new-file-size (cli_util.rs:1436-1441) — that key lives in CLI's misc.toml, NOT jj-lib's defaults, so don't settings.get it in gt.

### pollster blocking idiom  (/Users/lita/src/jj/cli/src/cli_util.rs:145 (use pollster::FutureExt as _), usage e.g. 1930, 1215, 2677)
```rust
use pollster::FutureExt as _;  some_async_call(...).block_on()
```
pollster 0.4.0 (workspace Cargo.toml:75). This is THE executor for jj futures — no tokio runtime needed or wanted for backend I/O. For streams: use futures::executor::block_on_stream (cli/src/diff_util.rs:28,688: `Ok(block_on_stream(stream).filter_ok(...))`). Only the watchman fsmonitor path builds its own tokio current-thread runtime internally (local_working_copy.rs:1195-1204) — irrelevant unless fsmonitor.backend="watchman".

### Transaction::commit  (/Users/lita/src/jj/lib/src/transaction.rs:120-125 (write at 130-167))
```rust
pub fn commit(self, description: impl Into<String>) -> Result<Arc<ReadonlyRepo>, TransactionCommitError>
```
WRITE #1: op store view + operation + index + publish op head. Sync facade (write_view/write_operation are .block_on()'d internally at :147,:160). Asserts !mut_repo.has_rewrites() — you MUST call repo_mut().rebase_descendants() before commit if you rewrote commits. Returns the new ReadonlyRepo whose op_id() feeds LockedWorkspace::finish.

### create_op_metadata  (/Users/lita/src/jj/lib/src/transaction.rs:170-191)
```rust
pub fn create_op_metadata(user_settings: &UserSettings, description: String, is_snapshot: bool) -> OperationMetadata
```
Every op records hostname/username from settings.operation_hostname()/operation_username(); with library defaults these are "" — set them for pretty `jj op log` output.

### trait Backend (async facade)  (/Users/lita/src/jj/lib/src/backend.rs:413-502)
```rust
#[async_trait] pub trait Backend: Any + Send + Sync + Debug { fn concurrency(&self) -> usize; async fn read_file(&self, path: &RepoPath, id: &FileId) -> BackendResult<Pin<Box<dyn AsyncRead + Send>>>; async fn write_file(&self, path: &RepoPath, contents: &mut (dyn AsyncRead + Send + Unpin)) -> BackendResult<FileId>; async fn read_tree(..); async fn write_tree(..); async fn read_commit(..); async fn write_commit(&self, contents: Commit, sign_with: Option<&mut SigningFn>) -> BackendResult<(CommitId, Commit)>; ... }
```
Uses async_trait + tokio::io::AsyncRead types in signatures, but GitBackend implements read_file as `let data = self.read_file_sync(id)?; Ok(Box::pin(Cursor::new(data)))` (git_backend.rs:1007-1014) — fully synchronous gix object read under an async signature — and returns concurrency()=1 (git_backend.rs:1003; backend.rs:431-438 doc: "A local backend like the Git backend ... may want to set this to 1"). The async-ness exists for future cloud backends; today pollster::block_on never actually parks on I/O.

### Store convenience (sync + async pairs)  (/Users/lita/src/jj/lib/src/store.rs:151,155,174,191,195,223,238,246)
```rust
pub fn get_commit(self: &Arc<Self>, id: &CommitId) -> BackendResult<Commit>; pub async fn get_commit_async(..); pub async fn write_commit(self: &Arc<Self>, commit: backend::Commit, sign_with: Option<&mut SigningFn>) -> BackendResult<Commit>; pub fn get_tree(self: &Arc<Self>, dir: RepoPathBuf, id: &TreeId) -> BackendResult<Tree>; pub async fn get_tree_async(..); pub async fn read_file(..); pub async fn write_file(..)
```
Sync variants exist for commit/tree reads (they block_on internally); file/blob I/O is async-only — block_on it.

### MergedTree diff APIs  (/Users/lita/src/jj/lib/src/merged_tree.rs:292-329, type alias at 389)
```rust
pub fn diff_stream<'matcher>(&self, other: &Self, matcher: &'matcher dyn Matcher) -> TreeDiffStream<'matcher>;  pub type TreeDiffStream<'matcher> = BoxStream<'matcher, TreeDiffEntry>
```
diff_stream itself is a sync constructor returning an async Stream; consume from sync code with futures::executor::block_on_stream. Also sync facades MergedTree::trees() (:141-143) and path_value() (:228-230) that block_on their _async twins.

### rewrite helpers (all async)  (/Users/lita/src/jj/lib/src/rewrite.rs:57,145)
```rust
pub async fn merge_commit_trees(repo: &dyn Repo, commits: &[Commit]) -> BackendResult<MergedTree>; pub async fn rebase_commit(mut_repo: &mut MutableRepo, old_commit: Commit, new_parent_ids: Vec<CommitId>) -> BackendResult<Commit>
```
gt sync's stack rebase: rebase_commit(...).block_on()? per commit (CLI does exactly this, cli/src/commands/new.rs:230). Prefer MutableRepo::rebase_descendants for bulk.

### CLI settings construction (reference pattern)  (/Users/lita/src/jj/cli/src/config.rs:572-578 (config_from_environment), 615-636 (default_config_layers), 582-599 (op host/user env layer); /Users/lita/src/jj/cli/src/cli_util.rs:4047 (UserSettings::from_config(config)?))
```rust
pub fn config_from_environment(default_layers: impl IntoIterator<Item = ConfigLayer>) -> RawConfig; pub fn default_config_layers() -> Vec<ConfigLayer>
```
These are jj-cli crate items, not jj-lib. gt reimplements in ~10 lines: with_defaults + one ConfigSource::User layer.

## Minimal flow
// deps: jj-lib = "0.36" (default features include "git"), pollster = "0.4", futures = "0.3" (only for diff streams)
use pollster::FutureExt as _;

// --- settings (once per process) ---
let mut config = StackedConfig::with_defaults();                    // config.rs:653 — makes every required key present
let mut layer = ConfigLayer::empty(ConfigSource::User);
layer.set_value("user.name", "Lita")?;                              // else commits get empty author (push refuses)
layer.set_value("user.email", "lita@moment.dev")?;
layer.set_value("operation.hostname", whoami::fallible::hostname().unwrap_or_default())?; // optional, "" otherwise
layer.set_value("operation.username", whoami::fallible::username().unwrap_or_default())?;
config.add_layer(layer);
let settings = UserSettings::from_config(config)?;                  // settings.rs:125; Err(ConfigGetError) only, never panics

// --- gt init ---
let (workspace, repo) = Workspace::init_colocated_git(&settings, root)?;  // workspace.rs:216

// --- every other command: load ---
let mut workspace = Workspace::load(&settings, root,
    &StoreFactories::default(),                                     // repo.rs:424 — registers GitBackend etc.
    &default_working_copy_factories())?;                            // workspace.rs:605
let repo: Arc<ReadonlyRepo> = workspace.repo_loader().load_at_head()?;

// --- gt create/modify: snapshot + commit — THE THREE WRITES ---
let mut locked_ws = workspace.start_working_copy_mutation()?;       // takes wc lock
let opts = SnapshotOptions { base_ignores: GitIgnoreFile::empty(), progress: None,
    start_tracking_matcher: &EverythingMatcher, force_tracking_matcher: &NothingMatcher,
    max_new_file_size: u64::MAX };
let (new_tree, _stats) = locked_ws.locked_wc().snapshot(&opts).block_on()?;   // async facade -> pollster
let mut tx = repo.start_transaction();
let commit = tx.repo_mut().rewrite_commit(&wc_commit).set_tree(new_tree).write()?; // or new_commit(...)
tx.repo_mut().set_wc_commit(ws_name, commit.id().clone())?;
tx.repo_mut().rebase_descendants()?;                                // MANDATORY before commit if anything was rewritten (transaction.rs:136 assert)
// WRITE (git refs, colocated): jj_lib::git::export_refs / reset_head here — see git subsystem notes
let repo = tx.commit("gt create")?;                                 // WRITE 1: op log (transaction.rs:120)
locked_ws.finish(repo.op_id().clone())?;                            // WRITE 2: workspace wc-state file (workspace.rs:466); sync, block_on inside

// --- gt checkout: update files on disk ---
let stats = workspace.check_out(repo.op_id().clone(), None, &target_commit)?;  // WRITE 3: files (workspace.rs:428, block_on inside)

// --- diffs from sync code ---
let stream = tree_a.diff_stream(&tree_b, &EverythingMatcher);       // merged_tree.rs:292
for entry in futures::executor::block_on_stream(stream) { ... }     // diff_util.rs:688 pattern

// --- gt sync stack rebase ---
let rebased = rebase_commit(tx.repo_mut(), old_commit, vec![new_main_id]).block_on()?;  // rewrite.rs:145

## Gotchas
- No global init exists or is needed in jj-lib: lib.rs (/Users/lita/src/jj/lib/src/lib.rs) is pure module declarations; the CLI's CliRunner::init (cli_util.rs:3777-3779) only starts tracing_subscriber and cleanup_guard::init() (SIGINT handling) — both optional for gt. No rustls/crypto-provider/gix global setup anywhere.
- UserSettings::from_config never panics but WILL return ConfigGetError::NotFound for user.name/user.email/operation.hostname/operation.username/signing.behavior if you build StackedConfig::empty() instead of with_defaults(). With with_defaults() everything succeeds but name/email are "" — jj's own push guard (cli/src/commands/git/push.rs:570-576) refuses commits with empty/placeholder author, so gt submit should enforce real values too.
- Do NOT read snapshot.max-new-file-size, git.fetch, git.push, or ui.* through UserSettings in a lib-only consumer: those defaults live in jj-cli's config TOMLs (cli/src/config/*.toml), not jj-lib's misc.toml, so settings.get returns NotFound. Hardcode or add them to your own layer.
- git.subprocess no longer exists in 0.36 — git fetch/push ALWAYS spawn the external git binary (GitSubprocessContext, git.rs:44-45) found via git.executable-path (default "git", part of GitSettings). GitSettings has exactly 4 fields (git.rs:86-92); git.fetch/git.push default-remote keys are CLI conveniences.
- The async story is a facade over sync I/O: Backend trait is #[async_trait] with tokio AsyncRead in signatures (backend.rs:440-450), but GitBackend::read_file does a sync gix read then wraps in Cursor (git_backend.rs:1012-1013), and concurrency()==1 (git_backend.rs:1003; rationale comment backend.rs:431-434 'A local backend like the Git backend (at until it supports partial clones) may want to set this to 1'). Therefore pollster (a trivial thread-parking executor, no reactor) is sufficient AND the sanctioned pattern — do not pull in tokio; TreeState::query_watchman spins up its own current-thread tokio runtime only when watchman fsmonitor is enabled (local_working_copy.rs:1195-1204).
- jj-lib itself calls pollster .block_on() inside nominally-sync APIs (transaction.rs:147,160; workspace.rs:445,467; commit.rs:137; merged_tree.rs:142,229; tree_builder.rs:115; operation.rs:112,118). Consequence: NEVER call those sync facades (tx.commit, Workspace::check_out, LockedWorkspace::finish, MergedTree::trees, Commit::tree) from inside an async task on a tokio runtime thread — pollster will park the thread. From plain sync main() everything is fine.
- TreeState::snapshot is `async fn` but performs the whole filesystem walk synchronously inside rayon::scope (local_working_copy.rs:1280-1307) — snapshotting is CPU/FS-bound and blocking regardless of executor.
- Transaction::write asserts !mut_repo.has_rewrites() with message 'BUG: Descendants have not been rebased after the last rewrites.' (transaction.rs:136-139) — always call repo_mut().rebase_descendants() after rewrite_commit/amend before tx.commit, or the process panics.
- Order of the three writes matters and is encoded in cli_util.rs snapshot_working_copy (cli/src/cli_util.rs:1894-1976): take wc lock FIRST (start_working_copy_mutation), snapshot, then tx.commit (op log), then locked_ws.finish(new_op_id) LAST; for checkout the same: commit the op, then update files/state with the new op id (import_git_head pattern at cli_util.rs:1211-1218 uses locked_wc().reset(&wc_commit).block_on() when git already moved the files). Finishing the wc at an op id that was never committed, or committing without finishing, produces a 'stale working copy' on next load.
- For colocated repos the CLI serializes git import/snapshot/export cycles with a dedicated file lock at .jj/repo/git_import_export.lock (cli_util.rs:1102-1112) and reloads at current op heads while holding it (op_heads_store.get_op_heads().block_on(), cli_util.rs:1136-1139) — a multi-process-safe gt should copy this.
- Workspace::load requires factories that can load what init created: StoreFactories::default() registers GitBackend only under feature "git" (repo.rs:433-441), which IS in default features (lib/Cargo.toml:99 default=["git"]) — but if you set default-features=false you'll get StoreLoadError for the git backend.
- Signer::from_settings (signing.rs:180) is invoked by every Workspace::init_* — with default signing.backend="none" it yields a no-op signer; SignBehavior default "keep" means unsigned commits stay unsigned. No signing keys required for the demo.
- UserSettings is cheap to clone (Arc-backed, settings.rs:42-47) and the same instance must be reused across transactions in one process: its ChaCha20 JJRng guarantees unique change IDs (settings.rs:160-166 comment); constructing fresh UserSettings repeatedly with a fixed debug.randomness-seed would duplicate change IDs.
- gix repos opened by GitBackend inject author/committer from user.name/user.email as git config overrides (git_backend.rs:518-534) because gix requires a committer to write reflogs — another reason to set real user.name/user.email even though from_config accepts empty strings.


---

# GAP-CRITIC

## Summary

Resolved the 5 remaining blockers for gt on jj-lib 0.36 (source verified at /Users/lita/src/jj): (1) gt init recipe — Workspace::init_colocated_git for a fresh dir vs Workspace::init_external_git(root/.git) when .git already exists (jj's own `git init --colocate` branches exactly this way at cli/src/commands/git/init.rs:160-166), followed by a first 'import git refs' transaction (free git::import_refs + export_refs) and an 'import git head' transaction (git::import_head + check_out of view().git_head()); (2) the snapshot transaction — exact SnapshotOptions fields (5, no Default), LockedWorkingCopy async trait methods, tree-change comparison via tree_ids_and_labels(), and the rewrite_commit().set_tree().write() fold-in; (3) commit/bookmark creation — MutableRepo::new_commit takes a MergedTree VALUE (not a tree id), CommitBuilder setters, set_local_bookmark_target with RefTarget::normal/absent; (4) fetch/push — GitFetch::fetch now takes an ExpandedFetchRefSpecs built by git::expand_fetch_refspecs(remote, StringExpression), push_branches takes GitBranchPushTargets{Vec<(RefNameBuf, BookmarkPushUpdate)>} and only updates the view when GitPushStats::all_ok(); auth is purely the spawned `git` binary's own credential machinery because create_command inherits the parent env (only LC_ALL=C is overridden, stdin is null); (5) stack computation — fully programmatic revsets via ResolvedRevsetExpression::commit(id).range(&other).evaluate(&dyn Repo) with Revset::iter yielding children-before-parents. Each API below is a load-bearing brick in the three-writes story: tx.commit writes only the op log; Workspace::check_out / LockedWorkspace::finish write files + wc state; git::reset_head/export_refs (pre-commit, inside the tx) write the colocated .git.

## Key APIs

### Workspace::init_colocated_git (gt init, fresh dir)  (/Users/lita/src/jj/lib/src/workspace.rs:216)
```rust
pub fn init_colocated_git(user_settings: &UserSettings, workspace_root: &Path) -> Result<(Self, Arc<ReadonlyRepo>), WorkspaceInitError>
```
cfg(feature="git"). Creates .git IN the workspace root via GitBackend::init_colocated (it precomputes the store-relative path for you). Internally runs a transaction that checks out the ROOT commit into the default workspace and commits op "add workspace 'default'" (init_working_copy, workspace.rs:129-155), so the returned Arc<ReadonlyRepo> is already 1 op past genesis. For an EXISTING git repo use init_external_git — jj's own `git init --colocate` picks External when workspace_root/.git exists (cli/src/commands/git/init.rs:160-166); init_colocated_git would gix-init a new repo.

### Workspace::init_external_git (gt init on existing git repo)  (/Users/lita/src/jj/lib/src/workspace.rs:248)
```rust
pub fn init_external_git(user_settings: &UserSettings, workspace_root: &Path, git_repo_path: &Path) -> Result<(Self, Arc<ReadonlyRepo>), WorkspaceInitError>
```
git_repo_path = workspace_root.join(".git") for colocation. After init the jj view is EMPTY: reproduce init_git_refs (cli/src/commands/git/init.rs:231-261): tx = repo.start_transaction(); git::import_refs(tx.repo_mut(), &opts with abandon_unreachable_commits=false)?; if colocated { git::export_refs(tx.repo_mut())?; } repo = tx.commit("import git refs")?. Then move @ onto HEAD: git::import_head(tx.repo_mut()) (git.rs:847, pub fn import_head(mut_repo: &mut MutableRepo) -> Result<(), GitImportError>), read tx.repo().view().git_head().as_normal(), store().get_commit(&id), tx.repo_mut().check_out(name, &commit) (init.rs:206-216).

### Workspace::load (every gt command after init)  (/Users/lita/src/jj/lib/src/workspace.rs:382)
```rust
pub fn load(user_settings: &UserSettings, workspace_path: &Path, store_factories: &StoreFactories, working_copy_factories: &WorkingCopyFactories) -> Result<Self, WorkspaceLoadError>
```
Pass &StoreFactories::default() (Default impl repo.rs:424, includes GitBackend under default 'git' feature) and &default_working_copy_factories() (workspace.rs:605, pub fn default_working_copy_factories() -> WorkingCopyFactories). Then workspace.repo_loader().load_at_head() -> Result<Arc<ReadonlyRepo>, RepoLoaderError> (repo.rs:756).

### SnapshotOptions (exact fields, no Default)  (/Users/lita/src/jj/lib/src/working_copy.rs:212)
```rust
pub struct SnapshotOptions<'a> { pub base_ignores: Arc<GitIgnoreFile>, pub progress: Option<&'a SnapshotProgress<'a>>, pub start_tracking_matcher: &'a dyn Matcher, pub force_tracking_matcher: &'a dyn Matcher, pub max_new_file_size: u64 }
```
gt values: base_ignores: GitIgnoreFile::empty() (gitignore.rs:53, pub fn empty() -> Arc<Self>), progress: None, start_tracking_matcher: &EverythingMatcher (matchers.rs:120), force_tracking_matcher: &NothingMatcher (matchers.rs:107), max_new_file_size: u64::MAX. SnapshotProgress<'a> = dyn Fn(&RepoPath) + Sync (working_copy.rs:236).

### trait LockedWorkingCopy (the write-#2 surface)  (/Users/lita/src/jj/lib/src/working_copy.rs:110)
```rust
async fn snapshot(&mut self, options: &SnapshotOptions) -> Result<(MergedTree, SnapshotStats), SnapshotError>; async fn check_out(&mut self, commit: &Commit) -> Result<CheckoutStats, CheckoutError>; async fn reset(&mut self, commit: &Commit) -> Result<(), ResetError>; async fn finish(self: Box<Self>, operation_id: OperationId) -> Result<Box<dyn WorkingCopy>, WorkingCopyStateError>; fn old_operation_id(&self) -> &OperationId; fn old_tree(&self) -> &MergedTree
```
#[async_trait]; drive with pollster: use pollster::FutureExt as _; locked_ws.locked_wc().snapshot(&opts).block_on()?. Access via Workspace::start_working_copy_mutation(&mut self) -> Result<LockedWorkspace<'_>, WorkingCopyStateError> (workspace.rs:418); LockedWorkspace::locked_wc(&mut self) -> &mut dyn LockedWorkingCopy (:462); LockedWorkspace::finish(self, operation_id: OperationId) -> Result<(), WorkingCopyStateError> (:466) — finish is sync (blocks internally) and consumes the lock.

### Snapshot fold-in + change detection (gt create/modify write-0)  (/Users/lita/src/jj/cli/src/cli_util.rs:1933)
```rust
if new_tree.tree_ids_and_labels() != wc_commit.tree().tree_ids_and_labels() { mut_repo.rewrite_commit(&wc_commit).set_tree(new_tree).write()?; ... }
```
MergedTree::tree_ids_and_labels(&self) -> (&Merge<TreeId>, &ConflictLabels) (merged_tree.rs:131) is THE tree-equality check. Commit::tree(&self) -> MergedTree (commit.rs:124, sync, blocks internally). If unchanged, do NOT rewrite (avoids CommitBuilder 'already exists' error) but STILL call locked_ws.finish(op_id) with the current op id.

### MutableRepo::new_commit (gt create)  (/Users/lita/src/jj/lib/src/repo.rs:948)
```rust
pub fn new_commit(&mut self, parents: Vec<CommitId>, tree: MergedTree) -> CommitBuilder<'_>
```
Takes a MergedTree VALUE, not a tree id (0.36 changed this). For an empty child of parent: new_commit(vec![parent.id().clone()], parent.tree()). Settings come from the repo; no settings arg. Panics if parents is empty.

### MutableRepo::rewrite_commit (gt modify amend)  (/Users/lita/src/jj/lib/src/repo.rs:954)
```rust
pub fn rewrite_commit(&mut self, predecessor: &Commit) -> CommitBuilder<'_>
```
CommitBuilder::write records the rewrite in parent_mapping -> you MUST call repo_mut().rebase_descendants() before tx.commit or transaction.rs:136-139 assert panics. rebase_descendants(&mut self) -> BackendResult<usize> (repo.rs:1428); it is also what drags bookmarks + wc pointer onto the amended commit. Afterwards re-read final ids from the view (view().get_wc_commit_id) or new_parents(&[old_id]) (repo.rs:1041, pub fn new_parents(&self, old_ids: &[CommitId]) -> Vec<CommitId>).

### CommitBuilder setters + write  (/Users/lita/src/jj/lib/src/commit_builder.rs:66)
```rust
pub fn set_parents(mut self, parents: Vec<CommitId>) -> Self (:66); pub fn set_tree(mut self, tree: MergedTree) -> Self (:88); pub fn set_description(mut self, description: impl Into<String>) -> Self (:116); pub fn write(self) -> BackendResult<Commit> (:163)
```
Builder-by-value chaining (attached CommitBuilder<'_>). write() puts the commit in the store, adds it as a view head (for new commits), and records rewrites (for rewrite_commit). It does NOT touch wc pointer or bookmarks.

### MutableRepo::check_out / edit / set_wc_commit (gt checkout, in-view half)  (/Users/lita/src/jj/lib/src/repo.rs:1514)
```rust
pub fn check_out(&mut self, name: WorkspaceNameBuf, commit: &Commit) -> Result<Commit, CheckOutCommitError> (:1514); pub fn edit(&mut self, name: WorkspaceNameBuf, commit: &Commit) -> Result<(), EditCommitError> (:1526); pub fn set_wc_commit(&mut self, name: WorkspaceNameBuf, commit_id: CommitId) -> Result<(), RewriteRootCommit> (:1458)
```
check_out creates a NEW empty wc commit on top (new_commit(vec![commit.id()], commit.tree()).write() then edit) and returns it — feed that returned Commit to Workspace::check_out after tx.commit. name = WorkspaceName::DEFAULT.to_owned(). edit() silently abandons the previous wc commit if it was empty+undescribed.

### Workspace::check_out (gt checkout, on-disk half = writes #2a+#2b)  (/Users/lita/src/jj/lib/src/workspace.rs:428)
```rust
pub fn check_out(&mut self, operation_id: OperationId, old_tree: Option<&MergedTree>, commit: &Commit) -> Result<CheckoutStats, CheckoutError>
```
Call AFTER tx.commit with repo.op_id().clone() of the NEW repo. old_tree = Some(&old_wc_commit.tree()) gives ConcurrentCheckout protection (compares tree_ids_and_labels against locked_wc.old_tree(), :439-443); None skips it. Internally: start_working_copy_mutation -> locked_wc.check_out(commit).block_on() -> finish(operation_id).

### MutableRepo::set_local_bookmark_target + RefTarget (gt create/sync bookmarks)  (/Users/lita/src/jj/lib/src/repo.rs:1676)
```rust
pub fn set_local_bookmark_target(&mut self, name: &RefName, target: RefTarget); pub fn get_local_bookmark(&self, name: &RefName) -> RefTarget (:1672); pub fn get_remote_bookmark(&self, symbol: RemoteRefSymbol<'_>) -> RemoteRef (:1699); pub fn track_remote_bookmark(&mut self, symbol: RemoteRefSymbol<'_>) -> IndexResult<()> (:1724)
```
RefTarget::normal(id: CommitId) -> Self (op_store.rs:82), RefTarget::absent() (:64, setting absent DELETES the bookmark), as_normal(&self) -> Option<&CommitId> (:104), is_present (:115), has_conflict (:120). Build symbols: RefName::new("main").to_remote_symbol(RemoteName::new("origin")) -> RemoteRefSymbol<'a> (ref_name.rs:311, struct at :367 { name: &RefName, remote: &RemoteName }). Bookmark reaches refs/heads/* only via git::export_refs(mut_repo) -> Result<GitExportStats, GitExportError> (git.rs:967) inside the same tx, pre-commit.

### GitFetch (gt sync fetch)  (/Users/lita/src/jj/lib/src/git.rs:2578)
```rust
pub fn new(mut_repo: &'a mut MutableRepo, git_settings: &'a GitSettings, import_options: &'a GitImportOptions) -> Result<Self, UnexpectedGitBackendError> (:2578); pub fn fetch(&mut self, remote_name: &RemoteName, refspecs: ExpandedFetchRefSpecs, callbacks: RemoteCallbacks, depth: Option<NonZeroU32>, fetch_tags_override: Option<FetchTagsOverride>) -> Result<(), GitFetchError> (:2602); pub fn get_default_branch(&self, remote_name: &RemoteName) -> Result<Option<RefNameBuf>, GitFetchError> (:2670); pub fn import_refs(&mut self) -> Result<GitImportStats, GitImportError> (:2694)
```
0.36 fetch takes ExpandedFetchRefSpecs (NOT a pattern list): build with pub fn expand_fetch_refspecs(remote: &RemoteName, bookmark_expr: StringExpression) -> Result<ExpandedFetchRefSpecs, GitRefExpansionError> (git.rs:2300); StringExpression::exact("main") (str_util.rs:380) or StringExpression::pattern(StringPattern::everything()). fetch only moves refs/remotes/* in .git; import_refs() then copies into the view and clears the fetched list. GitFetch mutably borrows mut_repo — drop it before other repo_mut() use. Whole thing runs inside a transaction; commit it ("fetch from origin").

### GitImportOptions + GitSettings (exact fields)  (/Users/lita/src/jj/lib/src/git.rs:427)
```rust
pub struct GitImportOptions { pub auto_local_bookmark: bool, pub abandon_unreachable_commits: bool, pub remote_auto_track_bookmarks: HashMap<RemoteNameBuf, StringMatcher> } (:427); pub struct GitSettings { pub auto_local_bookmark: bool, pub abandon_unreachable_commits: bool, pub executable_path: PathBuf, pub write_change_id_header: bool } (:86); pub fn from_settings(settings: &UserSettings) -> Result<GitSettings, ConfigGetError> (:95)
```
For gt sync set abandon_unreachable_commits=true (auto-drops commits of deleted/merged remote branches on import) and either auto_local_bookmark=true or explicitly track_remote_bookmark for 'main'. StringMatcher built via StringExpression::to_matcher() (str_util.rs:439). Free import: pub fn import_refs(mut_repo: &mut MutableRepo, options: &GitImportOptions) -> Result<GitImportStats, GitImportError> (git.rs:473).

### git::push_branches (gt submit)  (/Users/lita/src/jj/lib/src/git.rs:2744)
```rust
pub fn push_branches(mut_repo: &mut MutableRepo, git_settings: &GitSettings, remote: &RemoteName, targets: &GitBranchPushTargets, callbacks: RemoteCallbacks) -> Result<GitPushStats, GitPushError>
```
GitBranchPushTargets { pub branch_updates: Vec<(RefNameBuf, BookmarkPushUpdate)> } (git.rs:2729); BookmarkPushUpdate { pub old_target: Option<CommitId>, pub new_target: Option<CommitId> } (refs.rs:204) — old_target is the force-with-lease expectation sourced from get_remote_bookmark(symbol).target.as_normal(); None = 'ref must not exist' (correct for first push of a new stack branch). GitPushStats { pushed: Vec<GitRefNameBuf>, rejected: Vec<(GitRefNameBuf, Option<String>)>, remote_rejected: ... } with all_ok() (git.rs:135-148). View (remote-tracking bookmark + git ref) is updated ONLY if all_ok() (:2769-2784) — on partial failure some refs landed remotely but the view is untouched; refetch to reconcile. RemoteCallbacks is #[non_exhaustive] but #[derive(Default)] (git.rs:2842): use RemoteCallbacks::default(). Run inside a tx; commit it.

### git subprocess auth model (gt submit/sync credentials)  (/Users/lita/src/jj/lib/src/git_subprocess.rs:101)
```rust
fn create_command(&self) -> Command  // Command::new(git_executable_path).args(["-c","core.fsmonitor=false"]).args(["-c","submodule.recurse=false"]).arg("--git-dir").arg(&self.git_dir).env("LC_ALL","C").stdin(Stdio::null()).stderr(Stdio::piped())
```
All fetch/push spawns the real `git` binary (path = GitSettings.executable_path). The child INHERITS gt's process environment (only LC_ALL is overridden); stdin is null so interactive prompts hang/fail. RemoteCallbacks' get_password/get_username_password are NEVER called on this path. Therefore auth = git's own credential machinery: `gh auth setup-git`, a credential.helper, or std::env::set_var("GIT_ASKPASS", script)+GIT_TERMINAL_PROMPT=0 in gt's own process before pushing.

### git::reset_head (colocated HEAD/index, part of write #3)  (/Users/lita/src/jj/lib/src/git.rs:1415)
```rust
pub fn reset_head(mut_repo: &mut MutableRepo, wc_commit: &Commit) -> Result<(), GitResetHeadError>
```
Sets .git HEAD detached at the FIRST PARENT of wc_commit and rebuilds the git index to match — call with the new wc commit inside every tx that moves @, BEFORE tx.commit, alongside export_refs (reference order: cli_util.rs:2094 reset_head, :2103 export_refs, :2107 tx.commit). GitResetHeadError::UpdateHeadRef = concurrent HEAD move; downgrade to warning.

### Programmatic revsets (gt stack computation, no parser)  (/Users/lita/src/jj/lib/src/revset.rs:246)
```rust
pub type ResolvedRevsetExpression = RevsetExpression<ResolvedExpressionState> (:246); pub fn commit(commit_id: CommitId) -> Arc<Self> (:365); pub fn commits(commit_ids: Vec<CommitId>) -> Arc<Self> (:369); pub fn range(self: &Arc<Self>, heads: &Arc<Self>) -> Arc<Self> (:577, 'reachable from heads but not from self'); pub fn evaluate<'index>(self: Arc<Self>, repo: &'index dyn Repo) -> Result<Box<dyn Revset + 'index>, RevsetEvaluationError> (:651)
```
Stack = ResolvedRevsetExpression::commit(main_id).range(&ResolvedRevsetExpression::commit(wc_id)).evaluate(repo.as_ref() as &dyn Repo). Revset::iter yields Result<CommitId, RevsetEvaluationError> in topological order CHILDREN BEFORE PARENTS (revset.rs:3345-3350) — collect and .rev() for bottom-up stack order. RevsetIteratorExt::commits(self, store: &Arc<Store>) adapts to Commit objects (:3386-3391). Works mid-transaction: MutableRepo implements Repo. Simple ancestry check alternative: repo.index().is_ancestor(&a, &b).

### Transaction::commit / write (write #1 only)  (/Users/lita/src/jj/lib/src/transaction.rs:120)
```rust
pub fn commit(self, description: impl Into<String>) -> Result<Arc<ReadonlyRepo>, TransactionCommitError> (:120); pub fn write(mut self, description: impl Into<String>) -> Result<UnpublishedOperation, TransactionCommitError> (:130); pub fn set_is_snapshot(&mut self, is_snapshot: bool) (:115)
```
write() asserts !mut_repo.has_rewrites() (:136-139) — rebase_descendants() first or panic. commit returns the NEW Arc<ReadonlyRepo>; reassign and use repo.op_id().clone() (repo.rs:289) for the subsequent Workspace::check_out / LockedWorkspace::finish. start_transaction(self: &Arc<Self>) -> Transaction is infallible (repo.rs:326). Set is_snapshot(true) on the snapshot tx to mimic jj.

### WorkingCopyFreshness::check_stale (pre-snapshot guard)  (/Users/lita/src/jj/lib/src/working_copy.rs:363)
```rust
pub fn check_stale(locked_wc: &dyn LockedWorkingCopy, wc_commit: &Commit, repo: &ReadonlyRepo) -> Result<WorkingCopyFreshness, OpStoreError>
```
Variants (:347-357): Fresh; Updated(Box<Operation>) => the REPO is behind: repo = repo.reload_at(&op)? and continue; WorkingCopyStale / SiblingOperation => error out for the demo. Call after taking the wc lock, before snapshot.

## Minimal flow
// ---- shared boilerplate (every command) ----
let mut config = StackedConfig::with_defaults();
let mut layer = ConfigLayer::empty(ConfigSource::User);
layer.set_value("user.name", "Lita")?; layer.set_value("user.email", "lita@moment.dev")?;
config.add_layer(layer);
let settings = UserSettings::from_config(config)?;                    // keep ONE instance per process
let mut workspace = Workspace::load(&settings, cwd, &StoreFactories::default(), &default_working_copy_factories())?; // workspace.rs:382
let mut repo: Arc<ReadonlyRepo> = workspace.repo_loader().load_at_head()?; // repo.rs:756

// ---- gt init ----
if !cwd.join(".git").exists() {
    let (workspace, repo) = Workspace::init_colocated_git(&settings, cwd)?;          // workspace.rs:216
} else {
    let (mut workspace, mut repo) = Workspace::init_external_git(&settings, cwd, &cwd.join(".git"))?; // workspace.rs:248
    // op 2: import refs (cli/src/commands/git/init.rs:231-261)
    let git_settings = GitSettings::from_settings(&settings)?;
    let opts = GitImportOptions { auto_local_bookmark: false, abandon_unreachable_commits: false, remote_auto_track_bookmarks: HashMap::new() };
    let mut tx = repo.start_transaction();
    git::import_refs(tx.repo_mut(), &opts)?;                                          // git.rs:473
    git::export_refs(tx.repo_mut())?;                                                 // git.rs:967 (colocated)
    repo = tx.commit("import git refs")?;
    // op 3: move @ onto git HEAD (init.rs:206-216)
    let mut tx = repo.start_transaction();
    git::import_head(tx.repo_mut())?;                                                 // git.rs:847
    if let Some(head_id) = tx.repo().view().git_head().as_normal().cloned() {
        let head = tx.repo().store().get_commit(&head_id)?;
        let wc = tx.repo_mut().check_out(WorkspaceName::DEFAULT.to_owned(), &head)?;  // repo.rs:1514
        git::reset_head(tx.repo_mut(), &wc)?;                                         // git.rs:1415
        repo = tx.commit("import git head")?;
        workspace.check_out(repo.op_id().clone(), None, &wc)?;                        // workspace.rs:428  [writes #2a+#2b]
    } else { repo = tx.commit("import git head")?; }
}

// ---- WRITE ZERO: snapshot (start of create/modify/submit/sync) ----
use pollster::FutureExt as _;
let mut locked_ws = workspace.start_working_copy_mutation()?;                          // workspace.rs:418 (hold across tx.commit!)
let wc_id = repo.view().get_wc_commit_id(WorkspaceName::DEFAULT).unwrap().clone();
let wc_commit = repo.store().get_commit(&wc_id)?;
match WorkingCopyFreshness::check_stale(locked_ws.locked_wc(), &wc_commit, &repo)? {  // working_copy.rs:363
    WorkingCopyFreshness::Fresh => {}
    WorkingCopyFreshness::Updated(op) => { repo = repo.reload_at(&op)?; /* re-read wc_commit */ }
    _ => bail!("stale working copy"),
}
let opts = SnapshotOptions { base_ignores: GitIgnoreFile::empty(), progress: None,
    start_tracking_matcher: &EverythingMatcher, force_tracking_matcher: &NothingMatcher, max_new_file_size: u64::MAX }; // working_copy.rs:212
let (new_tree, _stats) = locked_ws.locked_wc().snapshot(&opts).block_on()?;            // working_copy.rs:118
if new_tree.tree_ids_and_labels() != wc_commit.tree().tree_ids_and_labels() {          // cli_util.rs:1933
    let mut tx = repo.start_transaction(); tx.set_is_snapshot(true);
    let new_wc = tx.repo_mut().rewrite_commit(&wc_commit).set_tree(new_tree).write()?; // repo.rs:954
    tx.repo_mut().set_wc_commit(WorkspaceName::DEFAULT.to_owned(), new_wc.id().clone())?;
    tx.repo_mut().rebase_descendants()?;                                               // repo.rs:1428 — MANDATORY
    git::reset_head(tx.repo_mut(), &new_wc)?; git::export_refs(tx.repo_mut())?;        // write #3, pre-commit
    repo = tx.commit("snapshot working copy")?;                                        // write #1
}
locked_ws.finish(repo.op_id().clone())?;                                               // write #2b — ALWAYS, even if unchanged

// ---- gt create --all -m msg (after snapshot; wc_commit = current @) ----
let mut tx = repo.start_transaction();
let described = tx.repo_mut().rewrite_commit(&wc_commit).set_description(msg).write()?; // give @ the message
tx.repo_mut().set_local_bookmark_target(RefName::new(&branch), RefTarget::normal(described.id().clone())); // repo.rs:1676
let new_wc = tx.repo_mut().check_out(WorkspaceName::DEFAULT.to_owned(), &described)?;  // new empty @ on top
tx.repo_mut().rebase_descendants()?;
git::reset_head(tx.repo_mut(), &new_wc)?; git::export_refs(tx.repo_mut())?;
repo = tx.commit(format!("create {branch}"))?;
workspace.check_out(repo.op_id().clone(), Some(&described.tree()), &new_wc)?;          // files unchanged but state must advance

// ---- gt checkout main ----
let target_id = repo.view().get_local_bookmark(RefName::new("main")).as_normal().unwrap().clone();
let target = repo.store().get_commit(&target_id)?;
let old_wc_tree = wc_commit.tree();
let mut tx = repo.start_transaction();
let new_wc = tx.repo_mut().check_out(WorkspaceName::DEFAULT.to_owned(), &target)?;
git::reset_head(tx.repo_mut(), &new_wc)?; git::export_refs(tx.repo_mut())?;
repo = tx.commit("checkout main")?;
workspace.check_out(repo.op_id().clone(), Some(&old_wc_tree), &new_wc)?;               // writes files + state

// ---- stack enumeration (submit/sync) ----
let main_id = repo.view().get_local_bookmark(RefName::new("main")).as_normal().unwrap().clone();
let revset = ResolvedRevsetExpression::commit(main_id.clone())
    .range(&ResolvedRevsetExpression::commit(wc_id.clone()))
    .evaluate(repo.as_ref())?;                                                         // revset.rs:651
let stack: Vec<CommitId> = revset.iter().try_collect()?; let stack = stack.reverse();  // children-first -> bottom-up

// ---- gt submit ----
let git_settings = GitSettings::from_settings(&settings)?;
let mut tx = repo.start_transaction();
let mut updates = vec![];
for branch in stack_bookmarks {
    let local = tx.repo().view().get_local_bookmark(&branch).as_normal().cloned();
    let sym = branch.to_remote_symbol(RemoteName::new("origin"));
    let old = tx.repo().view().get_remote_bookmark(sym).target.as_normal().cloned();   // lease; None = must-not-exist
    updates.push((branch.to_owned(), BookmarkPushUpdate { old_target: old, new_target: local }));
}
let stats = git::push_branches(tx.repo_mut(), &git_settings, RemoteName::new("origin"),
    &GitBranchPushTargets { branch_updates: updates }, RemoteCallbacks::default())?;   // git.rs:2744
if !stats.all_ok() { /* report stats.rejected / stats.remote_rejected */ }
repo = tx.commit("push stack")?;
// then per-branch GitHub REST/GraphQL: create PR with base = previous stack branch (auth via gh token; jj not involved)

// ---- gt sync ----
let import_opts = GitImportOptions { auto_local_bookmark: false, abandon_unreachable_commits: true, remote_auto_track_bookmarks: HashMap::new() };
let mut tx = repo.start_transaction();
{   let mut fetch = GitFetch::new(tx.repo_mut(), &git_settings, &import_opts)?;        // git.rs:2578
    let refspecs = expand_fetch_refspecs(RemoteName::new("origin"), StringExpression::pattern(StringPattern::everything()))?; // git.rs:2300
    fetch.fetch(RemoteName::new("origin"), refspecs, RemoteCallbacks::default(), None, None)?; // git.rs:2602
    fetch.import_refs()?;                                                              // git.rs:2694
}   // drop GitFetch before touching tx.repo_mut() again
// rebase stack onto new origin/main
let new_main = tx.repo().view().get_remote_bookmark(RefName::new("main").to_remote_symbol(RemoteName::new("origin"))).target.as_normal().unwrap().clone();
tx.repo_mut().set_local_bookmark_target(RefName::new("main"), RefTarget::normal(new_main.clone()));
tx.repo_mut().set_rewritten_commit(old_stack_root_parent, new_main);                    // repo.rs:971, reparent stack base
tx.repo_mut().rebase_descendants_with_options(&RebaseOptions{ empty: EmptyBehavior::AbandonAllEmpty,
    rewrite_refs: RewriteRefsOptions{ delete_abandoned_bookmarks: true }, ..Default::default() }, |_,_| {})?; // drops squash-merged commits + their bookmarks
tx.repo_mut().rebase_descendants()?;
let final_wc_id = tx.repo().view().get_wc_commit_id(WorkspaceName::DEFAULT).unwrap().clone();
let final_wc = tx.repo().store().get_commit(&final_wc_id)?;
git::reset_head(tx.repo_mut(), &final_wc)?; git::export_refs(tx.repo_mut())?;
repo = tx.commit("sync")?;
workspace.check_out(repo.op_id().clone(), Some(&pre_sync_wc_tree), &final_wc)?;

## Gotchas
- MutableRepo::new_commit and CommitBuilder::set_tree take MergedTree BY VALUE (repo.rs:948, commit_builder.rs:88) — not MergedTreeId. Get one from commit.tree() (commit.rs:124) or the snapshot result. Any notes/examples from other jj versions that pass a tree id will not compile on 0.36.
- GitFetch::fetch in 0.36 takes ExpandedFetchRefSpecs (git.rs:2602-2613), NOT a slice of branch patterns — you must call git::expand_fetch_refspecs(remote, StringExpression) (git.rs:2300) first. StringExpression lives in jj_lib::str_util (exact at :380, pattern at :375). Passing an empty refspec set makes fetch a silent no-op (git.rs:2625-2628).
- On an existing git repo, gt init must use Workspace::init_external_git(root, root.join(".git")) — init_colocated_git would gix-init; jj's own CLI branches on colocated_git_repo_path.exists() (cli/src/commands/git/init.rs:160-166). Immediately after init the jj view is empty AND @ is at the root commit: without the 'import git refs' tx and the 'import git head' + check_out tx, gt checkout main will find no bookmarks and the first snapshot will try to absorb the ENTIRE worktree as new files into an empty-tree wc commit.
- push_branches only updates the view's remote-tracking state when GitPushStats::all_ok() (git.rs:2769); rejected refs appear in stats.rejected (lease failure, i.e. remote moved) or stats.remote_rejected — it returns Ok, not Err, so gt submit must check the stats explicitly. The lease value comes from view().get_remote_bookmark(symbol).target: use None only when the branch should not exist remotely yet.
- RemoteCallbacks is #[non_exhaustive] (git.rs:2839) — construct with RemoteCallbacks::default(), never struct-literally. Its credential closures are dead code on the subprocess path: create_command (git_subprocess.rs:101-140) inherits gt's env (only LC_ALL=C overridden) with stdin=null, so HTTPS auth must be pre-arranged via gh auth setup-git / credential.helper / GIT_ASKPASS set on gt's own process.
- Revset iteration order is children-before-parents (revset.rs:3346, doc on trait Revset) — a bottom-up Graphite stack requires collecting and reversing. evaluate() consumes the Arc (self: Arc<Self>) and borrows repo as &dyn Repo for the returned Revset's lifetime ('index), so evaluate, drain into Vec<CommitId>, and drop the Revset before starting mutation.
- LockedWorkspace::finish(op_id) must run even on a no-change snapshot (with the CURRENT op id) and with the NEW op id after any tx that moved @ — and note there is no discard(): dropping a LockedWorkspace after locked_wc().check_out() has already mutated disk files, leaving them attributed to the old tree so the next snapshot silently amends them into the old wc commit.
