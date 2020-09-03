use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

pub fn encode_path<P: AsRef<Path>>(original_path: P) -> Result<(PathBuf, PathBuf)> {
    let original_path: PathBuf = original_path.as_ref().to_owned();

    let mut base_path: PathBuf = original_path.clone();
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

    Ok((original_path, new_path))
}
