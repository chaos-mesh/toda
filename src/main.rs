#![allow(incomplete_features)]
#![feature(box_syntax)]
#![feature(async_closure)]
#![feature(specialization)]

extern crate derive_more;

mod fd_replacer;
mod hookfs;
mod injector;
mod mount;
mod mount_injector;
mod namespace;
mod ptrace;
mod fuse_device;

use fd_replacer::FdReplacer;
use injector::InjectorConfig;
use mount_injector::MountInjector;

use anyhow::Result;
use signal_hook::iterator::Signals;
use structopt::StructOpt;
use tracing::{info, trace, Level};
use tracing_subscriber;
use nix::sys::signal::{signal, Signal, SigHandler};

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
    // ignore dying children
    unsafe {signal(Signal::SIGCHLD, SigHandler::SigIgn)?};

    let option = Options::from_args();
    let verbose = Level::from_str(&option.verbose)?;
    let subscriber = tracing_subscriber::fmt().with_max_level(verbose).finish();
    tracing::subscriber::set_global_default(subscriber).expect("no global subscriber has been set");

    trace!("parse injector configs");
    let injector_config: Vec<InjectorConfig> = serde_json::from_reader(std::io::stdin())?;
    trace!("inject with config {:?}", injector_config);

    let path = option.path;
    let pid = option.pid;

    let mut fdreplacer = FdReplacer::new(&path, pid)?;
    fdreplacer.trace()?;

    let mut injection = MountInjector::create_injection(path, pid, injector_config)?;

    let fuse_dev = fuse_device::read_fuse_dev_t()?;

    let mut mount_injection = namespace::with_mnt_pid_namespace(
        box move || -> Result<_> {
            if let Err(err) = fuse_device::mkfuse_node(fuse_dev) {
                info!("fail to make /dev/fuse node: {}", err)
            }

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

    namespace::with_mnt_pid_namespace(
        box move || -> Result<()> {
            info!("recovering mount");

            // TODO: retry umount multiple times
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
