#![feature(box_syntax)]
#![feature(async_closure)]
#![allow(clippy::or_fun_call)]
#![allow(clippy::too_many_arguments)]

extern crate derive_more;

mod fd_replacer;
mod fuse_device;
mod hookfs;
mod injector;
mod mount;
mod mount_injector;
mod namespace;
mod ptrace;

use fd_replacer::{FdReplacer, encode_path};
use injector::InjectorConfig;
use mount_injector::MountInjector;

use anyhow::Result;
use nix::sys::signal::{signal, SigHandler, Signal};
use nix::sys::mman::{MlockAllFlags, mlockall};
use nix::unistd::{pipe, read, write};
use structopt::StructOpt;
use tracing::{info, Level};

use std::os::unix::io::RawFd;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(StructOpt, Debug, Clone)]
#[structopt(name = "basic")]
struct Options {
    #[structopt(short, long)]
    pid: i32,

    #[structopt(long)]
    path: PathBuf,

    #[structopt(short = "v", long = "verbose", default_value = "trace")]
    verbose: String,
}

fn inject(option: Options) -> Result<MountInjector> {
    info!("parse injector configs");
    let injector_config: Vec<InjectorConfig> = serde_json::from_reader(std::io::stdin())?;
    info!("inject with config {:?}", injector_config);

    let path = option.path.clone();
    let fuse_dev = fuse_device::read_fuse_dev_t()?;
    let mount_injector = namespace::with_mnt_pid_namespace(
        box move || -> Result<_> {
            let fdreplacer = FdReplacer::prepare(&path)?;
            let mut injection = MountInjector::create_injection(&path, injector_config)?;

            if let Err(err) = fuse_device::mkfuse_node(fuse_dev) {
                info!("fail to make /dev/fuse node: {}", err)
            }

            injection.mount()?;

            // At this time, `mount --move` has already been executed.
            // Our FUSE are mounted on the "path", so we 
            let (new_path, original_path) = encode_path(&path)?;
            fdreplacer.reopen(original_path.as_path(), new_path.as_path())?;
            drop(fdreplacer);

            Ok(injection)
        },
        option.pid,
    )?;

    info!("enable injection");
    mount_injector.enable_injection();

    Ok(mount_injector)
}

fn resume(option: Options, mut mount_injector: MountInjector) -> Result<()> {
    info!("disable injection");
    mount_injector.disable_injection();
    let path = option.path.clone();

    namespace::with_mnt_pid_namespace(
        box move || -> Result<()> {
            let fdreplacer = FdReplacer::prepare(&path)?;
            let (original_path, new_path) = encode_path(path)?;
            fdreplacer.reopen(original_path, new_path)?;
            info!("fdreplace reopened");

            info!("recovering mount");
            // TODO: retry umount multiple times
            mount_injector.recover_mount()?;

            drop(fdreplacer);
            info!("fdreplace detached");
            info!("recover successfully");
            Ok(())
        },
        option.pid,
    )?;

    Ok(())
}

static mut SIGNAL_PIPE_WRITER: RawFd = 0;

const SIGNAL_MSG: [u8; 6] = *b"SIGNAL";

extern fn signal_handler(_: libc::c_int) {
    unsafe {
        write(SIGNAL_PIPE_WRITER, &SIGNAL_MSG).unwrap();
    }
}

fn main() -> Result<()> {
    mlockall(MlockAllFlags::MCL_CURRENT)?;

    let (reader, writer) = pipe()?;
    unsafe {SIGNAL_PIPE_WRITER = writer;}

    // ignore dying children
    unsafe { signal(Signal::SIGCHLD, SigHandler::SigIgn)? };
    unsafe { signal(Signal::SIGINT, SigHandler::Handler(signal_handler))? };
    unsafe { signal(Signal::SIGTERM, SigHandler::Handler(signal_handler))? };

    let option = Options::from_args();
    let verbose = Level::from_str(&option.verbose)?;
    let subscriber = tracing_subscriber::fmt().with_max_level(verbose).finish();
    tracing::subscriber::set_global_default(subscriber).expect("no global subscriber has been set");

    let mount_injector = inject(option.clone())?;

    info!("waiting for signal to exit");
    let mut buf = vec![0u8; 6];
    read(reader, buf.as_mut_slice())?;
    info!("start to recover and exit");

    resume(option, mount_injector)?;
    
    Ok(())
}
