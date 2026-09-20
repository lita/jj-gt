The headers are the talk's three phases. 

**Snapshot** runs before every command and is its own operation (it also adopts anything git moved:
`import_head` before the snapshot, `import_refs` after). 

**Transact** is the command's own transaction. Every jj-lib API call requires a transaction. In a command you can have many transactions.

**Sync/Finalize** is `Workspace::check_out`, which bundles writes 1
and 2 with the new operation id. A command may run several transactions, and
each one gets its own Transact and Sync/Finalize pair.

Every `▸ git refs` line is a write *into* `.git` (`reset_head`, `export_refs`,
`update_intent_to_add`). The reverse direction, `.git → jj` (`import_refs`,
`import_head`), only changes the jj view and shows up as a dimmed `·` note.
Note that `reset_head` rewrites `.git/HEAD` only when `parent(@)` actually
moved — right after `jj-gt init` it is still attached to `main`, and only the
index is rebuilt; the line says which case you got.

`export_refs` compares actual Git refs before and after the call: changed
refs are listed with their old/new targets (or creation/deletion), while a
no-op prints `· export_refs: no Git refs changed`. A snapshot rewrites `@`,
but only moves a branch if a bookmark follows that rewrite or a rebased
descendant. Edits in an unbookmarked scratch `@` leave branch refs alone.
The `git refs` category also includes the index: `update_intent_to_add`
rewrites it to refresh intent-to-add entries, even if none changed; it does
not stage file contents.

## The three writes (plus the one everyone knows)

These are three categories of writes beyond the op-log commit, not a count of
individual writes or lines in the `--explain` output.

| write                  | what                                                    | API                                                     | when                                     |
| ---------------------- | ------------------------------------------------------- | ------------------------------------------------------- | ---------------------------------------- |
| op log                 | operation + view + index + op-heads swap                | `Transaction::commit`                                   | the only thing `commit` does             |
| **1. working copy**    | files on disk                                           | `LockedWorkingCopy::check_out` / `Workspace::check_out` | **after** commit                         |
| **2. workspace state** | `.jj/working_copy`: which op + tree the workspace is at | `LockedWorkspace::finish(op_id)`                        | **after** commit, with the **new** op id |
| **3. git refs**        | colocated `.git`: `refs/heads/*`, `HEAD`, the index     | `git::export_refs` + `git::reset_head`                  | **inside** the tx, **before** commit     |

And **write zero**: snapshotting the working copy into `@` at the start of
every command (its own operation) — skip it and every command sees a stale `@`.

## Where each concept lives

- `src/engine.rs` — the whole story:
  - `Gt::snapshot` = write zero + stale-workspace healing
    (`WorkingCopyFreshness::check_stale`) — and for a colocated repo, write
    zero also means **adopting git's own writes**: `import_head` before the
    snapshot (a raw `git commit` moves HEAD and rewrites files) and
    `import_refs` after, exactly like the jj CLI's per-command sandwich
  - `Gt::finish_tx` = the jj CLI's `finish_transaction`, reimplemented:
    `rebase_descendants` → `reset_head` → `export_refs` → `tx.commit` →
    `workspace.check_out` — the ordering is load-bearing
- `src/commands/init.rs` — `Workspace::init_external_git` does **not** import
  refs; the "import git refs" / "import git head" operations are yours (and
  the head adoption uses `LockedWorkingCopy::reset`, not `check_out`, because
  git already wrote the files)
- `src/commands/create.rs` — describe `@`, bookmark it, fresh scratch `@` on top
- `src/commands/modify.rs` — squash `@` into the branch commit; **abandon** the
  old `@` rather than rebasing it (rebasing would re-apply a conflict
  resolution onto itself); descendants auto-restack
- `src/commands/sync.rs` — `GitFetch` (fetch only touches `refs/remotes/*`;
  `import_refs` is the call that updates the jj view), then a bottom-up
  restack with a replacement map; squash merges are the commits that rebase to
  **empty** on the new trunk
- `src/commands/submit.rs` — `classify_ref_push_action` → `push_refs` with
  force-with-lease; a push mutates the view, so **a push is an op-log write too**
- `src/gitnet.rs` + `src/auth.rs` — jj-lib spawns the real `git` binary and
  does no credential handling: `GitSubprocessOptions.environment` carries a
  `GIT_ASKPASS` helper that answers with the GitHub token
- `src/stack.rs` — the stack is a programmatic revset
  (`commit(root).descendants()`, streamed children-first), no parser needed
- `src/commands/ops.rs` — `jj-gt ops` shows how many operations one "command"
  really is
- `src/commands/undo.rs` — `jj-gt undo` rolls back a whole command _including its
  snapshot_. jj-lib has no undo API and `jj undo` peels one operation at a time,
  so this is pure engine code: every jj-gt transaction is stamped with a per-command
  id (`Transaction::set_attribute`), and undo restores the view from just before
  that whole group. Undo/redo toggle, fully reversible via the op log — and undo
  is itself three writes (it runs the same `finish_tx` epilogue)

The verified API research (exact 0.45.1 signatures with file:line references)
lives in `.claude/research-notes-0.45.md`.

[jj-lib]: https://github.com/jj-vcs/jj/blob/main/docs/technical/architecture.md
