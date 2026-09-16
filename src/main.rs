//! jj-gt — a Graphite-style stacked-PR CLI built on jj-lib.
//! Demo for the JJCon talk "Three Writes, Not One".

mod auth;
mod commands;
mod engine;
mod explain;
mod github;
mod gitnet;
mod settings;
mod stack;
mod state;
mod util;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::explain::Explain;

#[derive(Parser)]
#[command(name = "jj-gt", about = "stacked PRs on jj-lib (JJCon demo)")]
struct Cli {
    /// Narrate the writes as they happen.
    #[arg(long, global = true)]
    explain: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Colocate jj onto this git repo and pick a trunk.
    Init,
    /// Check out a bookmark or commit ID; resume unbookmarked saved work.
    Checkout {
        #[arg(value_name = "BOOKMARK_OR_COMMIT")]
        name: String,
    },
    /// Turn the working copy into a new stack branch.
    Create {
        /// Include all changes (accepted for Graphite parity; jj snapshots everything).
        #[arg(long)]
        all: bool,
        #[arg(short, long)]
        message: String,
    },
    /// Amend the working copy into the current branch commit.
    Modify {
        #[arg(long)]
        all: bool,
    },
    /// Push the stack and create/update stacked PRs.
    Submit,
    /// Fetch trunk, drop merged branches, restack.
    Sync,
    /// Show the current stack, other bookmarks, and unbookmarked saved work.
    #[command(alias = "ls")]
    Log,
    /// Show the operation log tail.
    Ops,
    /// Roll back the last jj-gt command — snapshot included.
    Undo,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("jj-gt: error: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let explain = Explain::new(cli.explain);
    let cwd = std::env::current_dir()?;

    // Network commands authenticate the git subprocess via GIT_ASKPASS.
    let needs_token = matches!(cli.command, Command::Submit | Command::Sync);
    let askpass: Option<(auth::GitAuth, PathBuf)> = if needs_token {
        let token = github::discover_token()?;
        let auth = auth::install(&token)?;
        let path = std::env::var_os("GIT_ASKPASS").map(PathBuf::from);
        path.map(|p| (auth, p))
    } else {
        None
    };
    let askpass_path = askpass.as_ref().map(|(_, p)| p.as_path());

    match cli.command {
        Command::Init => commands::init::run(&cwd, explain),
        Command::Checkout { name } => {
            let mut gt = engine::Gt::load(&cwd, explain)?;
            commands::checkout::run(&mut gt, &name)
        }
        Command::Create { all, message } => {
            let mut gt = engine::Gt::load(&cwd, explain)?;
            commands::create::run(&mut gt, &message, all)
        }
        Command::Modify { all } => {
            let mut gt = engine::Gt::load(&cwd, explain)?;
            commands::modify::run(&mut gt, all)
        }
        Command::Submit => {
            let mut gt = engine::Gt::load(&cwd, explain)?;
            commands::submit::run(&mut gt, askpass_path)
        }
        Command::Sync => {
            let mut gt = engine::Gt::load(&cwd, explain)?;
            commands::sync::run(&mut gt, askpass_path)
        }
        Command::Log => {
            let mut gt = engine::Gt::load(&cwd, explain)?;
            commands::log::run(&mut gt)
        }
        Command::Ops => {
            let gt = engine::Gt::load(&cwd, explain)?;
            commands::ops::run(&gt)
        }
        Command::Undo => {
            let mut gt = engine::Gt::load(&cwd, explain)?;
            commands::undo::run(&mut gt)
        }
    }
}
