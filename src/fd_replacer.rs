use crate::ptrace;
use crate::utils;

use std::fs::read_dir;
use std::fs::read_link;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::{collections::HashMap, fmt::Debug};

use anyhow::{anyhow, Result};

use dynasmrt::{dynasm, DynasmApi, DynasmLabelApi};

use tracing::{info, trace, warn};

#[derive(Clone, Copy)]
#[repr(packed)]
#[repr(C)]
struct ReplaceCase {
    fd: u64,
    new_path_offset: u64,
}

impl ReplaceCase {
    pub fn new(fd: u64, new_path_offset: u64) -> ReplaceCase {
        ReplaceCase {
            fd,
            new_path_offset,
        }
    }
}

struct ProcessAccesser {
    process: ptrace::TracedProcess,

    cases: Vec<ReplaceCase>,
    new_paths: Cursor<Vec<u8>>,
}

impl Debug for ProcessAccesser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.process.fmt(f)
    }
}

impl ProcessAccesser {
    pub fn prepare(pid: i32) -> Result<ProcessAccesser> {
        let process = ptrace::TracedProcess::trace(pid)?;

        Ok(ProcessAccesser {
            process,

            cases: Vec::new(),
            new_paths: Cursor::new(Vec::new()),
        })
    }

    #[tracing::instrument]
    pub fn push_case(&mut self, fd: u64, new_path: PathBuf) -> anyhow::Result<()> {
        info!("push case fd: {}, new_path: {}", fd, new_path.display());

        let mut new_path = new_path
            .to_str()
            .ok_or(anyhow!("fd contains non-UTF-8 character"))?
            .as_bytes()
            .to_vec();

        new_path.push(0);

        let offset = self.new_paths.position();
        self.new_paths.write_all(new_path.as_slice())?;

        self.cases.push(ReplaceCase::new(fd, offset));

        Ok(())
    }

    #[tracing::instrument]
    pub fn run(mut self) -> anyhow::Result<()> {
        self.new_paths.set_position(0);

        let mut new_paths = Vec::new();
        self.new_paths.read_to_end(&mut new_paths)?;

        let (cases_ptr, length, _) = self.cases.clone().into_raw_parts();
        let size = length * std::mem::size_of::<ReplaceCase>();
        let cases = unsafe { std::slice::from_raw_parts(cases_ptr as *mut u8, size) };

        self.process.run_codes(|addr| {
            let mut vec_rt =
                dynasmrt::VecAssembler::<dynasmrt::x64::X64Relocation>::new(addr as usize);
            dynasm!(vec_rt
                ; .arch x64
                ; ->cases:
                ; .bytes cases
                ; ->cases_length:
                ; .qword cases.len() as i64
                ; ->new_paths:
                ; .bytes new_paths.as_slice()
            );

            trace!("static bytes placed");
            let replace = vec_rt.offset();
            dynasm!(vec_rt
                ; .arch x64
                // set r15 to 0
                ; xor r15, r15
                ; lea r14, [-> cases]

                ; jmp ->end
                ; ->start:
                // fcntl
                ; mov rax, 0x48
                ; mov rdi, QWORD [r14+r15] // fd
                ; mov rsi, 0x3
                ; mov rdx, 0x0
                ; syscall
                ; mov rsi, rax
                // open
                ; mov rax, 0x2
                ; lea rdi, [-> new_paths]
                ; add rdi, QWORD [r14+r15+8] // path
                ; mov rdx, 0x0
                ; syscall
                ; push rax
                ; mov rdi, rax
                // dup2
                ; mov rax, 0x21
                ; mov rsi, QWORD [r14+r15] // fd
                ; syscall
                // close
                ; mov rax, 0x3
                ; pop rdi
                ; syscall

                ; add r15, 0x10
                ; ->end:
                ; mov r13, QWORD [->cases_length]
                ; cmp r15, r13
                ; jb ->start

                ; int3
            );

            let instructions = vec_rt.finalize()?;
            let mut log_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open("/tmp/code.log")?;
            log_file.write_all(&instructions[replace.0..])?;
            trace!("write file to /tmp/code.log");

            Ok((replace.0 as u64, instructions))
        })?;

        trace!("reopen successfully");
        Ok(())
    }
}

pub struct FdReplacer {
    processes: HashMap<i32, ProcessAccesser>,
}

impl FdReplacer {
    #[tracing::instrument(skip(detect_path, new_path))]
    pub fn prepare<P1: AsRef<Path>, P2: AsRef<Path>>(
        detect_path: P1,
        new_path: P2,
    ) -> Result<FdReplacer> {
        let pids = utils::iter_pids()?;

        let mut processes = HashMap::new();
        for pid in pids {
            let entries = match read_dir(format!("/proc/{}/fd", pid)) {
                Ok(entries) => entries,
                Err(err) => {
                    warn!("fail to read /proc/{}/fd: {:?}", pid, err);
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(err) => {
                        warn!("fail to read entry {:?}", err);
                        continue;
                    }
                };

                let fd = entry
                    .file_name()
                    .to_str()
                    .ok_or(anyhow!("fd contains non-UTF-8 character"))?
                    .parse()?;

                let path = entry.path();
                if let Ok(path) = read_link(&path) {
                    if path.starts_with(detect_path.as_ref()) {
                        let process = processes.entry(pid).or_insert_with(|| {
                            // TODO: handle error here
                            ProcessAccesser::prepare(pid).unwrap()
                        });

                        let stripped_path = path.strip_prefix(&detect_path)?;
                        process.push_case(fd, new_path.as_ref().join(stripped_path))?;
                    }
                }
            }
        }

        Ok(FdReplacer { processes })
    }

    #[tracing::instrument(skip(self))]
    pub fn run(&mut self) -> Result<()> {
        for (_, accesser) in self.processes.drain() {
            accesser.run()?;
        }

        Ok(())
    }
}

impl Drop for ProcessAccesser {
    #[tracing::instrument(skip(self))]
    fn drop(&mut self) {
        self.process
            .detach()
            .unwrap_or_else(|err| panic!("fails to detach process {}: {}", self.process.pid, err))
    }
}
