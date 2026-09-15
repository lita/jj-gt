#!/usr/bin/env bash
# Run from any directory: bash /path/to/jj-gt/docs/commit-loss-demo.sh
set -Eeuo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
for required in cargo git gt jj; do
  if ! type -P "$required" >/dev/null; then
    printf 'Missing prerequisite: %s\n' "$required" >&2
    exit 1
  fi
done
GRAPHITE_GT=$(type -P gt)
JJ_GT="$REPO/target/debug/jj-gt"
export GIT_PAGER=cat PAGER=cat GIT_TERMINAL_PROMPT=0 LC_ALL=C

DEMO=$(mktemp -d "${TMPDIR:-/tmp}/gt-vs-jj-gt.XXXXXX")
LOG="$DEMO/demo.log"
touch "$LOG"
trap 'printf "Demo failed near line %s. Log: %s\n" "$LINENO" "$LOG" >&2; tail -n 30 "$LOG" >&2' ERR

quiet() { "$@" >> "$LOG" 2>&1; }
step() { printf '\n%s\n' "$1"; }

setup_case() {
  local case_dir="$DEMO/$1"
  mkdir -p "$case_dir"
  git init --bare -q -b main "$case_dir/origin.git"
  git clone -q "$case_dir/origin.git" "$case_dir/alice"
  (
    cd "$case_dir/alice"
    git config user.name Alice
    git config user.email alice@example.invalid
    git config commit.gpgsign false
    printf 'base\n' > README.md
    git add README.md
    git commit -qm Base
    git push -q origin main
    git checkout -qb feature
    printf 'A1\n' > alice.txt
    git add alice.txt
    git commit -qm A1
    git push -qu origin feature
  )
  git clone -q -b feature "$case_dir/origin.git" "$case_dir/bob"
  git -C "$case_dir/bob" config user.name Bob
  git -C "$case_dir/bob" config user.email bob@example.invalid
  git -C "$case_dir/bob" config commit.gpgsign false
}

# Bob adds a separate file so there is no textual merge conflict.
bob_push() {
  local case_dir="$DEMO/$1"
  printf 'B1: Bob was here\n' > "$case_dir/bob/bob.txt"
  git -C "$case_dir/bob" add bob.txt
  git -C "$case_dir/bob" commit -qm B1
  git -C "$case_dir/bob" push -q origin feature
}

# Graphite submit SUBSTITUTE, as in the source reproduction.
# Its expected SHA comes from Alice's current Git remote-tracking ref.
submit_substitute() {
  git push \
    "--force-with-lease=refs/heads/feature:$(git rev-parse origin/feature)" \
    --progress --atomic origin "$(git rev-parse feature):refs/heads/feature"
}


main() {
  echo 'Shared branch: Graphite reproduction vs. jj-gt'
  echo 'Graphite uses a Git push substitute; jj-gt uses its real submit command.'
  printf 'Repositories and logs: %s\n' "$DEMO"
  printf 'Graphite version: '
  "$GRAPHITE_GT" --version

  step 'Setting up two local demos (including cargo build)...'
  quiet cargo build --manifest-path "$REPO/Cargo.toml" --target-dir "$REPO/target"
  quiet setup_case graphite
  quiet setup_case jj-gt

  step '1. Graphite: Bob pushes B1. Alice amends her older A1.'
  cd "$DEMO/graphite/alice"
  quiet "$GRAPHITE_GT" init --trunk main --no-interactive
  quiet "$GRAPHITE_GT" track feature --parent main --no-interactive
  quiet bob_push graphite
  BOB_GT=$(git -C "$DEMO/graphite/bob" rev-parse feature)
  printf 'A1-prime: Alice amended her work\n' >> alice.txt
  quiet "$GRAPHITE_GT" modify --all --message A1-prime --no-interactive

  if submit_substitute > "$DEMO/stale-lease.log" 2>&1; then
    echo 'Unexpected: stale lease accepted' >&2
    return 1
  fi
  case "$(cat "$DEMO/stale-lease.log")" in
    *'stale info'*) ;;
    *) cat "$DEMO/stale-lease.log" >&2; return 1 ;;
  esac
  REMOTE_GT="$DEMO/graphite/origin.git"
  test "$(git --git-dir="$REMOTE_GT" rev-parse feature)" = "$BOB_GT"
  echo 'Before fetch: push rejected (stale lease). Bob is safe.'

  step '2. Alice runs git fetch, then retries the same push substitute.'
  quiet git fetch origin
  test "$(git rev-parse origin/feature)" = "$BOB_GT"
  quiet submit_substitute
  test "$(git --git-dir="$REMOTE_GT" rev-parse feature)" = "$(git rev-parse feature)"
  git --git-dir="$REMOTE_GT" cat-file -e "$BOB_GT^{commit}"
  if git --git-dir="$REMOTE_GT" merge-base --is-ancestor "$BOB_GT" feature; then
    echo 'Unexpected: Bob is still in remote ancestry' >&2
    return 1
  else
    test "$?" -eq 1
  fi
  test -z "$(git --git-dir="$REMOTE_GT" ls-tree --name-only feature -- bob.txt)"
  echo 'LOST: remote feature no longer contains B1 or bob.txt'

  step '3. jj-gt: same Bob commit, same Alice edit, same git fetch.'
  cd "$DEMO/jj-gt/alice"
  quiet "$JJ_GT" init
  quiet bob_push jj-gt
  BOB_JJ=$(git -C "$DEMO/jj-gt/bob" rev-parse feature)
  printf 'A1-prime: Alice amended her work\n' >> alice.txt
  quiet "$JJ_GT" --explain modify --all
  ALICE_JJ=$(git rev-parse feature)
  quiet git fetch origin
  test "$(git rev-parse origin/feature)" = "$BOB_JJ"

  if GITHUB_TOKEN=local-demo "$JJ_GT" --explain submit > "$DEMO/jj-submit.log" 2>&1; then
    echo 'Unexpected: divergent submit succeeded' >&2
    return 1
  fi
  case "$(cat "$DEMO/jj-submit.log")" in
    *'bookmark feature is conflicted — resolve before submitting'*) ;;
    *) cat "$DEMO/jj-submit.log" >&2; return 1 ;;
  esac
  echo 'Submit rejected: bookmark feature is conflicted — resolve before submitting'
  # Inspect only AFTER submit so jj does not import refs for jj-gt.
  jj --no-pager bookmark list feature

  REMOTE_JJ="$DEMO/jj-gt/origin.git"
  test "$(git --git-dir="$REMOTE_JJ" rev-parse feature)" = "$BOB_JJ"
  git --git-dir="$REMOTE_JJ" merge-base --is-ancestor "$BOB_JJ" feature
  test "$(git --git-dir="$REMOTE_JJ" show feature:bob.txt)" = 'B1: Bob was here'
  test "$(git show "$ALICE_JJ:alice.txt")" = "$(printf 'A1\nA1-prime: Alice amended her work')"
  echo 'SAFE: remote feature still contains B1 and bob.txt; Alice retains her edit'

  step 'Done. All checks passed.'
  printf 'Inspect the repositories and logs in: %s\n' "$DEMO"
}

# No prompts or pagers should consume terminal input, even during a paste.
main </dev/null
