use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{anyhow, Result};
use nix::mount::umount;
use retry::delay::Fixed;
use retry::{retry, OperationResult};
use tracing::info;

use crate::injector::{InjectorConfig, MultiInjector};
use crate::{hookfs, mount};

#[derive(Debug)]
pub struct MountInjector {
    original_path: PathBuf,
    new_path: PathBuf,
    injector_config: Vec<InjectorConfig>,
}

pub struct MountInjectionGuard {
    original_path: PathBuf,
    new_path: PathBuf,
    pub hookfs: Arc<hookfs::HookFs>,
    handler: Option<JoinHandle<Result<()>>>,
    unmounter: Option<fuser::SessionUnmounter>,
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

        retry(Fixed::from_millis(500).take(20), || {
            // fuser consumes its mount handle on failure; retries must call umount directly.
            let result = match self.unmounter.take() {
                Some(mut unmounter) => unmounter.unmount(),
                None => umount(mount_point.as_path()).map_err(std::io::Error::from),
            };
            if let Err(err) = result {
                info!("umount returns error: {:?}", err);
                OperationResult::Retry(err)
            } else {
                OperationResult::Ok(())
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
        let injectors = MultiInjector::build(self.injector_config.clone())?;
        let original_path = self.original_path.clone();
        let new_path = self.new_path.clone();

        let mounts = mount::MountsInfo::parse_mounts()?;

        if mounts.non_root(&original_path)? {
            // TODO: make the parent mount points private before move mount points
            mounts.move_mount(original_path, new_path)?;
        } else {
            return Err(anyhow!("inject on a root mount"));
        }

        let hookfs = Arc::new(hookfs::HookFs::new(
            &self.original_path,
            &self.new_path,
            injectors,
        ));

        let fs = hookfs::AsyncFileSystem::from(hookfs.clone());

        let mut config = fuser::Config::default();
        config.acl = fuser::SessionACL::All;
        config.mount_options = vec![
            fuser::MountOption::FSName("toda".to_owned()),
            fuser::MountOption::DefaultPermissions,
        ];

        info!("mount with config {:?}", config);

        let mut session = match fuser::Session::new(fs, &self.original_path, &config) {
            Ok(session) => session,
            Err(error) => {
                if let Err(rollback_error) = mounts.move_mount(&self.new_path, &self.original_path)
                {
                    let message = format!(
                        "FUSE mount failed: {}; failed to restore mount from {} to {}: {:#}",
                        error,
                        self.new_path.display(),
                        self.original_path.display(),
                        rollback_error
                    );
                    return Err(anyhow!(error).context(message));
                }
                return Err(error.into());
            }
        };
        let unmounter = session.unmount_callable();
        let handler = std::thread::spawn(move || {
            session.run()?;

            drop(hookfs::runtime::RUNTIME.write().unwrap().take().unwrap());

            Ok(())
        });
        Ok(MountInjectionGuard {
            handler: Some(handler),
            unmounter: Some(unmounter),
            hookfs,
            original_path: self.original_path.clone(),
            new_path: self.new_path.clone(),
        })
    }
}
