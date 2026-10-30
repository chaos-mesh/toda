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
use std::sync::Mutex;
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
use tracing::{error, info};
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

enum Phase {
    SetupRollback,
    Active,
    RecoveryRollback,
    RecoveryCommit,
    RecoveryMount,
    ReleaseToActive,
    ReleaseToDone,
    Done,
}

struct Injection {
    option: Options,
    mount: Option<MountInjectionGuard>,
    references: Option<UnionReplacer>,
    phase: Phase,
}

impl Injection {
    fn new(option: Options) -> Self {
        Self {
            option,
            mount: None,
            references: None,
            phase: Phase::SetupRollback,
        }
    }

    fn start(&mut self) -> Result<()> {
        self.option.path = self.option.path.canonicalize()?;
        if !self.option.mount_only {
            let references = self.references.insert(UnionReplacer::default());
            references.prepare(&self.option.path, &self.option.path)?;
        }
        if let Err(error) = fuse_device::mkfuse_node() {
            info!(%error, "could not create /dev/fuse node");
        }
        let mut injection =
            MountInjector::create_injection(&self.option.path, Vec::<InjectorConfig>::new())?;
        match injection.mount() {
            Ok(guard) => self.mount = Some(guard),
            Err(error) => {
                self.mount = injection.take_pending_recovery();
                return Err(error);
            }
        }
        if let Some(references) = &mut self.references {
            references.run()?;
        }
        self.phase = Phase::ReleaseToActive;
        self.finish_references()?;
        self.mount.as_ref().unwrap().enable_injection();
        self.phase = Phase::Active;
        info!("injection enabled");
        Ok(())
    }

    fn finish_references(&mut self) -> Result<()> {
        if let Some(references) = &mut self.references {
            references.commit()?;
            references.release()?;
        }
        self.references = None;
        Ok(())
    }

    fn recover(&mut self) -> Result<()> {
        loop {
            match self.phase {
                Phase::SetupRollback => {
                    if let Some(references) = &mut self.references {
                        references.rollback()?;
                    }
                    if let Some(mount) = &mut self.mount {
                        mount.try_recover_mount()?;
                    }
                    if let Some(references) = &mut self.references {
                        references.release()?;
                    }
                    self.references = None;
                    self.mount = None;
                    self.phase = Phase::Done;
                }
                Phase::ReleaseToActive => {
                    self.finish_references()?;
                    self.phase = Phase::Active;
                }
                Phase::Active => {
                    self.mount.as_ref().unwrap().disable_injection();
                    self.phase = Phase::RecoveryRollback;
                    if !self.option.mount_only {
                        let (_, backing) = encode_path(&self.option.path)?;
                        let references = self.references.insert(UnionReplacer::default());
                        references.prepare(&self.option.path, backing)?;
                        references.run()?;
                    }
                    self.phase = Phase::RecoveryCommit;
                }
                Phase::RecoveryRollback => {
                    if let Some(references) = &mut self.references {
                        references.rollback()?;
                        references.release()?;
                    }
                    self.references = None;
                    self.phase = Phase::Active;
                    anyhow::bail!(
                        "failed recovery was rolled back; send SIGINT or SIGTERM to retry"
                    );
                }
                Phase::RecoveryCommit => {
                    if let Some(references) = &mut self.references {
                        references.commit()?;
                    }
                    self.phase = Phase::RecoveryMount;
                }
                Phase::RecoveryMount => {
                    self.mount.as_mut().unwrap().try_recover_mount()?;
                    self.phase = Phase::ReleaseToDone;
                }
                Phase::ReleaseToDone => {
                    self.finish_references()?;
                    self.references = None;
                    self.mount = None;
                    self.phase = Phase::Done;
                }
                Phase::Done => return Ok(()),
            }
        }
    }
}

static mut SIGNAL_PIPE_WRITER: RawFd = 0;

const SIGNAL_MSG: [u8; 6] = *b"SIGNAL";

extern "C" fn signal_handler(_: libc::c_int) {
    unsafe {
        let _ = write(BorrowedFd::borrow_raw(SIGNAL_PIPE_WRITER), &SIGNAL_MSG);
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
    let mut injection = Injection::new(option);
    let setup = injection.start();
    if let Err(error) = &setup {
        error!(error = %format!("{error:#}"), "injection setup failed; recovery resources retained");
    }
    let status = setup.map_err(|error| anyhow::Error::msg(error.to_string()));
    let hookfs = injection.mount.as_ref().map(|mount| mount.hookfs.clone());
    thread::spawn(|| {
        Runtime::new()
            .expect("Failed to create Tokio runtime")
            .block_on(start_server(jsonrpc::RpcImpl::new(
                Mutex::new(status),
                hookfs,
            )));
    });
    loop {
        info!("waiting for SIGINT or SIGTERM to recover");
        if let Err(error) = wait_for_signal(&reader) {
            error!(%error, "signal wait failed; retaining recovery resources");
            continue;
        }
        match injection.recover() {
            Ok(()) => {
                info!("recovery completed");
                return Ok(());
            }
            Err(error) => {
                error!(error = %format!("{error:#}"), "recovery incomplete; resources retained for another signal")
            }
        }
    }
}
