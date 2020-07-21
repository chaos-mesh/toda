use crate::hookfs;
use crate::mount;

use std::fs::rename;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use fuse::BackgroundSession;

#[derive(Default)]
pub struct InjectionBuilder {
    original_path: Option<PathBuf>,
    new_path: Option<PathBuf>,
}

impl InjectionBuilder {
    pub fn new() -> InjectionBuilder {
        return InjectionBuilder::default();
    }

    pub fn path<P: AsRef<Path>>(self, path: P) -> Result<InjectionBuilder> {
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

        return Ok(InjectionBuilder {
            original_path: Some(original_path),
            new_path: Some(new_path),
        });
    }

    pub fn run(self) -> Result<Injection> {
        if let InjectionBuilder {
            original_path: Some(original_path),
            new_path: Some(new_path),
        } = self
        {
            if mount::is_root(&original_path)? {
                // TODO: make the parent mount points private before move mount points
                mount::move_mount(&original_path, &new_path)?;
            } else {
                rename(&original_path, &new_path)?;
            }

            let fs = hookfs::HookFs::new(&original_path, &new_path);
            let session = unsafe {
                std::fs::create_dir_all(new_path.as_path())?;

                fuse::spawn_mount(fs, &original_path, &[])?
            };

            return Ok(Injection {
                original_path,
                new_path,
                fuse_session: Some(session),
            });
        } else {
            return Err(anyhow!("run without setting path"));
        }
    }
}

pub struct Injection {
    original_path: PathBuf,
    new_path: PathBuf,
    fuse_session: Option<BackgroundSession<'static>>,
}

impl Drop for Injection {
    fn drop(&mut self) {
        let injection = self.fuse_session.take().unwrap();
        drop(injection);

        if mount::is_root(&self.new_path).unwrap() {
            // TODO: make the parent mount points private before move mount points
            mount::move_mount(&self.new_path, &self.original_path).unwrap();
        } else {
            rename(&self.new_path, &self.original_path).unwrap();
        }
    }
}
