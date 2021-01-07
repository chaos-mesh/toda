use crate::hookfs;
use crate::injector::MultiInjector;
use crate::mount;
use crate::stop;
use crate::InjectorConfig;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{anyhow, Result};

use nix::mount::umount;

use log::info;

use retry::{retry, delay::Fixed, OperationResult};

#[derive(Debug)]
pub struct MountInjector {
    original_path: PathBuf,
    new_path: PathBuf,
    injector_config: Vec<InjectorConfig>,
}

pub struct MountInjectionGuard {
    original_path: PathBuf,
    new_path: PathBuf,
    hookfs: Arc<hookfs::HookFs>,
    handler: Option<JoinHandle<Result<()>>>,
}

impl MountInjectionGuard {
    pub fn enable_injection(&self) {
        self.hookfs.enable_injection();
    }

    pub fn disable_injection(&self) {
        self.hookfs.disable_injection();
    }

    pub fn recover_mount(mut self) -> Result<()> {
        let mount_point = self.original_path.clone();

        retry(Fixed::from_millis(200).take(10), || {
            if let Err(err) = umount(mount_point.as_path()) {
                info!("umount returns error: {:?}", err);
                return OperationResult::Retry(err)
            } else {
                return OperationResult::Ok(())
            }
        })?;

        info!("unmount successfully!");
        self.handler
            .take()
            .ok_or(anyhow!("handler is empty"))?
            .join()
            .unwrap()?;

        let new_path = self.new_path.clone();
        let original_path = self.original_path;

        let mounts = mount::MountsInfo::parse_mounts()?;

        if mounts.non_root(&original_path)? {
            // TODO: make the parent mount points private before move mount points
            mounts.move_mount(new_path, original_path)?;
        } else {
            return Err(anyhow!("inject on a root mount"));
        }

        Ok(())
    }
}

impl MountInjector {
    pub fn create_injection<P: AsRef<Path>>(
        path: P,
        injector_config: Vec<InjectorConfig>,
    ) -> Result<MountInjector> {
        let original_path: PathBuf = path.as_ref().to_owned();

        let mut base_path: PathBuf = path.as_ref().to_owned();
        if !base_path.pop() {
            return Err(anyhow!("path is the root"));
        }

        let mut new_path: PathBuf = base_path;
        let original_filename = original_path
            .file_name()
            .ok_or(anyhow!("the path terminates in `..` or `/`"))?
            .to_str()
            .ok_or(anyhow!("path with non-UTF-8 character"))?;
        let new_filename = format!("__chaosfs__{}__", original_filename);
        new_path.push(new_filename.as_str());

        Ok(MountInjector {
            original_path,
            new_path,
            injector_config,
        })
    }

    // This method should be called in host namespace
    pub fn mount(&mut self) -> Result<MountInjectionGuard> {
        let original_path = self.original_path.clone();
        let new_path = self.new_path.clone();

        let mounts = mount::MountsInfo::parse_mounts()?;

        if mounts.non_root(&original_path)? {
            // TODO: make the parent mount points private before move mount points
            mounts.move_mount(original_path, new_path)?;
        } else {
            return Err(anyhow!("inject on a root mount"));
        }

        let injectors = MultiInjector::build(self.injector_config.clone())?;

        let hookfs = Arc::new(hookfs::HookFs::new(
            &self.original_path,
            &self.new_path,
            injectors,
        ));

        let original_path = self.original_path.clone();
        let new_path = self.new_path.clone();
        let cloned_hookfs = hookfs.clone();

        let (before_mount_waiter, before_mount_guard) = stop::lock();
        let handler = std::thread::spawn(box move || {
            let fs = hookfs::AsyncFileSystem::from(cloned_hookfs);

            std::fs::create_dir_all(new_path.as_path())?;

            let args = ["allow_other", "fsname=toda", "default_permissions"];
            let flags: Vec<_> = args
                .iter()
                .flat_map(|item| vec![OsStr::new("-o"), OsStr::new(item)])
                .collect();

            info!("mount with flags {:?}", flags);

            drop(before_mount_guard);
            fuser::mount(fs, &original_path, &flags)?;

            drop(hookfs::runtime::RUNTIME.write().unwrap().take().unwrap());

            Ok(())
        });
        // TODO: remove this. But wait for FUSE gets up
        // Related Issue: https://github.com/zargony/fuse-rs/issues/9
        before_mount_waiter.wait();
        std::thread::sleep(std::time::Duration::from_secs(1));

        Ok(MountInjectionGuard {
            handler: Some(handler),
            hookfs,
            original_path: self.original_path.clone(),
            new_path: self.new_path.clone(),
        })
    }
}
