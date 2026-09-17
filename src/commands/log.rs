//! jj-gt log: the current stack and other available checkout targets.

use std::collections::HashSet;

use anyhow::Result;
use jj_lib::repo::Repo as _;
use owo_colors::OwoColorize;

use crate::engine::{Gt, summarize};
use crate::stack::current_stack;
use crate::util::short;

pub fn run(gt: &mut Gt) -> Result<()> {
    gt.snapshot()?;
    print_stack(gt)
}

pub fn print_stack(gt: &Gt) -> Result<()> {
    let entries = current_stack(gt)?;
    let trunk_id = gt.trunk_id()?;
    let wc_id = gt.wc_commit()?.id().clone();
    let mut shown_commits = HashSet::from([trunk_id.clone()]);
    let mut shown_bookmarks = HashSet::from([gt.state.trunk.clone()]);
    println!();
    // Top of stack first. The vertical column implies each line's parent is
    // the line below — flag it when that's not actually true (forks).
    for (i, entry) in entries.iter().enumerate().rev() {
        let parent_below = if i == 0 {
            Some(&trunk_id)
        } else {
            entries.get(i - 1).map(|e| e.commit.id())
        };
        let actual_parent = entry.commit.parent_ids().first();
        let forked = actual_parent.is_some() && actual_parent != parent_below;
        let entry: &crate::stack::StackEntry = entry;
        let id = gt.short_commit_id(entry.commit.id())?;
        shown_commits.insert(entry.commit.id().clone());
        let here = entry.commit.id() == &wc_id;
        let marker = if here {
            "◉".green().bold().to_string()
        } else {
            "◯".to_string()
        };
        let mut line = format!("{marker}  ");
        if let Some(bookmark) = &entry.bookmark {
            shown_bookmarks.insert(bookmark.as_str().to_owned());
            line.push_str(&bookmark.as_str().magenta().bold().to_string());
            line.push_str("  ");
            if let Some(pr) = gt.state.prs.get(bookmark.as_str()) {
                line.push_str(&format!("#{pr}").cyan().to_string());
                line.push_str("  ");
            }
        }
        line.push_str(&id.dimmed().to_string());
        if entry.is_scratch {
            let label = if here {
                "  (working copy — empty)"
            } else {
                "  (empty)"
            };
            line.push_str(&label.dimmed().to_string());
        } else if entry.bookmark.is_none() {
            line.push_str(&"  (unbookmarked)".dimmed().to_string());
        }
        if entry.commit.has_conflict() {
            line.push_str(&"  CONFLICT".red().bold().to_string());
        }
        if here {
            line.push_str(&"  ← you are here".green().to_string());
        }
        println!("{line}");
        let desc = entry.commit.description().lines().next().unwrap_or("");
        if !desc.is_empty() {
            println!("{}  {}", "│".dimmed(), desc);
        }
        if forked {
            let parent = actual_parent
                .map(|id| short(&jj_lib::object_id::ObjectId::hex(id)))
                .unwrap_or_default();
            println!(
                "{}",
                format!("╌  (fork: parent is {parent}, not the line below)").dimmed()
            );
        }
    }
    println!(
        "{}  {}  {}  {}",
        "◈".blue().bold(),
        gt.state.trunk.blue().bold(),
        gt.short_commit_id(&trunk_id)?.dimmed(),
        "(trunk)".dimmed()
    );

    // Keep navigation broader than the active stack. In particular, an alias
    // on an already-shown commit is still a useful checkout target.
    let mut heading = false;
    for (name, target) in gt.checkout_bookmarks() {
        if shown_bookmarks.contains(&name) {
            continue;
        }
        if !heading {
            println!("\n{}", "Other bookmarks (checkout by name)".bold());
            heading = true;
        }
        if let Some(id) = target.as_normal() {
            let commit = gt.repo.store().get_commit(id)?;
            println!(
                "◯  {}  {}  {}",
                name.magenta().bold(),
                gt.short_commit_id(id)?.dimmed(),
                summarize(&commit)
            );
        } else {
            println!(
                "◯  {}  {}",
                name.magenta().bold(),
                "CONFLICT — resolve with jj".red()
            );
        }
    }

    let mut heads: Vec<_> = gt.repo.view().heads().iter().collect();
    heads.sort();
    let mut heading = false;
    for id in heads {
        if shown_commits.contains(id) {
            continue;
        }
        let commit = gt.repo.store().get_commit(id)?;
        if !gt.is_saved_work(&commit)? {
            continue;
        }
        if !heading {
            println!("\n{}", "Saved work (checkout by commit ID)".bold());
            heading = true;
        }
        let conflict = if commit.has_conflict() {
            "  CONFLICT"
        } else {
            ""
        };
        println!(
            "◯  {}  (unbookmarked)  {}{}",
            gt.short_commit_id(id)?,
            summarize(&commit),
            conflict.red()
        );
    }
    println!(
        "\n{}",
        "Checkout: jj-gt checkout <bookmark-or-commit-id>".dimmed()
    );
    println!();
    Ok(())
}
