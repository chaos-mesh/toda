mod hookfs;
mod mount;
mod namespace;

use anyhow::{anyhow, Result};
use structopt::StructOpt;

use std::fs::rename;
use std::path::{Path, PathBuf};

#[derive(StructOpt, Debug)]
#[structopt(name = "basic")]
struct Options {
    #[structopt(short, long)]
    pid: Option<i32>,

    #[structopt(long)]
    path: PathBuf,
}

fn main() -> Result<()> {
    let option = Options::from_args();

    if let Some(pid) = option.pid {
        namespace::enter_mnt_namespace(pid)?
    }

    let original_path: PathBuf = option.path.clone();

    let mut base_path: PathBuf = option.path.clone();
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

    if mount::is_root(&original_path)? {
        // TODO: make the parent mount points private before move mount points
        mount::move_mount(&original_path, &new_path)?;
    } else {
        rename(&original_path, &new_path)?;
    }

    let fs = hookfs::HookFs::new(&original_path, &new_path);
    unsafe {
        std::fs::create_dir_all(new_path.as_path())?;

        let session = fuse::spawn_mount(fs, &original_path, &[])?;

        std::thread::sleep(std::time::Duration::from_secs(100));
    }
    return Ok(());
}
