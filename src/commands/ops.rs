//! gt ops: tail of the operation log — makes the hidden op-log writes visible.

use anyhow::Result;
use jj_lib::repo::ReadonlyRepo;
use owo_colors::OwoColorize;
use pollster::FutureExt as _;

use crate::engine::Gt;
use crate::util::short;

pub fn run(gt: &Gt) -> Result<()> {
    print_ops(&gt.repo, 12)
}

pub fn print_ops(repo: &ReadonlyRepo, limit: usize) -> Result<()> {
    let mut op = Some(repo.operation().clone());
    let mut n = 0;
    while let Some(current) = op {
        if n >= limit {
            println!("{}", "  ⋮".dimmed());
            break;
        }
        let meta = current.metadata();
        let marker = if n == 0 { "@".green().bold().to_string() } else { "○".dimmed().to_string() };
        println!(
            "{} {}  {}",
            marker,
            short(&jj_lib::object_id::ObjectId::hex(current.id())).yellow(),
            meta.description
        );
        n += 1;
        op = current.parents().block_on()?.into_iter().next();
    }
    Ok(())
}
