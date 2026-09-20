# jj-gt — aka Graphite's gt writting with jj-lib

This is the demo I used for JJCon 2026. 

`jj-gt` reimplements Graphite's stacked-PR CLI on top of [jj-lib] 0.45.1 with a
**colocated** git repo: `jj-gt create` stacks bookmarked commits, `jj-gt submit`
pushes them and opens stacked GitHub PRs, `jj-gt sync` fetches trunk and restacks
— including detecting squash-merged branches by rebasing them to empty.

This is not fully featured and does not talk to Graphite servers at all - although it would not be hard to build if people want that. This has not been extensively tested, as I mostly made this for learning.

I made `--explain` to show when jj specific repository state, disk, and git repository state gets written when using `jj-lib`.

You will need to have rust and cargo installed to build the binary.
