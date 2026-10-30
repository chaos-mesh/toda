use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{anyhow, Result};
use nix::mount::umount;
use retry::delay::Fixed;
use retry::{retry, OperationResult};
use tracing::{info, warn};

use crate::injector::{InjectorConfig, MultiInjector};
use crate::{hookfs, mount};

pub struct MountInjector {
    original_path: PathBuf,
    new_path: PathBuf,
    injector_config: Vec<InjectorConfig>,
    pending_recovery: Option<MountInjectionGuard>,
}

#[derive(PartialEq)]
enum MountState {
    FuseMounted,
    BackingOnly,
    Restored,
}

pub struct MountInjectionGuard {
    original_path: PathBuf,
    new_path: PathBuf,
    pub hookfs: Arc<hookfs::HookFs>,
    handler: Option<JoinHandle<Result<()>>>,
    unmounter: Option<fuser::SessionUnmounter>,
    state: MountState,
}

impl MountInjectionGuard {
    pub fn enable_injection(&self) {
        self.hookfs.enable_injection();
    }

    pub fn disable_injection(&self) {
        self.hookfs.disable_injection();
    }

    pub fn recover_mount(mut self) -> Result<()> {
        self.try_recover_mount()
    }

    pub fn try_recover_mount(&mut self) -> Result<()> {
        if self.state == MountState::Restored {
            return Ok(());
        }
        if self.state == MountState::FuseMounted {
            let mount_point = self.original_path.clone();
            retry(Fixed::from_millis(500).take(20), || {
                // fuser consumes its mount handle on failure; retry with umount.
                let result = match self.unmounter.take() {
                    Some(mut unmounter) => unmounter.unmount(),
                    None => umount(mount_point.as_path()).map_err(std::io::Error::from),
                };
                match result {
                    Ok(()) => OperationResult::Ok(()),
                    Err(error) => OperationResult::Retry(error),
                }
            })?;
            self.state = MountState::BackingOnly;
        }
        if let Some(handler) = self.handler.take() {
            handler
                .join()
                .map_err(|_| anyhow!("FUSE dispatcher panicked"))??;
        }
        let mounts = mount::MountsInfo::parse_mounts()?;
        mounts.move_mount(&self.new_path, &self.original_path)?;
        self.state = MountState::Restored;
        if let Err(error) = remove_backing_directory(&self.new_path) {
            warn!(path = %self.new_path.display(), %error, "could not remove backing directory");
        }
        Ok(())
    }
}

fn remove_backing_directory(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
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
            pending_recovery: None,
        })
    }

    pub fn take_pending_recovery(&mut self) -> Option<MountInjectionGuard> {
        self.pending_recovery.take()
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

        self.pending_recovery = Some(MountInjectionGuard {
            original_path: self.original_path.clone(),
            new_path: self.new_path.clone(),
            hookfs: hookfs.clone(),
            handler: None,
            unmounter: None,
            state: MountState::BackingOnly,
        });
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
                match self.pending_recovery.as_mut().unwrap().try_recover_mount() {
                    Ok(()) => {
                        self.pending_recovery = None;
                    }
                    Err(rollback) => {
                        return Err(anyhow!(error)
                            .context(format!("mount rollback remains pending: {rollback:#}")))
                    }
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
        let mut guard = self.pending_recovery.take().unwrap();
        guard.handler = Some(handler);
        guard.unmounter = Some(unmounter);
        guard.state = MountState::FuseMounted;
        Ok(guard)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn missing_backing_directory_is_already_clean() -> std::io::Result<()> {
        let path = std::env::temp_dir().join(format!(
            "toda-cleanup-missing-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(!path.exists());
        remove_backing_directory(&path)
    }
}
