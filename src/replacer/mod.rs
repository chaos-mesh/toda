use std::path::Path;

use anyhow::Result;

mod cwd_replacer;
mod fd_replacer;

use tracing::error;

pub trait Replacer {
    fn run(&mut self) -> Result<()> ;
}

pub struct UnionReplacer {
    replacers: Vec<Box<dyn Replacer>>
}

impl UnionReplacer {
    pub fn prepare<P1: AsRef<Path>, P2: AsRef<Path>>(
        detect_path: P1,
        new_path: P2) -> Result<UnionReplacer> {
        let mut replacers: Vec<Box<dyn Replacer>> = Vec::new();

        match FdReplacer::prepare(&detect_path, &new_path) {
            Err(err) => error!("Error while preparing fd replacer: {:?}", err),
            Ok(replacer) => replacers.push(Box::new(replacer))
        }
        match CwdReplacer::prepare(&detect_path, &new_path) {
            Err(err) => error!("Error while preparing cwd replacer: {:?}", err),
            Ok(replacer) => replacers.push(Box::new(replacer))
        }

        Ok(
            UnionReplacer {
                replacers,
            }
        )
    }
}

impl Replacer for UnionReplacer {
    fn run(&mut self) -> Result<()> {
        for replacer in self.replacers.iter_mut() {
            replacer.run()?;
        }

        Ok(())
    }
}

pub use cwd_replacer::CwdReplacer;
pub use fd_replacer::FdReplacer;
