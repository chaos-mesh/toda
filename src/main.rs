#![feature(box_syntax)]
#![feature(async_closure)]

extern crate derive_more;

mod hookfs;
mod inject;
mod mount;
mod namespace;
mod ptrace;

use inject::Injection;

use anyhow::Result;
use signal_hook::iterator::Signals;
use structopt::StructOpt;
use tracing::{info, Level};
use tracing_subscriber;

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

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

    let injection = Arc::new(Mutex::new(Injection::create_injection(
        option.path,
        option.pid,
    )?));
    let mount_injection = injection.clone();

    namespace::with_mnt_namespace(
        box move || -> Result<()> {
            mount_injection.lock().unwrap().mount()?;
            return Ok(());
        },
        option.pid,
    )?;

    injection.lock().unwrap().reopen()?;

    let signals = Signals::new(&[signal_hook::SIGTERM, signal_hook::SIGINT])?;

    info!("waiting for signal to exit");
    signals.forever().next();
    info!("start to recover and exit");

    injection.lock().unwrap().reopen()?;
    let mount_injection = injection.clone();
    namespace::with_mnt_namespace(
        box move || -> Result<()> {
            mount_injection.lock().unwrap().recover_mount()?;
            return Ok(());
        },
        option.pid,
    )?;
    info!("recover successfully");
    return Ok(());
}
