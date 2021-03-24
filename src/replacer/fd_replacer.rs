use std::collections::HashMap;
use std::fmt::Debug;
use std::io::{Cursor, Read, Write};
use std::iter::FromIterator;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use dynasmrt::{dynasm, DynasmApi, DynasmLabelApi};
use itertools::Itertools;
use procfs::process::FDTarget;
use tracing::{error, info, trace};

use super::utils::all_processes;
use super::{ptrace, Replacer};

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

struct ProcessAccessorBuilder {
    cases: Vec<ReplaceCase>,
    new_paths: Cursor<Vec<u8>>,
}

impl ProcessAccessorBuilder {
    pub fn new() -> ProcessAccessorBuilder {
        ProcessAccessorBuilder {
            cases: Vec::new(),
            new_paths: Cursor::new(Vec::new()),
        }
    }

    pub fn build(self, process: ptrace::TracedProcess) -> Result<ProcessAccessor> {
        Ok(ProcessAccessor {
            process,

            cases: self.cases,
            new_paths: self.new_paths,
        })
    }

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
}

impl FromIterator<(u64, PathBuf)> for ProcessAccessorBuilder {
    fn from_iter<T: IntoIterator<Item = (u64, PathBuf)>>(iter: T) -> Self {
        let mut builder = Self::new();
        for (fd, path) in iter {
            if let Err(err) = builder.push_case(fd, path) {
                error!("fail to write to AccessorBuilder. Error: {:?}", err)
            }
        }

        builder
    }
}

struct ProcessAccessor {
    process: ptrace::TracedProcess,

    cases: Vec<ReplaceCase>,
    new_paths: Cursor<Vec<u8>>,
}

impl Debug for ProcessAccessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.process.fmt(f)
    }
}

impl ProcessAccessor {
    pub fn run(&mut self) -> anyhow::Result<()> {
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
                ; nop
                ; nop
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
                ; mov r12, rax // store newly opened fd in r12
                // lseek
                ; mov rax, 0x8
                ; mov rdi, QWORD [r14+r15] // fd
                ; mov rsi, 0
                ; mov rdx, libc::SEEK_CUR
                ; syscall
                ; mov rdi, r12
                ; mov rsi, rax
                // lseek
                ; mov rax, 0x8
                ; mov rdx, libc::SEEK_SET
                ; syscall
                // dup2
                ; mov rax, 0x21
                ; mov rdi, r12
                ; mov rsi, QWORD [r14+r15] // fd
                ; syscall
                // close
                ; mov rax, 0x3
                ; mov rdi, r12
                ; syscall

                ; add r15, std::mem::size_of::<ReplaceCase>() as i32
                ; ->end:
                ; mov r13, QWORD [->cases_length]
                ; cmp r15, r13
                ; jb ->start

                ; int3
            );

            let instructions = vec_rt.finalize()?;

            Ok((replace.0 as u64, instructions))
        })?;

        trace!("reopen successfully");
        Ok(())
    }
}

pub struct FdReplacer {
    processes: HashMap<i32, ProcessAccessor>,
}

impl FdReplacer {
    pub fn prepare<P1: AsRef<Path>, P2: AsRef<Path>>(
        detect_path: P1,
        new_path: P2,
    ) -> Result<FdReplacer> {
        info!("preparing fd replacer");

        let detect_path = detect_path.as_ref();
        let new_path = new_path.as_ref();

        let processes = all_processes()?
            .filter_map(|process| -> Option<_> {
                let pid = process.pid;

                let traced_process = match ptrace::trace(pid) {
                    Ok(p) => p,
                    Err(err) => {
                        error!("fail to trace process: {} {}", pid, err);
                        return None;
                    }
                };
                let fd = process.fd().ok()?;

                Some((traced_process, fd))
            })
            .flat_map(|(process, fd)| {
                fd.into_iter()
                    .filter_map(|entry| match entry.target {
                        FDTarget::Path(path) => Some((entry.fd as u64, path)),
                        _ => None,
                    })
                    .filter(|(_, path)| path.starts_with(detect_path))
                    .filter_map(move |(fd, path)| {
                        trace!("replace fd({}): {}", fd, path.display());
                        let stripped_path = path.strip_prefix(&detect_path).ok()?;
                        Some((process.clone(), (fd, new_path.join(stripped_path))))
                    })
            })
            .group_by(|(process, _)| process.pid)
            .into_iter()
            .filter_map(|(pid, group)| Some((ptrace::trace(pid).ok()?, group)))
            .map(|(process, group)| (process, group.map(|(_, group)| group)))
            .filter_map(|(process, group)| {
                let pid = process.pid;
                match group.collect::<ProcessAccessorBuilder>().build(process) {
                    Ok(accessor) => Some((pid, accessor)),
                    Err(err) => {
                        error!("fail to build accessor: {:?}", err);
                        None
                    }
                }
            })
            .collect();

        Ok(FdReplacer { processes })
    }
}

impl Replacer for FdReplacer {
    fn run(&mut self) -> Result<()> {
        info!("running fd replacer");
        for (_, accessor) in self.processes.iter_mut() {
            accessor.run()?;
        }

        Ok(())
    }
}
