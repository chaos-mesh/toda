use crate::hookfs;
use crate::injector::MultiInjector;
use crate::mount;
use crate::InjectorConfig;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use fuse::BackgroundSession;

use tracing::trace;

#[derive(Debug)]
pub struct MountInjector {
    original_path: PathBuf,
    new_path: PathBuf,
    fuse_session: Option<BackgroundSession<'static>>,
    mounts: mount::MountsInfo,
    injector_config: Vec<InjectorConfig>,
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

        let mut new_path: PathBuf = base_path.clone();
        let original_filename = original_path
            .file_name()
            .ok_or(anyhow!("the path terminates in `..` or `/`"))?
            .to_str()
            .ok_or(anyhow!("path with non-UTF-8 character"))?;
        let new_filename = format!("__chaosfs__{}__", original_filename);
        new_path.push(new_filename.as_str());

        return Ok(MountInjector {
            original_path,
            new_path,
            fuse_session: None,
            mounts: mount::MountsInfo::parse_mounts()?,
            injector_config,
        });
    }

    pub fn mount(&mut self) -> Result<()> {
        if self.mounts.non_root(&self.original_path)? {
            // TODO: make the parent mount points private before move mount points
            self.mounts
                .move_mount(&self.original_path, &self.new_path)?;
        } else {
            return Err(anyhow!("inject on a root mount"));
        }

        let injectors = MultiInjector::build(self.injector_config.clone())?;

        let fs = hookfs::AsyncFileSystem::from(hookfs::HookFs::new(
            &self.original_path,
            &self.new_path,
            injectors,
        ));
        let session = unsafe {
            std::fs::create_dir_all(self.new_path.as_path())?;

            let args = [
                "allow_other",
                "nonempty",
                "fsname=toda",
                "default_permissions",
            ];
            let flags: Vec<_> = args
                .iter()
                .flat_map(|item| vec![OsStr::new("-o"), OsStr::new(item)])
                .collect();

            trace!("mount with flags {:?}", flags);
            fuse::spawn_mount(fs, &self.original_path, &flags)?
        };
        trace!("wait 1 second");
        // TODO: remove this. But wait for FUSE gets up
        // Related Issue: https://github.com/zargony/fuse-rs/issues/9
        std::thread::sleep(std::time::Duration::from_secs(1));

        self.fuse_session = Some(session);

        return Ok(());
    }

    #[tracing::instrument(skip(self))]
    pub fn recover_mount(&mut self) -> Result<()> {
        let injection = self.fuse_session.take().unwrap();
        drop(injection);

        // TODO: replace the fd back and force remove the mount
        if self.mounts.non_root(&self.original_path)? {
            // TODO: make the parent mount points private before move mount points
            self.mounts
                .move_mount(&self.new_path, &self.original_path)?;
        } else {
            return Err(anyhow!("inject on a root mount"));
        }

        return Ok(());
    }
}
