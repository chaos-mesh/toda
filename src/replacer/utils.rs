use anyhow::Result;
use procfs::process::{self, Process};

pub fn all_processes() -> Result<impl Iterator<Item = Process>> {
    Ok(process::all_processes()?
        .into_iter()
        .filter_map(|process| process.ok())
        .filter(|process| -> bool {
            if let Ok(cmdline) = process.cmdline() {
                !cmdline.iter().map(|stat| stat.contains("toda")).any(|x| x)
            } else {
                true
            }
        }))
}
