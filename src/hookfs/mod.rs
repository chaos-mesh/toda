use anyhow::Result;
use fuse::{FileAttr, FileType, Filesystem};
use time::{get_time, Timespec};

use libc::{getxattr};

use nix::fcntl::{open, OFlag};
use nix::sys::stat;
use nix::unistd::{lseek, read, Whence, AccessFlags};
use nix::dir;

use tracing::{debug, trace};

use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::os::unix::ffi::OsStrExt;
use std::cell::RefCell;
use std::ffi::OsStr;

#[derive(Clone, Debug)]
pub struct HookFs {
    mount_path: PathBuf,
    original_path: PathBuf,

    files_counter: usize,
    opened_files: HashMap<usize, RawFd>,

    dirs_counter: usize,
    opened_dirs: HashMap<usize, RefCell<dir::Dir>>,

    // map from inode to real path
    inode_map: HashMap<u64, PathBuf>,
}

impl HookFs {
    pub fn new<P1: AsRef<Path>, P2: AsRef<Path>>(mount_path: P1, original_path: P2) -> HookFs {
        let mut inode_map = HashMap::new();
        inode_map.insert(1, original_path.as_ref().to_owned());

        return HookFs {
            mount_path: mount_path.as_ref().to_owned(),
            original_path: original_path.as_ref().to_owned(),
            files_counter: 0,
            opened_files: HashMap::new(),
            dirs_counter: 0,
            opened_dirs: HashMap::new(),
            inode_map,
        };
    }
}

fn convert_filetype(file_type: dir::Type) -> FileType {
    match file_type {
        dir::Type::Fifo => FileType::NamedPipe,
        dir::Type::CharacterDevice => FileType::CharDevice,
        dir::Type::Directory => FileType::Directory,
        dir::Type::BlockDevice => FileType::BlockDevice,
        dir::Type::File => FileType::RegularFile,
        dir::Type::Symlink => FileType::Symlink,
        dir::Type::Socket => FileType::Socket,
    }
}

// convert_libc_stat_to_fuse_stat converts file stat from libc form into fuse form.
// returns None if the file type is unknown.
fn convert_libc_stat_to_fuse_stat(stat: libc::stat) -> Option<FileAttr> {
    let kind = match stat.st_mode & libc::S_IFMT {
        libc::S_IFBLK => FileType::BlockDevice,
        libc::S_IFCHR => FileType::CharDevice,
        libc::S_IFDIR => FileType::Directory,
        libc::S_IFIFO => FileType::NamedPipe,
        libc::S_IFLNK => FileType::Symlink,
        libc::S_IFREG => FileType::RegularFile,
        libc::S_IFSOCK => FileType::Socket,
        _ => return None,
    };
    return Some(FileAttr {
        ino: stat.st_ino,
        size: stat.st_size as u64,
        blocks: stat.st_blocks as u64,
        atime: Timespec::new(stat.st_atime, stat.st_atime_nsec as i32),
        mtime: Timespec::new(stat.st_mtime, stat.st_mtime_nsec as i32),
        ctime: Timespec::new(stat.st_ctime, stat.st_ctime_nsec as i32),
        kind,
        perm: (stat.st_mode & 0o777) as u16,
        nlink: stat.st_nlink as u32,
        uid: stat.st_uid,
        gid: stat.st_gid,
        rdev: stat.st_rdev as u32,
        crtime: Timespec::new(0, 0), // It's macOS only
        flags: 0,                    // It's macOS only
    });
}

impl Filesystem for HookFs {
    #[tracing::instrument]
    fn init(&mut self, req: &fuse::Request) -> Result<(), nix::libc::c_int> {
        trace!("FUSE init");
        Ok(())
    }
    #[tracing::instrument]
    fn destroy(&mut self, req: &fuse::Request) {
        trace!("FUSE destroy");
    }
    #[tracing::instrument]
    fn lookup(
        &mut self,
        _req: &fuse::Request,
        _parent: u64,
        name: &std::ffi::OsStr,
        reply: fuse::ReplyEntry,
    ) {
        let time = get_time();

        let mut source_mount = self.original_path.clone();
        source_mount.push(name);
        match stat::stat(&source_mount) {
            Ok(stat) => {
                match convert_libc_stat_to_fuse_stat(stat) {
                    Some(stat) => {
                        self.inode_map.insert(stat.ino, source_mount);
                        // TODO: support generation number
                        // this can be implemented with ioctl FS_IOC_GETVERSION
                        trace!("return with {:?}", stat);
                        reply.entry(&time, &stat, 0);
                    }
                    None => {
                        trace!("return with errno: -1");
                        reply.error(-1) // TODO: set it with UNKNOWN FILE TYPE errno
                    }
                }
            }
            Err(err) => {
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                trace!("return with errno: {}", errno);
                reply.error(errno);
            }
        }
    }
    #[tracing::instrument]
    fn forget(&mut self, req: &fuse::Request, ino: u64, nlookup: u64) {
        debug!("umimplemented forget");
    }
    #[tracing::instrument]
    fn getattr(&mut self, req: &fuse::Request, ino: u64, reply: fuse::ReplyAttr) {
        let time = get_time();
        let path = self.inode_map[&ino].as_path();

        match stat::stat(path) {
            Ok(stat) => {
                match convert_libc_stat_to_fuse_stat(stat) {
                    Some(stat) => {
                        trace!("return with {:?}", stat);
                        reply.attr(&time, &stat)
                    }
                    None => {
                        trace!("return with errno: -1");
                        reply.error(-1) // TODO: set it with UNKNOWN FILE TYPE errno
                    }
                }
            }
            Err(err) => {
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                trace!("return with errno: {}", errno);
                reply.error(errno);
            }
        }
    }
    #[tracing::instrument]
    fn setattr(
        &mut self,
        req: &fuse::Request,
        _ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        _size: Option<u64>,
        _atime: Option<Timespec>,
        _mtime: Option<Timespec>,
        _fh: Option<u64>,
        _crtime: Option<Timespec>,
        _chgtime: Option<Timespec>,
        _bkuptime: Option<Timespec>,
        _flags: Option<u32>,
        reply: fuse::ReplyAttr,
    ) {
        debug!("unimplimented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn readlink(&mut self, req: &fuse::Request, ino: u64, reply: fuse::ReplyData) {
        debug!("unimplimented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn mknod(
        &mut self,
        req: &fuse::Request,
        _parent: u64,
        _name: &std::ffi::OsStr,
        _mode: u32,
        _rdev: u32,
        reply: fuse::ReplyEntry,
    ) {
        debug!("unimplimented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn mkdir(
        &mut self,
        _req: &fuse::Request,
        _parent: u64,
        _name: &std::ffi::OsStr,
        _mode: u32,
        reply: fuse::ReplyEntry,
    ) {
        debug!("unimplimented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn unlink(
        &mut self,
        _req: &fuse::Request,
        _parent: u64,
        _name: &std::ffi::OsStr,
        reply: fuse::ReplyEmpty,
    ) {
        debug!("unimplimented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn rmdir(
        &mut self,
        _req: &fuse::Request,
        _parent: u64,
        _name: &std::ffi::OsStr,
        reply: fuse::ReplyEmpty,
    ) {
        debug!("unimplimented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn symlink(
        &mut self,
        _req: &fuse::Request,
        _parent: u64,
        _name: &std::ffi::OsStr,
        _link: &std::path::Path,
        reply: fuse::ReplyEntry,
    ) {
        debug!("unimplimented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn rename(
        &mut self,
        _req: &fuse::Request,
        _parent: u64,
        _name: &std::ffi::OsStr,
        _newparent: u64,
        _newname: &std::ffi::OsStr,
        reply: fuse::ReplyEmpty,
    ) {
        debug!("unimplimented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn link(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        _newparent: u64,
        _newname: &std::ffi::OsStr,
        reply: fuse::ReplyEntry,
    ) {
        debug!("unimplimented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn open(&mut self, _req: &fuse::Request, ino: u64, flags: u32, reply: fuse::ReplyOpen) {
        // filter out append. The kernel layer will translate the
        // offsets for us appropriately.
        let filtered_flags = flags & (!(libc::O_APPEND as u32)) & (!0x8000); // 0x8000 is magic
        let filtered_flags = match OFlag::from_bits(filtered_flags as i32) {
            Some(flags) => flags,
            None => {
                reply.error(-1); // TODO: set errno to unknown flags
                return;
            }
        };

        if let Some(path) = self.inode_map.get(&ino) {
            match open(path, filtered_flags, stat::Mode::all()) {
                Ok(fd) => {
                    let id = self.files_counter;
                    self.files_counter += 1;

                    self.opened_files.insert(id, fd);
                    let fh = (self.opened_files.len() - 1) as u64;

                    trace!("return with fh: {}, flags: {}", fh, filtered_flags.bits());

                    // TODO: figure out which flag should be set here
                    reply.opened(fh, flags)
                }
                Err(err) => {
                    trace!("return with err: {}", err);
                    let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                    reply.error(errno)
                }
            }
        } else {
            trace!("return with errno: -1");
            reply.error(-1) // TODO: set errno to special value that no inode found
        }
    }
    #[tracing::instrument]
    fn read(
        &mut self,
        req: &fuse::Request,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        reply: fuse::ReplyData,
    ) {
        let fd: RawFd = self.opened_files[&(fh as usize)];
        if let Err(err) = lseek(fd, offset, Whence::SeekSet) {
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);
            return;
        }

        let mut buf = Vec::new();
        buf.resize(size as usize, 0);
        if let Err(err) = read(fd, &mut buf) {
            trace!("return with err: {}", err);
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);
            return;
        };
        trace!("return with data: {:?}", buf);
        reply.data(&buf)
    }
    #[tracing::instrument]
    fn write(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        _fh: u64,
        _offset: i64,
        _data: &[u8],
        _flags: u32,
        reply: fuse::ReplyWrite,
    ) {
        debug!("unimplemented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn flush(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        reply: fuse::ReplyEmpty,
    ) {
        debug!("unimplemented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn release(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        _fh: u64,
        _flags: u32,
        _lock_owner: u64,
        _flush: bool,
        reply: fuse::ReplyEmpty,
    ) {
        debug!("unimplemented");
        reply.ok();
    }
    #[tracing::instrument]
    fn fsync(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: fuse::ReplyEmpty,
    ) {
        debug!("unimplemented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn opendir(&mut self, req: &fuse::Request, ino: u64, flags: u32, reply: fuse::ReplyOpen) {
        let path = self.inode_map[&ino].as_path();

        let filtered_flags = flags & (!(libc::O_APPEND as u32)) & (!0x8000);
        let filtered_flags = match OFlag::from_bits(filtered_flags as i32) {
            Some(flags) => flags,
            None => {
                reply.error(-1); // TODO: set errno to unknown flags
                return;
            }
        };
        
        let dir = match dir::Dir::open(path, filtered_flags, stat::Mode::all()) {
            Ok(dir) => dir,
            Err(err) => {
                trace!("return with error: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);
                return;
            }
        };
        let id = self.dirs_counter;
        self.dirs_counter += 1;

        self.opened_dirs.insert(id, RefCell::new(dir));
        trace!("return with fh: {}, flags: {}", id, flags);
        reply.opened(id as u64, flags);
    }
    #[tracing::instrument]
    fn readdir(
        &mut self,
        req: &fuse::Request,
        ino: u64,
        fh: u64,
        offset: i64,
        mut reply: fuse::ReplyDirectory,
    ) {
        let offset = offset as usize;
        let dir = &self.opened_dirs[&(fh as usize)];

        // TODO: optimize the implementation
        let all_entries: Vec<_> = dir.borrow_mut().iter().collect();
        if offset >= all_entries.len() {
            trace!("empty reply");
            reply.ok();
            return;
        }
        for (index, entry) in all_entries.iter().enumerate().skip(offset as usize) {
            match entry {
                Ok(entry) => {
                    let name = entry.file_name();
                    let name = OsStr::from_bytes(name.to_bytes());

                    let file_type = match entry.file_type() {
                        Some(file_type) => convert_filetype(file_type),
                        None => {
                            debug!("unknown file_type");
                            reply.error(-1);
                            return;
                        }
                    };
                    if !reply.add(entry.ino(), (index + 1) as i64, file_type, name) {
                        trace!("add file {:?}", entry);
                    } else {
                        trace!("buffer is full");
                        break;
                    }
                }
                Err(err) => {
                    trace!("return with error: {}", err);
                    let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                    reply.error(errno);
                    return;
                }
            }
        }

        trace!("iterated all files");
        reply.ok();
        return;
    }
    #[tracing::instrument]
    fn releasedir(
        &mut self,
        req: &fuse::Request,
        _ino: u64,
        fh: u64,
        _flags: u32,
        reply: fuse::ReplyEmpty,
    ) {
        self.opened_dirs.remove(&(fh as usize));
        reply.ok();
    }
    #[tracing::instrument]
    fn fsyncdir(
        &mut self,
        req: &fuse::Request,
        _ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: fuse::ReplyEmpty,
    ) {
        debug!("unimplemented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn statfs(&mut self, req: &fuse::Request, _ino: u64, reply: fuse::ReplyStatfs) {
        debug!("unimplemented");
        reply.statfs(0, 0, 0, 0, 0, 512, 255, 0);
    }
    #[tracing::instrument]
    fn setxattr(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        _name: &std::ffi::OsStr,
        _value: &[u8],
        _flags: u32,
        _position: u32,
        reply: fuse::ReplyEmpty,
    ) {
        debug!("unimplemented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn getxattr(
        &mut self,
        req: &fuse::Request,
        ino: u64,
        name: &std::ffi::OsStr,
        size: u32,
        reply: fuse::ReplyXattr,
    ) {
        let path = self.inode_map[&ino].as_path();

        let path_ptr = path.as_os_str().as_bytes() as *const [u8] as *const libc::c_char;
        let name_ptr = name.as_bytes() as *const [u8] as *const libc::c_char;

        let mut buf = Vec::new();
        buf.resize(size as usize, 0u8);
        let buf_ptr = buf.as_mut_slice() as *mut [u8] as *mut libc::c_void;

        let ret = unsafe {
            getxattr(path_ptr, name_ptr, buf_ptr, size as usize)
        };

        if ret == -1 {
            let err = nix::Error::last();
            trace!("return with err: {}", err);
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);

            return
        }

        if size == 0 {
            debug!("may error because of 0 size in getxattr");
            trace!("return with size {}", ret);
            reply.size(ret as u32);
        } else {
            trace!("return with data {:?}", buf);
            reply.data(buf.as_slice());
        }
    }
    #[tracing::instrument]
    fn listxattr(&mut self, req: &fuse::Request, _ino: u64, _size: u32, reply: fuse::ReplyXattr) {
        debug!("unimplemented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn removexattr(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        _name: &std::ffi::OsStr,
        reply: fuse::ReplyEmpty,
    ) {
        debug!("unimplemented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn access(&mut self, req: &fuse::Request, ino: u64, mask: u32, reply: fuse::ReplyEmpty) {
        let path = self.inode_map[&ino].as_path();
        
        let mask = match AccessFlags::from_bits(mask as i32) {
            Some(mask) => mask,
            None => {
                trace!("unknown mask {}", mask);
                reply.error(-1);
                return
            }
        };
        if let Err(err) = nix::unistd::access(path, mask) {
            match err.as_errno() {
                Some(errno) => {
                    reply.error(errno as i32)
                }
                None => {
                    trace!("unknown error {}", err);
                    reply.error(-1)
                }
            }
        } else {
            reply.ok()
        }
    }
    #[tracing::instrument]
    fn create(
        &mut self,
        _req: &fuse::Request,
        _parent: u64,
        _name: &std::ffi::OsStr,
        _mode: u32,
        _flags: u32,
        reply: fuse::ReplyCreate,
    ) {
        debug!("unimplemented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn getlk(
        &mut self,
        req: &fuse::Request,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        _start: u64,
        _end: u64,
        _typ: u32,
        _pid: u32,
        reply: fuse::ReplyLock,
    ) {
        debug!("unimplemented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn setlk(
        &mut self,
        req: &fuse::Request,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        _start: u64,
        _end: u64,
        _typ: u32,
        _pid: u32,
        _sleep: bool,
        reply: fuse::ReplyEmpty,
    ) {
        debug!("unimplemented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    fn bmap(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        _blocksize: u32,
        _idx: u64,
        reply: fuse::ReplyBmap,
    ) {
        debug!("unimplemented");
        reply.error(nix::libc::ENOSYS);
    }
}
