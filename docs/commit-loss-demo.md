# Shared-branch commit loss: Graphite vs. jj-gt

## Run it

From the jj-gt repository root, run this **single command** in your usual terminal
(zsh or Bash both work):

```sh
bash docs/commit-loss-demo.sh
```

The script builds jj-gt, creates temporary local repositories, and runs both
cases automatically. It prints three short stages and checks the results. Setup
output goes to a log; the script prints its location. Each run starts fresh.

Requires Git, Graphite's `gt` on your PATH, `jj`, and Cargo. No GitHub auth is
needed. You do not need to start an interactive Bash shell or paste setup code.
If you are still at a `>` prompt from the previous demo, press **Ctrl-C** first.

## What to say

1. **Bob pushes a commit. Alice amends her older copy.** Her first push is
   rejected because the remote moved. Bob's work is safe.
2. **Alice fetches, then retries.** The refreshed lease lets her overwrite Bob's
   commit. His file disappears from the remote branch.
3. **Repeat with jj-gt.** It records the two histories as a conflicted bookmark
   and refuses to submit. Bob's commit and file stay on the remote; Alice keeps
   her edit locally.

Expected highlights:

```text
Before fetch: push rejected (stale lease). Bob is safe.
LOST: remote feature no longer contains B1 or bob.txt
Submit rejected: bookmark feature is conflicted — resolve before submitting
SAFE: remote feature still contains B1 and bob.txt; Alice retains her edit
Done. All checks passed.
```

**Takeaway:** A push lease checks whether the remote changed since the last
observation. jj-gt also records the divergent histories and requires reconciliation
before publishing. The edits touch different files; this is a branch conflict,
not a file-content conflict.

## What the comparison covers

The [script](commit-loss-demo.sh) is a short variant of scenarios 1 and 2 in
`/Users/lita/src/gt-commit-loss-demo/repro.sh`, with Alice amending before fetching.
Bob uses plain Git in both cases.

The Graphite half uses real `gt init`, `gt track`, and `gt modify`, followed by
**the original reproduction's Git push substitute for `gt submit`**. It reproduces
the refreshed-lease loss mechanism; it does not test authenticated Graphite
submission. The jj-gt half calls its **actual `submit`**, which rejects the
conflicted bookmark before pushing or calling GitHub.

“Lost” means absent from the remote branch's ancestry and tree. Bob's clone still
contains his commit. The script checks both ancestry and file content directly
against each bare remote.

For the implementation, see ref import in [engine.rs](../src/engine.rs) and the
conflicted-bookmark check in [submit.rs](../src/commands/submit.rs).
