# jj-gt — aka Graphite's gt but with jj-lib

This is the demo I used for JJCon 2026. 

`jj-gt` reimplements Graphite's stacked-PR CLI on top of [jj-lib](https://crates.io/crates/jj-lib) with a
**colocated** git repo: `jj-gt create` stacks bookmarked commits, `jj-gt submit`
pushes them and opens stacked GitHub PRs, `jj-gt sync` fetches trunk and restacks
— including detecting squash-merged branches by rebasing them to empty.

Optional Graphite Integration: Authenticate with `jj-gt auth --token` to create and update stacks through Graphite. 
Without a Graphite token, `submit` uses GitHub directly.

I added `--explain` to show when' jj-lib' writes jj-specific repository state, disk, and git repository state.

You need Rust and Cargo installed to build the binary.

You do not need `jj` installed to use this tool, but you do need git and working
Git credentials to fetch and push. 

It also works alongside `jj`; inspect the shared operation log with `jj op log`.

## Graphite authentication and stacks

Get a CLI token from [Graphite](https://app.graphite.com/activate), then run:

```sh
jj-gt auth --token <GRAPHITE-TOKEN>  # works outside a repository too
jj-gt auth                         # verify the active token
jj-gt submit                       # push branches, create/update Graphite stack PRs
jj-gt sync                         # refresh PR status, fetch, remove merged branches, restack
jj-gt submit                       # push the restack and retarget surviving PRs
```

`auth` validates the token before saving it in **`~/.jj-gt/config`**, a JSON file
with an `authToken` field. The directory is private (`0700`) and the file is
readable and writable only by you (`0600`). Tokens never go into repository
state (`.jj/gt.json`). `GRAPHITE_AUTH_TOKEN` overrides the saved token, including
for CI. Existing Graphite CLI config files are not read or modified; authenticate
`jj-gt` once with the same token you would pass to `gt auth`.

When authenticated, `submit` uses Graphite's stack submission API, prints its PR
URLs, and preserves existing PR titles and descriptions. New PR descriptions
come from the commit message after its first line. Successful PR numbers are
saved even if another PR in the stack fails. `sync` reads Graphite PR status and
restacks locally; it does not push or create PRs. Confirmed merges only remove a
branch when its submitted head matches the local commit and the merge is in the
fetched trunk; the existing Git-based merge detection also remains active.

If Graphite reports that it cannot submit PRs to the repository, `submit`
automatically falls back to creating or updating stacked PRs directly on GitHub.
The fallback uses `GITHUB_TOKEN` or `gh auth token`; run `gh auth login` if needed.
Your saved Graphite token stays configured for repositories it can access.

Graphite tokens authenticate **Graphite API calls**. Git fetch/push still use
SSH, your Git credential helper, or a GitHub token from `GITHUB_TOKEN` / `gh auth
token`. With SSH or a working helper, Graphite mode does not require `gh` or a
GitHub API token. Direct GitHub PR submission does require a GitHub API token.

This integration uses Graphite's internal CLI endpoints, which may change.
See [API implementation notes](docs/graphite-api.md) for the inspected versions,
request format, and local testing setup.

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
jj-gt submit                      # retarget the surviving PR to main (sync does
                                  # not push; submit updates PRs via Graphite or GitHub)
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

For direct GitHub PR submission: `gh auth login` (jj-gt takes the token from
`gh auth token`, or set `GITHUB_TOKEN`). For Graphite, use `jj-gt auth --token`
as described above.
