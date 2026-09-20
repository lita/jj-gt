# jj-gt — aka Graphite's gt writting with jj-lib

This is the demo I used for JJCon 2026. 

`jj-gt` reimplements Graphite's stacked-PR CLI on top of [jj-lib] 0.45.1 with a
**colocated** git repo: `jj-gt create` stacks bookmarked commits, `jj-gt submit`
pushes them and opens stacked GitHub PRs, `jj-gt sync` fetches trunk and restacks
— including detecting squash-merged branches by rebasing them to empty.

This is not fully featured and does not talk to Graphite servers at all - although it would not be hard to build if people want that. This has not been extensively tested, as I mostly made this for learning.

I made `--explain` to show when jj specific repository state, disk, and git repository state gets written when using `jj-lib`.

You will need to have rust and cargo installed to build the binary.

You do not need `jj` installed to use this tool but I find it helps to use along side it and read the jj op log via `jj op log`

## Switching branches

`jj-gt checkout` accepts bookmark names, full commit IDs, and unambiguous
commit ID prefixes. `jj-gt log` shows the current stack, other available
bookmarks, and saved unbookmarked work with IDs you can check out.

Switching away with edits snapshots them first and prints a recovery command:

```sh
jj-gt checkout main       # prints: Saved unbookmarked work at <id>; ...
jj-gt log                 # find the snapshot under Saved work
jj-gt checkout <id>       # resume that snapshot, with its edits intact
jj-gt create -m "My work"  # turn the recovered edits into a bookmarked branch
```

Checkout resumes a nonempty, unbookmarked tip directly. For bookmarked or
historical commits, it opens a fresh working-copy commit on top, as before.
Bookmark names take precedence over commit ID prefixes; ambiguous prefixes
require a longer ID.

## Amending the parent branch

After editing files on a branch, run `jj-gt modify` to fold the working-copy
changes into its parent branch commit and start a fresh empty working-copy
commit on top. The branch keeps its name and commit message, and any commits
stacked above it are automatically restacked.

All changes are included without staging; `--all` is accepted for Graphite
compatibility but is optional. An empty working copy has nothing to amend.
If the working copy sits directly on trunk, use `jj-gt create -m "My work"`
to start a branch first.


## Demo script

For a separate shared-branch commit-loss comparison with Graphite, see
[the commit-loss demo](docs/commit-loss-demo.md).

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
