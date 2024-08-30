use std::fs::create_dir_all;
use std::path::Path;

use anyhow::{Context, Result};
use nix::mount::{mount, MsFlags};
use procfs::process::{self, Process};

#[derive(Debug, Clone)]
pub struct MountsInfo {
    mounts: Vec<process::MountInfo>,
}

impl MountsInfo {
    pub fn parse_mounts() -> Result<Self> {
        let process = Process::myself()?;
        let mounts = process.mountinfo()?;

        Ok(MountsInfo { mounts })
    }

    pub fn non_root<P: AsRef<Path>>(&self, path: P) -> Result<bool> {
        let mount_points = self.mounts.iter().map(|item| &item.mount_point);
        for mount_point in mount_points {
            if path.as_ref().starts_with(mount_point) {
                // The relationship is "contain" because if we want to inject /a/b, and /a is a mount point, we can still
                // use this method.
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn move_mount<P1: AsRef<Path>, P2: AsRef<Path>>(
        &self,
        original_path: P1,
        target_path: P2,
    ) -> Result<()> {
        create_dir_all(target_path.as_ref())?;

        mount::<_, _, str, str>(
            Some(original_path.as_ref()),
            target_path.as_ref(),
            None,
            MsFlags::MS_MOVE,
            None,
        )
        .context(format!(
            "source: {}, target: {}",
            original_path.as_ref().display(),
            target_path.as_ref().display()
        ))?;

        Ok(())
    }
}
