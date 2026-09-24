//! jj-gt split: split the current branch into several stacked branches.
//!
//! Three strategies, mirroring Graphite's `gt split`:
//!
//!   --by-commit  pick split points between the branch's existing commits;
//!                purely a bookmark operation, no commit is rewritten.
//!   --by-hunk    `git add -p`-style: pick hunks for each new branch, bottom-up;
//!                the leftovers stay on the current branch, which ends up on top.
//!   --by-file    move the changes to files matching git-style pathspecs into a
//!                new parent branch; the only form that runs non-interactively.
//!
//! jj model: the "current branch" is the bookmarked parent of the scratch @.
//! New branches are fresh commits built with MergedTreeBuilder on top of the
//! branch's base; the current branch is then rewritten to sit on top of them
//! (same change id, same tree — so its diff shrinks to the remainder) and
//! rebase_descendants carries @ and any upstack along.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Result, bail};
use jj_lib::backend::CommitId;
use jj_lib::commit::Commit;
use jj_lib::merged_tree::MergedTree;
use jj_lib::merged_tree_builder::MergedTreeBuilder;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::RefTarget;
use jj_lib::ref_name::{RefName, RefNameBuf};
use jj_lib::repo::Repo as _;
use owo_colors::OwoColorize;
use pollster::FutureExt as _;

use crate::engine::{Gt, summarize};
use crate::patch::{FileChange, tree_changes};
use crate::pathspec::{Pathspec, any_matches};
use crate::prompt::Prompt;
use crate::util::{branch_user_prefix, short, slugify};

pub struct Options {
    pub by_commit: bool,
    pub by_hunk: bool,
    pub by_file: Vec<String>,
    pub message: Option<String>,
}

enum Mode {
    Commit,
    Hunk,
    File(Vec<String>),
}

/// The branch under @: its bookmark, tip, and the commits it owns (bottom-up,
/// down to but excluding the first commit that is trunk or another bookmark).
struct Branch {
    name: RefNameBuf,
    tip: Commit,
    commits: Vec<Commit>,
    base_id: CommitId,
}

pub fn run(gt: &mut Gt, cwd: &Path, opts: Options) -> Result<()> {
    gt.snapshot()?;
    let mut prompt = Prompt::new();
    let branch = current_branch(gt)?;
    let mode = if opts.by_commit {
        Mode::Commit
    } else if opts.by_hunk {
        Mode::Hunk
    } else if !opts.by_file.is_empty() {
        Mode::File(opts.by_file.clone())
    } else if branch.commits.len() == 1 {
        println!(
            "{} has a single commit — splitting by hunk.",
            branch.name.as_str().magenta().bold()
        );
        Mode::Hunk
    } else {
        choose_mode(&branch, &mut prompt)?
    };
    match mode {
        Mode::Commit => by_commit(gt, &branch, &mut prompt),
        Mode::Hunk => by_hunk(gt, &branch, &mut prompt),
        Mode::File(specs) => by_file(gt, cwd, &branch, &specs, opts.message, &mut prompt),
    }
}

fn current_branch(gt: &Gt) -> Result<Branch> {
    let wc = gt.wc_commit()?;
    if !wc.is_empty(gt.repo.as_ref()).block_on()? {
        bail!(
            "the working copy has changes — run `jj-gt modify` or `jj-gt create` first, \
             then split"
        );
    }
    let tip_id = wc
        .parent_ids()
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("working-copy commit has no parent"))?;
    let root_id = gt.repo.store().root_commit_id().clone();
    if tip_id == root_id {
        bail!("working copy is based on jj's root commit, not a branch");
    }
    let trunk_id = gt.trunk_id()?;
    if tip_id == trunk_id {
        bail!(
            "@ sits directly on {} — check out a branch to split it",
            gt.state.trunk
        );
    }
    let tip = gt.repo.store().get_commit(&tip_id)?;
    let name = gt
        .repo
        .view()
        .local_bookmarks_for_commit(&tip_id)
        .map(|(name, _)| name.to_owned())
        .find(|name| **name != *gt.trunk())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "commit {} has no bookmark — split works on branches made with `jj-gt create`",
                short(&tip_id.hex())
            )
        })?;
    if tip.has_conflict() {
        bail!("{} has conflicts — resolve them (`jj-gt modify`) before splitting", name.as_str());
    }

    // The branch owns every commit down to trunk or the next bookmark.
    let mut commits = vec![tip.clone()];
    let mut cursor = tip.clone();
    let base_id = loop {
        let parents = cursor.parent_ids();
        if parents.len() != 1 {
            bail!(
                "commit {} is a merge — split only handles linear branches",
                short(&cursor.id().hex())
            );
        }
        let parent_id = parents[0].clone();
        if parent_id == trunk_id
            || parent_id == root_id
            || gt
                .repo
                .view()
                .local_bookmarks_for_commit(&parent_id)
                .next()
                .is_some()
        {
            break parent_id;
        }
        cursor = gt.repo.store().get_commit(&parent_id)?;
        commits.push(cursor.clone());
    };
    commits.reverse();
    Ok(Branch {
        name,
        tip,
        commits,
        base_id,
    })
}

fn choose_mode(branch: &Branch, prompt: &mut Prompt) -> Result<Mode> {
    let question = format!(
        "{} has {} commits. How do you want to split it?",
        branch.name.as_str().magenta().bold(),
        branch.commits.len()
    );
    let choice = prompt.choose(
        &question,
        &[
            ('c', "by commit — pick split points between existing commits"),
            ('h', "by hunk — pick hunks for each new branch, like git add -p"),
            ('f', "by file — move files matching a pathspec into a new parent branch"),
        ],
    )?;
    Ok(match choice {
        'c' => Mode::Commit,
        'h' => Mode::Hunk,
        _ => {
            println!("Enter pathspecs one per line (e.g. *.json, src/api/); empty line to finish.");
            let mut specs = Vec::new();
            loop {
                let spec = prompt.line("pathspec> ")?;
                if spec.is_empty() {
                    if specs.is_empty() {
                        println!("At least one pathspec is required.");
                        continue;
                    }
                    break;
                }
                specs.push(spec);
            }
            Mode::File(specs)
        }
    })
}

// ── --by-commit ──────────────────────────────────────────────────────────────

fn by_commit(gt: &mut Gt, branch: &Branch, prompt: &mut Prompt) -> Result<()> {
    let n = branch.commits.len();
    if n < 2 {
        bail!(
            "{} has a single commit — use --by-hunk or --by-file to split it",
            branch.name.as_str()
        );
    }
    println!(
        "\nCommits on {} (bottom-up; {} always ends the top branch):",
        branch.name.as_str().magenta().bold(),
        n
    );
    for (i, commit) in branch.commits.iter().enumerate() {
        println!(
            "  {}  {}  {}",
            format!("{:>2}", i + 1).bold(),
            gt.short_commit_id(commit.id())?.dimmed(),
            summarize(commit)
        );
    }
    let picks = prompt.indices(
        &format!("Commits that should END a new branch (e.g. 1,3), from 1 to {}: ", n - 1),
        n - 1,
    )?;
    if picks.is_empty() {
        bail!("no split points selected — nothing to do");
    }

    let mut taken = HashSet::new();
    let mut new_bookmarks = Vec::new();
    let mut lower = 1;
    for pick in picks {
        let commit = &branch.commits[pick - 1];
        let default = default_branch_name(gt, &summarize(commit), &taken);
        let question = format!(
            "Branch name for commits {}..{} (ending at {})",
            lower,
            pick,
            gt.short_commit_id(commit.id())?
        );
        let name = ask_branch_name(gt, prompt, &question, &default, &mut taken)?;
        new_bookmarks.push((name, commit.clone()));
        lower = pick + 1;
    }

    let mut tx = gt.start_tx();
    for (name, commit) in &new_bookmarks {
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new(name), RefTarget::normal(commit.id().clone()));
    }
    gt.explain.note(&format!(
        "by-commit split is bookmark-only: {} new bookmark(s), no commit rewritten",
        new_bookmarks.len()
    ));
    gt.finish_tx(tx, &format!("jj-gt split --by-commit {}", branch.name.as_str()))?;
    println!(
        "Split {} into {} branches",
        branch.name.as_str(),
        new_bookmarks.len() + 1
    );
    if let Err(e) = crate::commands::log::print_stack(gt) {
        eprintln!("jj-gt: note: could not render the stack: {e:#}");
    }
    Ok(())
}

// ── --by-file ────────────────────────────────────────────────────────────────

fn by_file(
    gt: &mut Gt,
    cwd: &Path,
    branch: &Branch,
    patterns: &[String],
    message: Option<String>,
    prompt: &mut Prompt,
) -> Result<()> {
    let specs = patterns
        .iter()
        .map(|p| Pathspec::parse(&gt.root, cwd, p))
        .collect::<Result<Vec<_>>>()?;
    let displays: Vec<&str> = specs.iter().map(|s| s.display.as_str()).collect();
    let (parent_tree, tree) = branch_trees(gt, branch)?;
    let changes = tree_changes(&parent_tree, &tree)?;
    let (matched, rest): (Vec<_>, Vec<_>) = changes
        .into_iter()
        .partition(|(path, _)| any_matches(&specs, path));
    if matched.is_empty() {
        bail!(
            "no changed files on {} match {}",
            branch.name.as_str(),
            displays.join(" ")
        );
    }
    if rest.is_empty() {
        bail!(
            "every changed file on {} matches {} — nothing would be left on the branch",
            branch.name.as_str(),
            displays.join(" ")
        );
    }
    println!(
        "\nExtracting {} file(s) into a new parent branch of {}:",
        matched.len(),
        branch.name.as_str().magenta().bold()
    );
    for (path, _) in &matched {
        println!("  {}", path.as_internal_file_string());
    }

    // Prompts only when a human is there and hasn't already answered via -m.
    let interactive = message.is_none() && prompt.is_terminal();
    let default_message = format!("{} ({})", summarize(&branch.tip), displays.join(" "));
    let message = match message {
        Some(m) => m,
        None if interactive => prompt.input("Commit message for the new branch", &default_message)?,
        None => default_message,
    };
    let mut taken = HashSet::new();
    let default_name = default_branch_name(gt, &message, &taken);
    let name = if interactive {
        ask_branch_name(gt, prompt, "Branch name", &default_name, &mut taken)?
    } else {
        default_name
    };

    let mut builder = MergedTreeBuilder::new(parent_tree);
    for (path, values) in matched {
        builder.set_or_remove(path, values.after);
    }
    let first_tree = builder.write_tree().block_on()?;
    let stats = commit_split(gt, branch, vec![(message, name.clone(), first_tree)], "--by-file")?;
    println!("Created branch {name} below {} ({stats})", branch.name.as_str());
    if let Err(e) = crate::commands::log::print_stack(gt) {
        eprintln!("jj-gt: note: could not render the stack: {e:#}");
    }
    Ok(())
}

// ── --by-hunk ────────────────────────────────────────────────────────────────

const HUNK_HELP: &str = "\
y - include this hunk in the new branch
n - leave this hunk for a later branch (or the current one)
a - include this and all remaining hunks in this file
d - leave this and all remaining hunks in this file
q - quit: abort the split without changing anything
? - print this help";

fn by_hunk(gt: &mut Gt, branch: &Branch, prompt: &mut Prompt) -> Result<()> {
    let (parent_tree, tree) = branch_trees(gt, branch)?;
    let files: Vec<FileChange> = tree_changes(&parent_tree, &tree)?
        .into_iter()
        .map(|(path, values)| FileChange::load(gt.repo.store(), path, values))
        .collect::<Result<_>>()?;
    let total: usize = files.iter().map(FileChange::hunk_count).sum();
    if total == 0 {
        bail!("{} has no changes to split", branch.name.as_str());
    }
    if total < 2 {
        bail!(
            "{} is a single hunk — nothing to split (use --by-commit if it has several commits)",
            branch.name.as_str()
        );
    }
    println!(
        "\n{} changed file(s), {total} hunk(s) on {}. Branches are built bottom-up; \
         whatever you don't pick stays on {}.",
        files.len(),
        branch.name.as_str().magenta().bold(),
        branch.name.as_str()
    );

    // assigned[file][hunk] = the 1-based round (new branch) that took it.
    let mut assigned: Vec<Vec<Option<usize>>> =
        files.iter().map(|f| vec![None; f.hunk_count()]).collect();
    let mut rounds: Vec<(String, String)> = Vec::new(); // (message, branch name)
    let mut taken = HashSet::new();
    let mut remaining = total;
    let mut round = 1;
    loop {
        println!(
            "\n{}",
            format!("─── Branch {round}: pick its hunks ({remaining} left to assign) ───").bold()
        );
        let picked = select_hunks(&files, &mut assigned, round, prompt)?;
        if picked == 0 {
            println!("No hunks selected.");
            if prompt.confirm("Select again?", true)? {
                continue;
            }
            bail!("split aborted — nothing changed");
        }
        remaining -= picked;
        if remaining == 0 {
            // Nothing left for the current branch, so this round IS the
            // current branch: forget the round and keep its name and message.
            for hunk in assigned.iter_mut().flatten() {
                if *hunk == Some(round) {
                    *hunk = None;
                }
            }
            println!(
                "All remaining changes selected — they stay on {}.",
                branch.name.as_str()
            );
            break;
        }
        let default_message = format!("{} (part {round})", summarize(&branch.tip));
        let message = prompt.input(
            &format!("Commit message for branch {round} ({picked} hunk(s))"),
            &default_message,
        )?;
        let default_name = default_branch_name(gt, &message, &taken);
        let name = ask_branch_name(gt, prompt, "Branch name", &default_name, &mut taken)?;
        rounds.push((message, name));
        round += 1;
        if !prompt.confirm(
            &format!(
                "Split the remaining {remaining} hunk(s) further? (otherwise they stay on {})",
                branch.name.as_str()
            ),
            false,
        )? {
            break;
        }
    }
    if rounds.is_empty() {
        bail!("nothing to split — every hunk stayed on {}", branch.name.as_str());
    }

    // Each new branch's tree is the base plus everything assigned to it or an
    // earlier round; FileChange::apply works from the original diff, so every
    // tree is built straight from the parent tree.
    let store = gt.repo.store().clone();
    let mut new_branches = Vec::new();
    for (k, (message, name)) in rounds.into_iter().enumerate() {
        let round = k + 1;
        let mut builder = MergedTreeBuilder::new(parent_tree.clone());
        for (file, hunks) in files.iter().zip(&assigned) {
            let mask: Vec<bool> = hunks.iter().map(|r| r.is_some_and(|r| r <= round)).collect();
            if let Some(value) = file.apply(&store, &mask)? {
                builder.set_or_remove(file.path.clone(), value);
            }
        }
        new_branches.push((message, name, builder.write_tree().block_on()?));
    }
    let names: Vec<String> = new_branches.iter().map(|(_, n, _)| n.clone()).collect();
    let stats = commit_split(gt, branch, new_branches, "--by-hunk")?;
    println!(
        "Created {} branch(es) below {}: {} ({stats})",
        names.len(),
        branch.name.as_str(),
        names.join(", ")
    );
    if let Err(e) = crate::commands::log::print_stack(gt) {
        eprintln!("jj-gt: note: could not render the stack: {e:#}");
    }
    Ok(())
}

/// Walk the unassigned hunks once, asking about each. Returns how many were
/// assigned to `round`.
fn select_hunks(
    files: &[FileChange],
    assigned: &mut [Vec<Option<usize>>],
    round: usize,
    prompt: &mut Prompt,
) -> Result<usize> {
    let mut picked = 0;
    for (file, hunks) in files.iter().zip(assigned.iter_mut()) {
        if hunks.iter().all(Option::is_some) {
            continue;
        }
        println!("{}", file.render_header());
        // Sticky answer for the rest of this file after `a` or `d`.
        let mut rest_of_file: Option<bool> = None;
        for (i, slot) in hunks.iter_mut().enumerate() {
            if slot.is_some() {
                continue;
            }
            let include = match rest_of_file {
                Some(answer) => answer,
                None => {
                    print!("{}", file.render_hunk(i));
                    loop {
                        let answer = prompt.line(&format!(
                            "{} include this hunk in branch {round}? [y,n,a,d,q,?] ",
                            format!("({})", file.path.as_internal_file_string()).dimmed()
                        ))?;
                        match answer.to_ascii_lowercase().as_str() {
                            "y" => break true,
                            "n" => break false,
                            "a" => {
                                rest_of_file = Some(true);
                                break true;
                            }
                            "d" => {
                                rest_of_file = Some(false);
                                break false;
                            }
                            "q" => bail!("split aborted — nothing changed"),
                            _ => println!("{HUNK_HELP}"),
                        }
                    }
                }
            };
            if include {
                *slot = Some(round);
                picked += 1;
            }
        }
    }
    Ok(picked)
}

// ── shared plumbing ──────────────────────────────────────────────────────────

/// (tree the branch was built on, the branch tip's tree).
fn branch_trees(gt: &Gt, branch: &Branch) -> Result<(MergedTree, MergedTree)> {
    let parent_tree = branch.tip.parent_tree(gt.repo.as_ref()).block_on()?;
    if parent_tree.has_conflict() {
        bail!(
            "the parent of {} has conflicts — resolve them before splitting",
            branch.name.as_str()
        );
    }
    Ok((parent_tree, branch.tip.tree()))
}

/// Write the new branches (bottom-up: message, bookmark, tree) on top of the
/// branch's base, then move the current branch on top of them. Its tree is
/// untouched, so its diff becomes whatever the new branches didn't take.
fn commit_split(
    gt: &mut Gt,
    branch: &Branch,
    new_branches: Vec<(String, String, MergedTree)>,
    flag: &str,
) -> Result<String> {
    let mut tx = gt.start_tx();
    let mut parent_id = branch.base_id.clone();
    let mut created = Vec::new();
    for (message, name, tree) in new_branches {
        let commit = tx
            .repo_mut()
            .new_commit(vec![parent_id.clone()], tree)
            .set_description(&message)
            .write()
            .block_on()?;
        tx.repo_mut()
            .set_local_bookmark_target(RefName::new(&name), RefTarget::normal(commit.id().clone()));
        parent_id = commit.id().clone();
        created.push((name, commit));
    }
    let rewritten = tx
        .repo_mut()
        .rewrite_commit(&branch.tip)
        .set_parents(vec![parent_id])
        .write()
        .block_on()?;
    gt.explain.note(&format!(
        "{} new commit(s) written on {}; {} rewritten {} → {} (same tree, new parent); \
         @ and upstack follow via rebase_descendants",
        created.len(),
        short(&branch.base_id.hex()),
        branch.name.as_str(),
        short(&branch.tip.id().hex()),
        short(&rewritten.id().hex())
    ));
    gt.finish_tx(tx, &format!("jj-gt split {flag} {}", branch.name.as_str()))?;
    Ok(format!(
        "{} now {}",
        branch.name.as_str(),
        short(&rewritten.id().hex())
    ))
}

fn bookmark_exists(gt: &Gt, name: &str) -> bool {
    gt.repo
        .view()
        .get_local_bookmark(RefName::new(name))
        .is_present()
}

/// Graphite-style generated name: `<user>/<slug>`, made unique against the
/// repo's bookmarks and the names already chosen in this command.
fn default_branch_name(gt: &Gt, message: &str, taken: &HashSet<String>) -> String {
    let mut slug = slugify(message, 48);
    if slug.is_empty() {
        slug = "split".to_owned();
    }
    let prefix = branch_user_prefix();
    let mut name = format!("{prefix}/{slug}");
    let mut n = 1;
    while bookmark_exists(gt, &name) || taken.contains(&name) {
        n += 1;
        name = format!("{prefix}/{slug}-{n}");
    }
    name
}

fn ask_branch_name(
    gt: &Gt,
    prompt: &mut Prompt,
    question: &str,
    default: &str,
    taken: &mut HashSet<String>,
) -> Result<String> {
    loop {
        let name = prompt.input(question, default)?;
        if name.chars().any(char::is_whitespace) {
            println!("Branch names cannot contain whitespace.");
            continue;
        }
        if bookmark_exists(gt, &name) || taken.contains(&name) {
            println!("A bookmark named {name} already exists.");
            continue;
        }
        taken.insert(name.clone());
        return Ok(name);
    }
}
