# Graphite CLI API integration

Investigated on 2026-09-21 using Graphite's
[authentication documentation](https://graphite.com/docs/configure-cli), the
published [`@withgraphite/graphite-cli` 1.7.13 package](https://www.npmjs.com/package/@withgraphite/graphite-cli/v/1.7.13)
(readable bundled JavaScript), and the installed 1.8.6 executable (the same
API host and endpoint names are present). These are internal CLI endpoints,
not a published stable API. No upstream implementation code is vendored here.

`gt auth --token` persists a Graphite CLI token and calls `check-auth`. Modern
`gt` defaults to `~/.config/graphite/auth`; older versions store `authToken` in
`~/.config/graphite/user_config`. Graphite also supports profiles and
`GRAPHITE_AUTH_TOKEN`. `jj-gt` uses its own single-account `~/.jj-gt/config`,
validates before replacing a saved token, and supports the environment override.

Requests use `https://api.graphite.com/v1`, JSON bodies, and the header
`Authorization: token <GRAPHITE-TOKEN>`:

| Operation | POST endpoint | Request / response |
| --- | --- | --- |
| Validate auth | `/graphite/check-auth` | Optional `repoOwner`, `repoName`; returns `githubLogin`, optional `canSubmitPrs` |
| Refresh PRs | `/graphite/cli/pull-request-info` | `repoOwner`, `repoName`, `prNumbers`, `prHeadRefNames`, `trunkBranchNames`, `consistent`; returns `result: {status: "ok", prs: [...]}` or `{status: "error", message}` |
| Submit stack | `/graphite/submit/pull-requests` | `repoOwner`, `repoName`, `trunkBranchName`, `useWebSubmit: false`, `prs`; returns a `prs` array with per-branch results |

A submission contains `action` (`create` or `update`), `head`, `headSha`, `base`,
and `baseSha`. Creates include `title` and `body`; updates include `prNumber` and
omit metadata fields to preserve the author's edits. Success results contain
`status` (`created` or `updated`), `head`, `prNumber`, `prURL`, and optional
`warnings`; errors contain `status: "error"`, `head`, and `error`. The server
manages Graphite's stack metadata; jj-gt's GitHub-only stack body is not applied.

If `check-auth` returns without permission to submit to the repository,
`submit` announces a fallback and uses the existing direct GitHub PR workflow.
It checks for GitHub credentials before pushing. Graphite request failures and
partial submission errors still surface normally; the fallback is selected
before any PR submission occurs.

PR lookups use uppercase `OPEN`, `CLOSED`, `MERGED` states. To recognize a merge
whose final tree differs from the branch, sync checks the latest version's
`headSha` and confirms `mergeCommitSha` is in fetched trunk history. Missing
version/merge metadata falls back to the existing Git merge detection. Closed
PRs do not cause local branch deletion. Sync does not submit PRs; a later submit
pushes rewritten heads and retargets bases.

`cargo test --test graphite_api` exercises requests against a local HTTP stub
and uses local bare Git remotes, without real credentials or remote writes.
`JJ_GT_GRAPHITE_API_URL` overrides the API root for these tests (include `/v1`).
HTTPS is required except for localhost. Automatic redirects are disabled and
requests have a 60-second timeout. This test suite does not replace a live
end-to-end check with an authorized Graphite account and repository.
Fallback tests also set `JJ_GT_GITHUB_API_URL` to a local GitHub API stub, with
the same HTTPS/localhost restriction.
