//! gt log: the colored stack view.

use anyhow::Result;
use owo_colors::OwoColorize;

use crate::engine::Gt;
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
        let id = jj_lib::object_id::ObjectId::hex(entry.commit.id());
        let here = entry.commit.id() == &wc_id;
        let marker = if here { "◉".green().bold().to_string() } else { "◯".to_string() };
        let mut line = format!("{marker}  ");
        if let Some(bookmark) = &entry.bookmark {
            line.push_str(&bookmark.as_str().magenta().bold().to_string());
            line.push_str("  ");
            if let Some(pr) = gt.state.prs.get(bookmark.as_str()) {
                line.push_str(&format!("#{pr}").cyan().to_string());
                line.push_str("  ");
            }
        }
        line.push_str(&short(&id).dimmed().to_string());
        if entry.is_scratch {
            line.push_str(&"  (working copy — empty)".dimmed().to_string());
        } else if entry.commit.has_conflict() {
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
            println!("{}", format!("╌  (fork: parent is {parent}, not the line below)").dimmed());
        }
    }
    println!(
        "{}  {}  {}  {}",
        "◈".blue().bold(),
        gt.state.trunk.blue().bold(),
        short(&jj_lib::object_id::ObjectId::hex(&trunk_id)).dimmed(),
        "(trunk)".dimmed()
    );
    println!();
    Ok(())
}
