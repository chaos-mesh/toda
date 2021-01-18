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

#![feature(box_syntax)]
#![feature(async_closure)]
#![feature(vec_into_raw_parts)]
#![feature(atomic_mut_ptr)]
#![feature(drain_filter)]
#![allow(clippy::or_fun_call)]
#![allow(clippy::too_many_arguments)]

extern crate derive_more;

mod fuse_device;
mod hookfs;
mod injector;
mod mount;
mod mount_injector;
mod ptrace;
mod replacer;
mod stop;
mod utils;

use injector::InjectorConfig;
use mount_injector::{MountInjectionGuard, MountInjector};
use replacer::{Replacer, UnionReplacer};
use utils::encode_path;

use anyhow::Result;
use env_logger;
use log::info;
use nix::sys::signal::{signal, SigHandler, Signal};
use nix::unistd::{pipe, read, write};
use structopt::StructOpt;

use std::os::unix::io::RawFd;
use std::path::PathBuf;

#[derive(StructOpt, Debug, Clone)]
#[structopt(name = "basic")]
struct Options {
    #[structopt(long)]
    path: PathBuf,

    #[structopt(long = "mount-only")]
    mount_only: bool,

    #[structopt(short = "v", long = "verbose", default_value = "trace")]
    verbose: String,
}

fn inject(option: Options) -> Result<MountInjectionGuard> {
    info!("parse injector configs");
    let injector_config: Vec<InjectorConfig> = serde_json::from_reader(std::io::stdin())?;
    info!("inject with config {:?}", injector_config);

    let path = option.path.clone();

    info!("canonicalizing path {}", path.display());
    let path = path.canonicalize()?;

    let replacer = if !option.mount_only {
        let mut replacer = UnionReplacer::new();
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

fn resume(option: Options, mount_guard: MountInjectionGuard) -> Result<()> {
    info!("disable injection");
    mount_guard.disable_injection();

    let path = option.path.clone();

    info!("canonicalizing path {}", path.display());
    let path = path.canonicalize()?;
    let (_, new_path) = encode_path(&path)?;

    let replacer = if !option.mount_only {
        let mut replacer = UnionReplacer::new();
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
        write(SIGNAL_PIPE_WRITER, &SIGNAL_MSG).unwrap();
    }
}

fn main() -> Result<()> {
    let (reader, writer) = pipe()?;
    unsafe {
        SIGNAL_PIPE_WRITER = writer;
    }

    unsafe { signal(Signal::SIGINT, SigHandler::Handler(signal_handler))? };
    unsafe { signal(Signal::SIGTERM, SigHandler::Handler(signal_handler))? };

    let option = Options::from_args();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(&option.verbose))
        .init();

    let mount_injector = inject(option.clone())?;

    info!("waiting for signal to exit");
    let mut buf = vec![0u8; 6];
    read(reader, buf.as_mut_slice())?;
    info!("start to recover and exit");

    resume(option, mount_injector)?;

    Ok(())
}
