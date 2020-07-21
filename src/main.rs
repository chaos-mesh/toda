mod hookfs;
mod mount;
mod namespace;
mod inject;

use inject::InjectionBuilder;

use anyhow::{Result};
use structopt::StructOpt;

use std::path::{Path, PathBuf};

#[derive(StructOpt, Debug)]
#[structopt(name = "basic")]
struct Options {
    #[structopt(short, long)]
    pid: Option<i32>,

    #[structopt(long)]
    path: PathBuf,
}

fn main() -> Result<()> {
    let option = Options::from_args();

    if let Some(pid) = option.pid {
        namespace::enter_mnt_namespace(pid)?
    }

    let injection = InjectionBuilder::new()
        .path(option.path)?
        .run()?;

    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.read_line(&mut line)?;
    
    drop(injection);
    return Ok(());
}
