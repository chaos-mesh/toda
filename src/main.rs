mod hookfs;
mod inject;
mod mount;
mod namespace;

use inject::InjectionBuilder;

use anyhow::{anyhow, Result};
use signal_hook::iterator::Signals;
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

    let injection = InjectionBuilder::new().path(option.path)?.run()?;

    let signals = Signals::new(&[signal_hook::SIGTERM, signal_hook::SIGINT])?;

    'outer: loop {
        // Pick up signals that arrived since last time
        for _ in signals.forever() {
            break 'outer;
        }
    }

    drop(injection);
    return Ok(());
}
