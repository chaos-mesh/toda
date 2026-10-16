// Copyright 2020 Chaos Mesh Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// See the License for the specific language governing permissions and
// limitations under the License.

#![allow(clippy::or_fun_call)]
#![allow(clippy::too_many_arguments)]

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::path::PathBuf;
use std::sync::{mpsc, Mutex};
use std::{io, thread};

use anyhow::Result;
use clap::Parser;
use nix::sys::signal::{signal, SigHandler, Signal};
use nix::unistd::{pipe, read, write};
use toda::fuse_device;
use toda::injector::InjectorConfig;
use toda::jsonrpc::{self, start_server};
use toda::mount_injector::{MountInjectionGuard, MountInjector};
use toda::replacer::{Replacer, UnionReplacer};
use toda::utils::encode_path;
use tokio::runtime::Runtime;
use tracing::{info, instrument};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug, Clone)]
#[command(name = "toda", version)]
struct Options {
    #[arg(long)]
    path: PathBuf,

    #[arg(long = "mount-only")]
    mount_only: bool,

    #[arg(short = 'v', long = "verbose", default_value = "trace")]
    verbose: String,
}

#[instrument(skip(option))]
fn inject(option: Options, injector_config: Vec<InjectorConfig>) -> Result<MountInjectionGuard> {
    info!("inject with config {:?}", injector_config);

    let path = option.path.clone();

    info!("canonicalizing path {}", path.display());
    let path = path.canonicalize()?;

    let replacer = if !option.mount_only {
        let mut replacer = UnionReplacer::default();
        replacer.prepare(&path, &path)?;

        Some(replacer)
    } else {
        None
    };

    if let Err(err) = fuse_device::mkfuse_node() {
        info!("fail to make /dev/fuse node: {}", err)
    }

    let mut injection = MountInjector::create_injection(&option.path, injector_config)?;
    let mount_guard = injection.mount()?;
    info!("mount successfully");

    if let Some(mut replacer) = replacer {
        // At this time, `mount --move` has already been executed.
        // Our FUSE are mounted on the "path", so we
        replacer.run()?;
        drop(replacer);
        info!("replacer detached");
    }

    info!("enable injection");
    mount_guard.enable_injection();

    Ok(mount_guard)
}

#[instrument(skip(option, mount_guard))]
fn resume(option: Options, mount_guard: MountInjectionGuard) -> Result<()> {
    info!("disable injection");
    mount_guard.disable_injection();

    let path = option.path.clone();

    info!("canonicalizing path {}", path.display());
    let path = path.canonicalize()?;
    let (_, new_path) = encode_path(&path)?;

    let replacer = if !option.mount_only {
        let mut replacer = UnionReplacer::default();
        replacer.prepare(&path, &new_path)?;
        info!("running replacer");
        let result = replacer.run();
        info!("replace result: {:?}", result);

        Some(replacer)
    } else {
        None
    };

    info!("recovering mount");
    mount_guard.recover_mount()?;

    info!("replacers detached");
    info!("recover successfully");

    drop(replacer);
    Ok(())
}

static mut SIGNAL_PIPE_WRITER: RawFd = 0;

const SIGNAL_MSG: [u8; 6] = *b"SIGNAL";

extern "C" fn signal_handler(_: libc::c_int) {
    unsafe {
        write(BorrowedFd::borrow_raw(SIGNAL_PIPE_WRITER), &SIGNAL_MSG).unwrap();
    }
}

fn wait_for_signal(chan: &impl AsFd) -> Result<()> {
    let mut buf = vec![0u8; 6];
    read(chan, buf.as_mut_slice())?;
    Ok(())
}

fn main() -> Result<()> {
    let (reader, writer) = pipe()?;
    unsafe {
        SIGNAL_PIPE_WRITER = writer.as_raw_fd();
    }

    unsafe { signal(Signal::SIGINT, SigHandler::Handler(signal_handler))? };
    unsafe { signal(Signal::SIGTERM, SigHandler::Handler(signal_handler))? };

    let option = Options::parse();
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&option.verbose))
        .or_else(|_| EnvFilter::try_new("trace"))
        .unwrap();
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(env_filter)
        .init();
    info!("start with option: {:?}", option);
    let mount_injector = inject(option.clone(), vec![]);

    let status = match &mount_injector {
        Ok(_) => Ok(()),
        Err(e) => Err(anyhow::Error::msg(e.to_string())),
    };

    let (tx, _) = mpsc::channel();
    {
        let hookfs = match &mount_injector {
            Ok(e) => Some(e.hookfs.clone()),
            Err(_) => None,
        };
        thread::spawn(|| {
            Runtime::new()
                .expect("Failed to create Tokio runtime")
                .block_on(start_server(jsonrpc::RpcImpl::new(
                    Mutex::new(status),
                    Mutex::new(tx),
                    hookfs,
                )));
        });
    }
    info!("waiting for signal to exit");
    wait_for_signal(&reader)?;
    info!("start to recover and exit");
    if let Ok(v) = mount_injector {
        resume(option, v)?;
    }
    Ok(())
}
