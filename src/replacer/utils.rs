use anyhow::Result;
use procfs::process::{self, Process};

pub fn all_processes() -> Result<impl Iterator<Item = Process>> {
    Ok(process::all_processes()?
        .filter_map(|process| process.ok())
        .filter(|process| process.pid != std::process::id() as i32))
}
