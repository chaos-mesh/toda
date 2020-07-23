mod hookfs;
mod inject;
mod mount;
mod namespace;
mod ptrace;

use inject::InjectionBuilder;

use anyhow::Result;
use signal_hook::iterator::Signals;
use structopt::StructOpt;

use std::path::PathBuf;

#[derive(StructOpt, Debug)]
#[structopt(name = "basic")]
struct Options {
    #[structopt(short, long)]
    pid: i32,

    #[structopt(long)]
    path: PathBuf,
}

fn main() -> Result<()> {
    let option = Options::from_args();

    // TODO: enter namespace in another thread
    namespace::enter_mnt_namespace(option.pid)?;

    let mut injection = InjectionBuilder::new()
        .path(option.path)?
        .pid(option.pid)?
        .mount()?;

    injection.reopen()?;

    let signals = Signals::new(&[signal_hook::SIGTERM, signal_hook::SIGINT])?;

    println!("ARRIVE HERE 1");
    signals.forever().next();

    println!("ARRIVE HERE 2");
    injection.recover()?;
    return Ok(());
}
