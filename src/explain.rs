//! The `--explain` narrator: prints each of the writes jj-gt performs, as it
//! performs them, so the audience can see that committing a jj transaction is
//! only one of several coordinated writes.
//!
//! Vocabulary used throughout the demo:
//!   op log          — the transaction commit (the write everyone knows about)
//!   working copy    — files on disk (write 1)
//!   workspace state — .jj/working_copy: which op + tree the workspace is at (write 2)
//!   git refs        — the colocated .git: refs/heads/*, HEAD, the index (write 3)

use owo_colors::OwoColorize;

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

    /// Network I/O (git subprocess, GitHub API) — not one of the three writes,
    /// but worth narrating.
    pub fn net(&self, what: &str) {
        if self.on {
            eprintln!("   {} {}  {}", "▸".yellow(), "network        ".yellow().bold(), what);
        }
    }
}
