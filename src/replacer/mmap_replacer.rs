use crate::ptrace;

use std::io::{Cursor, Read, Write};
use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

#[derive(Clone, Copy)]
#[repr(packed)]
#[repr(C)]
struct ReplaceCase {
    memory_addr: u64,
    length: usize,
    new_path_offset: u64,
}

struct ProcessAccesser {
    process: ptrace::TracedProcess,

    cases: Vec<ReplaceCase>,
    new_paths: Cursor<Vec<u8>>,
}

pub struct MmapReplacer {
    processes: HashMap<i32, ProcessAccesser>,
}

impl MmapReplacer {
    #[tracing::instrument(skip(detect_path, new_path))]
    pub fn prepare<P1: AsRef<Path>, P2: AsRef<Path>>(
        detect_path: P1,
        new_path: P2,
    ) -> Result<MmapReplacer> {
        let processes = HashMap::new();
        for process in procfs::process::all_processes()? {
            let pid = process.pid;

            let mmaps = process.maps();
            if mmaps.is_err() {
                continue
            }
        }

        Ok(MmapReplacer {
            processes,
        })
    }
}