//! jj-gt undo: roll back the last jj-gt command — snapshot included.
//!
//! jj-lib has no undo API; `jj undo` is CLI code that restores the previous
//! operation's view (one op at a time). jj-gt can group a whole command because it
//! stamps the operations it starts with a per-command id (CMD_ID_ATTR): undo finds
//! the newest command group and restores the view from just before it.
//!
//! Since the move to `WorkspaceOperationRunner`, the implicit snapshot / git
//! import operations are created inside jj-lib and carry no cmd id (the runner
//! exposes no attribute hook, and its `args` attribute hard-codes `jj` as the
//! program name). Undo therefore folds runner-created ops that directly precede a
//! jj-gt command into that command's group, and skips this invocation's own ops
//! by starting from the op head recorded at load time (`Gt::initial_op_id`).
//!
//! Because an undo is itself a command group, running `jj-gt undo` again restores
//! to *its* parent — i.e. undo and redo toggle. Simple, fully reversible, and a
//! nice demo of why the op log makes this tractable. (The real jj CLI walks
//! strictly backward on repeat and has a separate `redo`; we favor the toggle.)
//!
//! Like the jj CLI, the restored view keeps the CURRENT git_refs/git_heads
//! records — they describe what is actually in .git, and the epilogue's
//! export/reset needs truthful expectations. And, true to the talk, undo is
//! itself three writes: it runs through the same finish_tx epilogue.

use anyhow::{Result, bail};
use jj_lib::object_id::ObjectId as _;
use jj_lib::operation::Operation;
use owo_colors::OwoColorize;
use pollster::FutureExt as _;

use crate::engine::{CMD_ID_ATTR, Gt, RUNNER_OP_DESCRIPTIONS};
use crate::util::short;

/// Encoded in the op description so a human reading `jj op log` / `jj-gt ops` can
/// see what an undo restored to. Not parsed for logic.
const UNDO_DESC_PREFIX: &str = "jj-gt undo: restore to operation ";

fn single_parent(op: &Operation) -> Result<Operation> {
    let mut parents = op.parents().block_on()?;
    match parents.len() {
        0 => bail!("nothing left to undo (reached the root operation)"),
        1 => Ok(parents.pop().expect("len checked")),
        _ => bail!("cannot undo across a merge operation — use `jj op restore` instead"),
    }
}

fn cmd_id_of(op: &Operation) -> Option<String> {
    op.metadata().attributes.get(CMD_ID_ATTR).cloned()
}

/// An operation the runner created on its own (snapshot, import git head/refs).
/// Indistinguishable from the same ops made by the real `jj` CLI — the runner
/// records `jj` as argv[0] regardless of the actual binary.
fn is_runner_op(op: &Operation) -> bool {
    let metadata = op.metadata();
    metadata.is_snapshot || RUNNER_OP_DESCRIPTIONS.contains(&metadata.description.as_str())
}

pub fn run(gt: &mut Gt) -> Result<()> {
    let ex = gt.explain;
    // Snapshot first, as always. Its ops (if any) are skipped below, so undo
    // always targets the previous command.
    gt.snapshot()?;

    // Newest operation this undo invocation didn't create: the head as of
    // load. (The runner's ops carry no cmd id, so we can't recognise them by
    // attribute the way jj-gt used to.)
    let mut op = gt
        .repo()
        .loader()
        .load_operation(&gt.initial_op_id)
        .block_on()?;

    // Walk back over the whole command group (ops sharing one cmd id, plus the
    // runner's implicit ops right before them). Ops with no cmd id (raw jj, or
    // jj-gt init) are undone one at a time. The target is the operation just
    // before the group — for a jj-gt-undo group, that's the state the previous
    // undo rewound from, which is why undo/redo toggles.
    let group_id = cmd_id_of(&op);
    let mut undone: Vec<String> = Vec::new();
    let target = loop {
        undone.push(op.metadata().description.clone());
        let parent = single_parent(&op)?;
        let same_group = match (&group_id, cmd_id_of(&parent)) {
            (Some(gid), Some(pid)) => *gid == pid,
            (Some(_), None) => is_runner_op(&parent),
            (None, _) => false,
        };
        if same_group {
            op = parent;
        } else {
            break parent;
        }
    };

    for desc in &undone {
        if desc.contains("push") {
            eprintln!(
                "{} undoing a push only rewinds jj's record of the remote — \
                 GitHub still has the pushed refs; run `jj-gt sync` before the next submit",
                "!".yellow().bold()
            );
        }
    }
    ex.section("jj-gt undo: restore the view from before the last command");
    for desc in &undone {
        ex.note(&format!("undoing operation: {desc}"));
    }

    // Restore the view — repo state from the target op, but git_refs/git_heads
    // stay CURRENT (they record what's really in .git; see module docs).
    let target_view = target.view().block_on()?;
    let mut tx = gt.start_tx();
    let current = tx.base_repo().view().store_view().clone();
    let restored = target_view.store_view();
    let new_view = jj_lib::op_store::View {
        head_ids: restored.head_ids.clone(),
        local_bookmarks: restored.local_bookmarks.clone(),
        local_tags: restored.local_tags.clone(),
        remote_views: restored.remote_views.clone(),
        git_refs: current.git_refs,
        git_heads: current.git_heads,
        wc_commit_ids: restored.wc_commit_ids.clone(),
    };
    tx.repo_mut().set_view(new_view);
    gt.finish_tx(tx, &format!("{UNDO_DESC_PREFIX}{}", target.id().hex()))?;

    println!(
        "Undid {} operation(s), restored to {} ({})",
        undone.len(),
        short(&target.id().hex()),
        target.metadata().description
    );
    if let Err(e) = crate::commands::log::print_stack(gt) {
        eprintln!("jj-gt: note: could not render the stack: {e:#}");
    }
    Ok(())
}
