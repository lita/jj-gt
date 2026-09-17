//! The `--explain` narrator: prints each of the writes jj-gt performs, as it
//! performs them, so the audience can see that committing a jj transaction is
//! only one of several coordinated writes.
//!
//! Every mutating command is narrated in the talk's three phases:
//!   Snapshot        — save current file edits into @, recording an operation if
//!                     the tree changed (plus adopting git's own HEAD/ref moves)
//!   Transact        — create or rewrite commits, choose the next @, and publish
//!                     a new operation (a command may run several)
//!   Sync/Finalize   — check_out makes the files match that operation's @, then
//!                     its id is recorded in .jj/working_copy
//!
//! Vocabulary used throughout the demo:
//!   op log          — the transaction commit (the write everyone knows about)
//!   working copy    — files on disk (write 1)
//!   workspace state — .jj/working_copy: which op + tree the workspace is at (write 2)
//!   git refs        — the colocated .git: refs/heads/*, HEAD, the index (write 3)
//!
//! Direction convention: a `▸ git refs` line is always jj → .git (reset_head,
//! export_refs, update_intent_to_add). The reverse direction, .git → jj
//! (import_refs, import_head), only updates the jj view and is narrated as a
//! dimmed `·` note, never as a write.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use jj_lib::backend::CommitId;
use jj_lib::git;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::RefTarget;
use jj_lib::repo::Repo;
use owo_colors::OwoColorize;

use crate::util::short;

#[derive(Clone, Copy)]
pub struct Explain {
    pub on: bool,
}

impl Explain {
    pub fn new(on: bool) -> Self {
        Explain { on }
    }

    /// A free-form section header, e.g. "jj-gt init: colocating jj onto the git repo".
    pub fn section(&self, title: &str) {
        if self.on {
            eprintln!("{} {}", "──".dimmed(), title.bold().dimmed());
        }
    }

    /// One of the talk's three phase headers (Snapshot · Transact ·
    /// Sync/Finalize), with the one-line definition from the slides.
    fn phase(&self, title: &str, what: &str) {
        if self.on {
            eprintln!("{} {}  {}", "──".dimmed(), title.bold(), what.dimmed());
        }
    }

    /// Phase 1: fold the files on disk into @.
    pub fn phase_snapshot(&self) {
        self.phase(
            "Snapshot",
            "save current file edits into @, recording an operation if the tree changed",
        );
    }

    /// Phase 2: one transaction, from start_transaction to tx.commit.
    pub fn phase_transact(&self) {
        self.phase(
            "Transact",
            "create or rewrite commits, choose the next @, and publish a new operation",
        );
    }

    /// Phase 3: bring the working copy in line with the operation just published.
    pub fn phase_finalize(&self) {
        self.phase(
            "Sync/Finalize",
            "check_out makes the files match that operation's @, then record its id in .jj/working_copy",
        );
    }

    /// Free-form context line.
    pub fn note(&self, msg: &str) {
        if self.on {
            eprintln!("   {} {}", "·".dimmed(), msg.dimmed());
        }
    }

    /// The op-log write: `tx.commit(desc)` produced operation `op_hex`.
    pub fn op_log(&self, desc: &str, op_hex: &str) {
        if self.on {
            eprintln!(
                "   {} {}  tx.commit({:?}) → operation {}",
                "▸".magenta(),
                "op log         ".magenta().bold(),
                desc,
                (&op_hex[..12.min(op_hex.len())]).magenta()
            );
        }
    }

    /// Write 1: files on disk.
    pub fn working_copy(&self, added: u32, updated: u32, removed: u32) {
        if self.on {
            eprintln!(
                "   {} {}  check_out: {} added, {} updated, {} removed on disk",
                "▸".cyan(),
                "working copy   ".cyan().bold(),
                added,
                updated,
                removed
            );
        }
    }

    /// Write 2: .jj/working_copy now records this operation.
    pub fn workspace_state(&self, op_hex: &str) {
        if self.on {
            eprintln!(
                "   {} {}  .jj/working_copy ← operation {}",
                "▸".blue(),
                "workspace state".blue().bold(),
                (&op_hex[..12.min(op_hex.len())]).blue()
            );
        }
    }

    /// Write 3: colocated git refs / HEAD.
    pub fn git_refs(&self, what: &str) {
        if self.on {
            eprintln!(
                "   {} {}  {}",
                "▸".green(),
                "git refs       ".green().bold(),
                what
            );
        }
    }

    /// Read actual Git refs, including symbolic HEAD, only for --explain.
    /// The jj view can be stale or already agree with Git, so a view diff
    /// alone cannot tell us whether export_refs changed anything on disk.
    pub fn capture_git_refs(&self, repo: &dyn Repo) -> Option<BTreeMap<String, String>> {
        if !self.on {
            return None;
        }
        let read = || -> Result<BTreeMap<String, String>> {
            let git_repo = git::get_git_repo(repo.store())?;
            let mut refs = BTreeMap::new();
            for reference in git_repo.references()?.all()? {
                let reference = reference.map_err(anyhow::Error::from_boxed)?;
                refs.insert(
                    reference.name().as_bstr().to_string(),
                    reference.target().into_owned().to_string(),
                );
            }
            // The iterator excludes pseudo-refs. Export can detach HEAD when
            // it moves the branch HEAD was attached to.
            if let Some(head) = git_repo.try_find_reference("HEAD")? {
                refs.insert("HEAD".to_string(), head.target().into_owned().to_string());
            }
            Ok(refs)
        };
        match read() {
            Ok(refs) => Some(refs),
            Err(err) => {
                self.note(&format!(
                    "cannot inspect Git ref changes for --explain: {err}"
                ));
                None
            }
        }
    }

    /// Narrate observed ref changes, rather than treating an export call as
    /// proof that refs moved. Failed exports leave their refs unchanged.
    pub fn exported_refs(
        &self,
        before: &BTreeMap<String, String>,
        after: &BTreeMap<String, String>,
    ) {
        if before == after {
            self.note("export_refs: no Git refs changed");
            return;
        }
        let display_target = |target: &String| {
            if target.starts_with("ref: ") {
                target.clone()
            } else {
                short(target)
            }
        };
        for name in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
            let old = before.get(name);
            let new = after.get(name);
            if old == new {
                continue;
            }
            let what = match (old, new) {
                (None, Some(new)) => format!("created at {}", display_target(new)),
                (Some(old), Some(new)) => {
                    format!("{} → {}", display_target(old), display_target(new))
                }
                (Some(old), None) => format!("deleted (was {})", display_target(old)),
                (None, None) => unreachable!(),
            };
            self.git_refs(&format!(".git/{name}: {what} (export_refs)"));
        }
    }

    /// Write 3, the HEAD + index half: narrate what `git::reset_head` just
    /// did. `old_head` is the view's git_head captured BEFORE the call;
    /// `parent` is @'s first parent. reset_head rewrites .git/HEAD only when
    /// those differ (a parent of root means "unborn" HEAD); the index is
    /// rebuilt from parent(@)'s tree either way.
    pub fn reset_head(&self, old_head: &RefTarget, parent: &CommitId, root_id: &CommitId) {
        if !self.on {
            return;
        }
        let new_head = (parent != root_id).then(|| parent.clone());
        let head_moved = *old_head != RefTarget::resolved(new_head.clone());
        let was = old_head
            .as_normal()
            .map(|id| short(&id.hex()))
            .unwrap_or_else(|| "unborn".to_string());
        let what = match (head_moved, new_head) {
            (true, Some(id)) => format!(
                ".git/HEAD ← detached at {} (was {was}); .git/index ← tree of parent(@) (reset_head)",
                short(&id.hex())
            ),
            (true, None) => format!(
                ".git/HEAD ← unborn, parent(@) is root (was {was}); .git/index ← empty (reset_head)"
            ),
            (false, Some(id)) => format!(
                ".git/index ← tree of parent(@) {}; HEAD unchanged, already there (reset_head)",
                short(&id.hex())
            ),
            (false, None) => {
                ".git/index ← empty; HEAD unchanged, still unborn (reset_head)".to_string()
            }
        };
        self.git_refs(&what);
    }

    /// Network I/O (git subprocess, GitHub API) — not one of the three writes,
    /// but worth narrating.
    pub fn net(&self, what: &str) {
        if self.on {
            eprintln!("   {} {}  {}", "▸".yellow(), "network        ".yellow().bold(), what);
        }
    }
}
