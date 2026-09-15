# jj-gt — stacked PRs on jj-lib

Demo for the JJCon talk **"Three Writes, Not One: Building Developer Tools on jj-lib"**.

`jj-gt` reimplements Graphite's stacked-PR CLI on top of [jj-lib] 0.45.1 with a
**colocated** git repo: `jj-gt create` stacks bookmarked commits, `jj-gt submit`
pushes them and opens stacked GitHub PRs, `jj-gt sync` fetches trunk and restacks
— including detecting squash-merged branches by rebasing them to empty.

The point of the demo: **committing a jj transaction only writes the op log.**
Everything else the `jj` CLI quietly does around every command is the
consumer's job, and `jj-gt` reimplements it visibly. Run any command with
`--explain` to watch the writes happen:

```text
── the three writes — gt create lita/feat-api-...
   ▸ git refs         .git HEAD ⇒ detached at parent of @; index rebuilt
   ▸ git refs         .git/refs/heads/* now mirror jj bookmarks (export_refs)
   ▸ op log           tx.commit("gt create ...") → operation 10c40bc167a2
   ▸ working copy     check_out: 0 added, 0 updated, 0 removed on disk
   ▸ workspace state  .jj/working_copy ← operation 10c40bc167a2
```

## The three writes (plus the one everyone knows)

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
- `src/commands/undo.rs` — `jj-gt undo` rolls back a whole command *including its
  snapshot*. jj-lib has no undo API and `jj undo` peels one operation at a time,
  so this is pure engine code: every jj-gt transaction is stamped with a per-command
  id (`Transaction::set_attribute`), and undo restores the view from just before
  that whole group. Undo/redo toggle, fully reversible via the op log — and undo
  is itself three writes (it runs the same `finish_tx` epilogue)

The verified API research (exact 0.45.1 signatures with file:line references)
lives in `.claude/research-notes-0.45.md`.

## Demo script

```sh
# setup (once): a scratch GitHub repo + auth
gh repo create gt-demo-jjcon --private --add-readme --clone && cd gt-demo-jjcon
export PATH="/path/to/jj-gt/target/debug:$PATH"

jj-gt --explain init              # 3 operations before you've done anything
jj-gt --explain checkout main
$EDITOR src/api.py
jj-gt --explain create --all -m "feat(api): Add new API method for fetching users"
$EDITOR src/api.py
jj-gt create --all -m "feat(api): Add pagination to user fetching"
jj-gt log                         # the colored stack
jj-gt --explain submit            # push with lease + stacked PRs (#2 based on #1)

# amend mid-stack: descendants restack automatically
jj-gt --explain checkout lita/feat-api-add-new-api-method-for-fetching-users
$EDITOR src/api.py
jj-gt --explain modify --all      # "2 descendant(s) auto-restacked"
jj-gt log                         # upstack may show CONFLICT — it materialized, nothing blocked
jj-gt checkout lita/feat-api-add-pagination-to-user-fetching
cat src/api.py                    # jj's conflict markers, on disk
$EDITOR src/api.py                # resolve
jj-gt modify --all                # conflict gone, stack healthy
jj-gt submit                      # force-with-lease re-push

# the finale
gh pr merge 1 --squash            # NOTE: --delete-branch silently fails in a
                                  # colocated repo (detached HEAD confuses gh's
                                  # local-branch cleanup) — delete it below instead
jj-gt --explain sync              # squash-merge detected (rebased to EMPTY), local
                                  # branch deleted, rest of stack onto new main
jj-gt submit                      # retarget the surviving PR to main (sync is
                                  # local-only, like Graphite — submit talks to GitHub)
git push origin --delete lita/feat-api-add-new-api-method-for-fetching-users
                                  # AFTER submit: deleting a PR's base branch before
                                  # retargeting makes GitHub CLOSE the child PR
jj-gt ops                         # every "command" was 1-3 operations

# interop: it's all one repo
jj log                            # the real jj CLI reads jj-gt's writes perfectly

# undo a whole jj-gt command — snapshot included — because the op log makes it easy
jj-gt create --all -m "feat: oops"
jj-gt --explain undo              # branch AND the file vanish (one command, its
                                  # own op group); run `jj-gt undo` again to redo
```

## Build

```sh
cargo build           # produces target/debug/jj-gt
cargo install --path . # installs jj-gt without conflicting with Graphite's gt
```

Auth: `gh auth login` (jj-gt takes the token from `gh auth token`, or set
`GITHUB_TOKEN`). Pushes authenticate via a temporary `GIT_ASKPASS` helper —
no gitconfig changes.

[jj-lib]: https://github.com/jj-vcs/jj/blob/main/docs/technical/architecture.md
