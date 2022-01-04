use crate::ptrace;

use std::path::Path;

use anyhow::Result;

mod cwd_replacer;
mod fd_replacer;
mod mmap_replacer;
mod utils;

use tracing::error;

pub trait Replacer {
    fn run(&mut self) -> Result<()>;
}

#[derive(Default)]
pub struct UnionReplacer<'a> {
    replacers: Vec<Box<dyn Replacer + 'a>>,
}

impl<'a> UnionReplacer<'a> {
    pub fn prepare<P1: AsRef<Path>, P2: AsRef<Path>>(
        &mut self,
        detect_path: P1,
        new_path: P2,
    ) -> Result<()> {
        match FdReplacer::prepare(&detect_path, &new_path) {
            Err(err) => error!("Error while preparing fd replacer: {:?}", err),
            Ok(replacer) => self.replacers.push(Box::new(replacer)),
        }
        match CwdReplacer::prepare(&detect_path, &new_path) {
            Err(err) => error!("Error while preparing cwd replacer: {:?}", err),
            Ok(replacer) => self.replacers.push(Box::new(replacer)),
        }
        match MmapReplacer::prepare(&detect_path, &new_path) {
            Err(err) => error!("Error while preparing mmap replacer: {:?}", err),
            Ok(replacer) => self.replacers.push(Box::new(replacer)),
        }
        Ok(())
    }
}

impl<'a> Replacer for UnionReplacer<'a> {
    fn run(&mut self) -> Result<()> {
        for replacer in self.replacers.iter_mut() {
            replacer.run()?;
        }

        Ok(())
    }
}

pub use cwd_replacer::CwdReplacer;
pub use fd_replacer::FdReplacer;
pub use mmap_replacer::MmapReplacer;
