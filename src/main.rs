#![feature(box_syntax)]
#![feature(async_closure)]

extern crate derive_more;

mod hookfs;
mod mount_injector;
mod mount;
mod namespace;
mod ptrace;
mod fd_replacer;

use mount_injector::MountInjector;
use fd_replacer::FdReplacer;

use anyhow::Result;
use signal_hook::iterator::Signals;
use structopt::StructOpt;
use tracing::{info, Level};
use tracing_subscriber;

use std::path::PathBuf;
use std::str::FromStr;

#[derive(StructOpt, Debug)]
#[structopt(name = "basic")]
struct Options {
    #[structopt(short, long)]
    pid: i32,

    #[structopt(long)]
    path: PathBuf,

    #[structopt(short = "v", long = "verbose", default_value = "trace")]
    verbose: String,
}

fn main() -> Result<()> {
    let option = Options::from_args();

    let verbose = Level::from_str(&option.verbose)?;

    let subscriber = tracing_subscriber::fmt().with_max_level(verbose).finish();
    tracing::subscriber::set_global_default(subscriber).expect("no global subscriber has been set");

    let path = option.path;
    let pid = option.pid;

    let mut fdreplacer = FdReplacer::new(&path, pid)?;
    fdreplacer.trace()?;

    let mut mount_injection = namespace::with_mnt_namespace(
        box move || -> Result<_> {
            let mut injection =MountInjector::create_injection(
                path,
            )?;

            injection.mount()?;
            
            return Ok(injection);
        },
        option.pid,
    )?;
    fdreplacer.reopen()?;
    fdreplacer.detach()?;

    let signals = Signals::new(&[signal_hook::SIGTERM, signal_hook::SIGINT])?;

    info!("waiting for signal to exit");
    signals.forever().next();
    info!("start to recover and exit");

    fdreplacer.trace()?;
    fdreplacer.reopen()?;

    info!("fdreplace reopened");

    namespace::with_mnt_namespace(
        box move || -> Result<()> {
            info!("recovering mount");

            mount_injection.recover_mount()?;
            return Ok(());
        },
        option.pid,
    )?;

    fdreplacer.detach()?;
    info!("fdreplace detached");
    
    info!("recover successfully");
    return Ok(());
}
