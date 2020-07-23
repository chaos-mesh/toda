mod hookfs;
mod inject;
mod mount;
mod namespace;
mod ptrace;

use inject::InjectionBuilder;

use anyhow::Result;
use signal_hook::iterator::Signals;
use structopt::StructOpt;
use tracing::{info, Level};
use tracing_subscriber;

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

    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("no global subscriber has been set");

    // TODO: enter namespace in another thread
    namespace::enter_mnt_namespace(option.pid)?;

    let mut injection = InjectionBuilder::new()
        .path(option.path)?
        .pid(option.pid)?
        .mount()?;

    injection.reopen()?;

    let signals = Signals::new(&[signal_hook::SIGTERM, signal_hook::SIGINT])?;

    info!("waiting for signal to exit");
    signals.forever().next();
    info!("start to recover and exit");

    injection.recover()?;
    info!("recover successfully");
    return Ok(());
}
