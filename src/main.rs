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

mod cmd;
mod fuse_device;
mod hookfs;
mod injector;
mod mount;
mod mount_injector;
mod ptrace;
mod replacer;
mod stop;
mod todarpc;
mod utils;

use std::convert::TryFrom;
use std::io;
use std::path::PathBuf;
use std::process::exit;
use std::sync::{mpsc, Mutex};

use anyhow::Result;
use injector::InjectorConfig;
use mount_injector::{MountInjectionGuard, MountInjector};
use replacer::{Replacer, UnionReplacer};
use structopt::StructOpt;
use toda::signal::Signals;
use tokio::signal::unix::SignalKind;
use tracing::{info, instrument};
use tracing_subscriber::EnvFilter;
use utils::encode_path;

use crate::cmd::interactive::handler::TodaServer;
use crate::todarpc::TodaRpc;

#[derive(StructOpt, Debug, Clone)]
#[structopt(name = "basic")]
struct Options {
    #[structopt(long)]
    path: PathBuf,

    #[structopt(long = "mount-only")]
    mount_only: bool,

    #[structopt(short = "v", long = "verbose", default_value = "trace")]
    verbose: String,

    #[structopt(long = "interactive-path")]
    interactive_path: Option<PathBuf>,
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let option = Options::from_args();
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_from(&option.verbose))
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
    if let Some(path) = option.interactive_path.clone() {
        let hookfs = match &mount_injector {
            Ok(e) => Some(e.hookfs.clone()),
            Err(_) => None,
        };
        let mut toda_server =
            TodaServer::new(TodaRpc::new(Mutex::new(status), Mutex::new(tx), hookfs));
        toda_server.serve_interactive(path.clone());

        info!("waiting for signal to exit");
        let mut signals = Signals::from_kinds(&[SignalKind::interrupt(), SignalKind::terminate()])?;
        signals.wait().await;

        info!("start to recover and exit");
        if let Ok(v) = mount_injector {
            resume(option, v)?;
        }

        // delete the unix socket file
        std::fs::remove_file(path.clone())?;
        exit(0);
    }
    Ok(())
}
