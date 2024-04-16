use std::collections::HashMap;
use std::fmt::Debug;
use std::io::{Cursor, Read, Write};
use std::iter::FromIterator;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use dynasmrt::{dynasm, DynasmApi, DynasmLabelApi};
use itertools::Itertools;
use nix::sys::mman::{MapFlags, ProtFlags};
use procfs::process::MMapPath;
use tracing::{error, info, trace};

use super::utils::all_processes;
use super::{ptrace, Replacer};

#[derive(Clone, Debug)]
struct ReplaceCase {
    pub memory_addr: u64,
    pub length: u64,
    pub prot: u64,
    pub flags: u64,
    pub path: PathBuf,
    pub offset: u64,
}

#[derive(Clone, Copy)]
#[repr(packed)]
#[repr(C)]
struct RawReplaceCase {
    memory_addr: u64,
    length: u64,
    prot: u64,
    flags: u64,
    new_path_offset: u64,
    offset: u64,
}

impl RawReplaceCase {
    pub fn new(
        memory_addr: u64,
        length: u64,
        prot: u64,
        flags: u64,
        new_path_offset: u64,
        offset: u64,
    ) -> RawReplaceCase {
        RawReplaceCase {
            memory_addr,
            length,
            prot,
            flags,
            new_path_offset,
            offset,
        }
    }
}

// TODO: encapsulate this struct for fd replacer and mmap replacer
struct ProcessAccessorBuilder {
    cases: Vec<RawReplaceCase>,
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

    pub fn push_case(
        &mut self,
        memory_addr: u64,
        length: u64,
        prot: u64,
        flags: u64,
        new_path: PathBuf,
        offset: u64,
    ) -> anyhow::Result<()> {
        info!("push case");

        let mut new_path = new_path
            .to_str()
            .ok_or(anyhow!("fd contains non-UTF-8 character"))?
            .as_bytes()
            .to_vec();

        new_path.push(0);

        let new_path_offset = self.new_paths.position();
        self.new_paths.write_all(new_path.as_slice())?;

        self.cases.push(RawReplaceCase::new(
            memory_addr,
            length,
            prot,
            flags,
            new_path_offset,
            offset,
        ));

        Ok(())
    }
}

impl FromIterator<ReplaceCase> for ProcessAccessorBuilder {
    fn from_iter<T: IntoIterator<Item = ReplaceCase>>(iter: T) -> Self {
        let mut builder = Self::new();
        for case in iter {
            if let Err(err) = builder.push_case(
                case.memory_addr,
                case.length,
                case.prot,
                case.flags,
                case.path,
                case.offset,
            ) {
                error!("fail to write to AccessorBuilder. Error: {:?}", err)
            }
        }

        builder
    }
}

struct ProcessAccessor {
    process: ptrace::TracedProcess,

    cases: Vec<RawReplaceCase>,
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
        let size = length * std::mem::size_of::<RawReplaceCase>();
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
                // munmap
                ; mov rax, 0x0B
                ; mov rdi, QWORD [r14+r15] // addr
                ; mov rsi, QWORD [r14+r15+8] // length
                ; mov rdx, 0x0
                ; push rdi
                ; syscall
                // open
                ; mov rax, 0x2

                ; lea rdi, [-> new_paths]
                ; add r15, 8 * 4 // set r15 to point to path
                ; add rdi, QWORD [r14+r15] // path
                ; sub r15, 8 * 4

                ; mov rsi, libc::O_RDWR
                ; mov rdx, 0x0
                ; syscall
                ; pop rdi // addr
                ; push rax
                ; mov r8, rax // fd
                // mmap
                ; mov rax, 0x9
                ; add r15, 8
                ; mov rsi, QWORD [r14+r15] // length
                ; add r15, 8
                ; mov rdx, QWORD [r14+r15] // prot
                ; add r15, 8
                ; mov r10, QWORD [r14+r15] // flags
                ; add r15, 16
                ; mov r9, QWORD [r14+r15] // offset
                ; syscall
                ; sub r15, 8 * 5
                // close
                ; mov rax, 0x3
                ; pop rdi
                ; syscall

                ; add r15, std::mem::size_of::<RawReplaceCase>() as i32
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

fn get_prot_and_flags_from_perms<S: AsRef<str>>(perms: S) -> (u64, u64) {
    let bytes = perms.as_ref().as_bytes();
    let mut prot = ProtFlags::empty();
    let mut flags = MapFlags::MAP_PRIVATE;

    if bytes[0] == b'r' {
        prot |= ProtFlags::PROT_READ
    }
    if bytes[1] == b'w' {
        prot |= ProtFlags::PROT_WRITE
    }
    if bytes[2] == b'x' {
        prot |= ProtFlags::PROT_EXEC
    }
    if bytes[3] == b's' {
        flags = MapFlags::MAP_SHARED;
    }

    trace!(
        "perms: {}, prot: {:?}, flags: {:?}",
        perms.as_ref(),
        prot,
        flags
    );
    (prot.bits() as u64, flags.bits() as u64)
}

pub struct MmapReplacer {
    processes: HashMap<i32, ProcessAccessor>,
}

impl MmapReplacer {
    pub fn prepare<P1: AsRef<Path>, P2: AsRef<Path>>(
        detect_path: P1,
        new_path: P2,
    ) -> Result<MmapReplacer> {
        info!("preparing mmap replacer");

        let detect_path = detect_path.as_ref();
        let new_path = new_path.as_ref();

        let processes = all_processes()?
            .filter_map(|process| -> Option<_> {
                let pid = process.pid;

                let traced_process = ptrace::trace(pid).ok()?;
                let maps = process.maps().ok()?;

                Some((traced_process, maps))
            })
            .flat_map(|(process, maps)| {
                maps.into_iter()
                    .filter_map(move |entry| {
                        match entry.pathname {
                            MMapPath::Path(path) => {
                                let (start_address, end_address) = entry.address;
                                let length = end_address - start_address;
                                let (prot, flags) = get_prot_and_flags_from_perms(entry.perms);
                                // TODO: extract permission from perms

                                let case = ReplaceCase {
                                    memory_addr: start_address,
                                    length,
                                    prot,
                                    flags,
                                    path,
                                    offset: entry.offset,
                                };
                                Some((process.clone(), case))
                            }
                            _ => None,
                        }
                    })
                    .filter(|(_, case)| case.path.starts_with(detect_path))
                    .filter_map(|(process, mut case)| {
                        let stripped_path = case.path.strip_prefix(&detect_path).ok()?;
                        case.path = new_path.join(stripped_path);
                        Some((process, case))
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

        Ok(MmapReplacer { processes })
    }
}

impl Replacer for MmapReplacer {
    fn run(&mut self) -> Result<()> {
        info!("running mmap replacer");
        for (_, accessor) in self.processes.iter_mut() {
            accessor.run()?;
        }

        Ok(())
    }
}
