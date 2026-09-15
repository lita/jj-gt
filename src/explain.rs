//! The `--explain` narrator: prints each of the writes jj-gt performs, as it
//! performs them, so the audience can see that committing a jj transaction is
//! only one of several coordinated writes.
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

use jj_lib::backend::CommitId;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::RefTarget;
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

    /// A phase header, e.g. "snapshot working copy" or "jj-gt create".
    pub fn section(&self, title: &str) {
        if self.on {
            eprintln!("{} {}", "──".dimmed(), title.bold().dimmed());
        }
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
