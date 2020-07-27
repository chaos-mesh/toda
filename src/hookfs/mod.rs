use anyhow::Result;
use fuse::{FileAttr, FileType, Filesystem};
use time::{get_time, Timespec};

use libc::{getxattr, setxattr, listxattr, removexattr};

use nix::fcntl::{open, OFlag};
use nix::sys::stat;
use nix::sys::statfs;
use nix::sys::time::{TimeVal, TimeValLike};
use nix::unistd::{unlink, mkdir, write, fsync, lseek, read, Whence, AccessFlags, chown, Uid, Gid, truncate};
use nix::dir;

use tracing::{debug, trace};

use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::os::unix::ffi::OsStrExt;
use std::cell::RefCell;
use std::ffi::{OsStr, CString};

// use fuse::consts::FOPEN_DIRECT_IO;

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
    #[tracing::instrument(skip(_req))]
    fn init(&mut self, _req: &fuse::Request) -> Result<(), nix::libc::c_int> {
        trace!("FUSE init");
        Ok(())
    }
    #[tracing::instrument(skip(_req))]
    fn destroy(&mut self, _req: &fuse::Request) {
        trace!("FUSE destroy");
    }
    #[tracing::instrument(skip(_req))]
    fn lookup(
        &mut self,
        _req: &fuse::Request,
        parent: u64,
        name: &std::ffi::OsStr,
        reply: fuse::ReplyEntry,
    ) {
        let time = get_time();

        let parent_path = match self.inode_map.get(&parent) {
            Some(path) => path.as_path(),
            None => {
                reply.error(-1);
                return;
            },
        };
        let path = parent_path.join(name);
        match stat::stat(&path) {
            Ok(stat) => {
                match convert_libc_stat_to_fuse_stat(stat) {
                    Some(stat) => {
                        self.inode_map.insert(stat.ino, path);
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
                trace!("return with error: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);
            }
        }
    }
    #[tracing::instrument(skip(_req))]
    fn forget(&mut self, _req: &fuse::Request, ino: u64, nlookup: u64) {
        trace!("forget not implemented yet");
        // Maybe hookfs doesn't need forget
    }
    #[tracing::instrument(skip(_req))]
    fn getattr(&mut self, _req: &fuse::Request, ino: u64, reply: fuse::ReplyAttr) {
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
    #[tracing::instrument(skip(req))]
    fn setattr(
        &mut self,
        req: &fuse::Request,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<Timespec>,
        mtime: Option<Timespec>,
        _fh: Option<u64>,
        _crtime: Option<Timespec>,
        _chgtime: Option<Timespec>,
        _bkuptime: Option<Timespec>,
        _flags: Option<u32>,
        reply: fuse::ReplyAttr,
    ) {
        let path = match self.inode_map.get(&ino) {
            Some(path) => path.as_path(),
            None => {
                debug!("cannot find inode({}) in inode_map", ino);
                reply.error(-1);
                return
            }
        };

        if let Err(err) = chown(path, uid.map(|uid| Uid::from_raw(uid)), gid.map(|gid| Gid::from_raw(gid))) {
            trace!("return with error: {}", err);
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);
            return;
        }
        if let Some(mode) = mode {
            if let Err(err) = stat::fchmodat(None, path,  stat::Mode::from_bits_truncate(mode), stat::FchmodatFlags::FollowSymlink) {
                trace!("return with error: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);
                return;
            }
        }
        if let Some(size) = size {
            if let Err(err) = truncate(path, size as i64) {
                trace!("return with error: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);
                return;
            }
        }

        if let (Some(atime), Some(mtime)) = (atime, mtime) {
            let atime = TimeVal::seconds(atime.sec) + TimeVal::nanoseconds(atime.nsec as i64);
            let mtime = TimeVal::seconds(mtime.sec) + TimeVal::nanoseconds(mtime.nsec as i64);
            // TODO: check whether one of them is Some
            if let Err(err) = stat::utimes(path, &atime, &mtime) {
                trace!("return with error: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);
                return;
            }
        }
        
        self.getattr(req, ino, reply)
    }
    #[tracing::instrument(skip(_req))]
    fn readlink(&mut self, _req: &fuse::Request, ino: u64, reply: fuse::ReplyData) {
        debug!("unimplimented");
        
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument(skip(req))]
    fn mknod(
        &mut self,
        req: &fuse::Request,
        parent: u64,
        name: &std::ffi::OsStr,
        mode: u32,
        rdev: u32,
        reply: fuse::ReplyEntry,
    ) {
        let parent_path = match self.inode_map.get(&parent) {
            Some(path) => path.as_path(),
            None => {
                reply.error(-1);
                return;
            },
        };
        let path = parent_path.join(name);
        let path = match CString::new(path.as_os_str().as_bytes()) {
            Ok(path) => path,
            Err(_) => {
                // TODO: set better errno
                // path contains nul
                reply.error(-1);
                return
            }
        };
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const i8;

        trace!("mknod for {:?}", path);
        let ret = unsafe {
            libc::mknod(path_ptr, mode, rdev as u64)
        };
        if ret == -1 {
            let err = nix::Error::last();
            trace!("return with err: {}", err);
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);

            return
        }
        self.lookup(req, parent, name, reply);
    }
    #[tracing::instrument(skip(req))]
    fn mkdir(
        &mut self,
        req: &fuse::Request,
        parent: u64,
        name: &std::ffi::OsStr,
        mode: u32,
        reply: fuse::ReplyEntry,
    ) {
        let parent_path = match self.inode_map.get(&parent) {
            Some(path) => path.as_path(),
            None => {
                reply.error(-1);
                return;
            },
        };
        let path = parent_path.join(name);

        let mode = match stat::Mode::from_bits(mode) {
            Some(mode) => mode,
            None => {
                debug!("unavailable mode: {}", mode);
                reply.error(-1);
                return
            }
        };
        match mkdir(&path, mode) {
            Ok(_) => {
                self.lookup(req, parent, name, reply)
            }
            Err(err) => {
                trace!("return with err: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);

                return
            }
        }
    }
    #[tracing::instrument(skip(_req))]
    fn unlink(
        &mut self,
        _req: &fuse::Request,
        parent: u64,
        name: &std::ffi::OsStr,
        reply: fuse::ReplyEmpty,
    ) {
        let parent_path = match self.inode_map.get(&parent) {
            Some(path) => path.as_path(),
            None => {
                reply.error(-1);
                return;
            },
        };
        let path = parent_path.join(name);
        match unlink(&path) {
            Ok(_) => {
                reply.ok()
            }
            Err(err) => {
                trace!("return with err: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);

                return
            }
        }
    }
    #[tracing::instrument(skip(_req))]
    fn rmdir(
        &mut self,
        _req: &fuse::Request,
        parent: u64,
        name: &std::ffi::OsStr,
        reply: fuse::ReplyEmpty,
    ) {
        let parent_path = match self.inode_map.get(&parent) {
            Some(path) => path.as_path(),
            None => {
                reply.error(-1);
                return;
            },
        };
        let path = parent_path.join(name);

        let path = match CString::new(path.as_os_str().as_bytes()) {
            Ok(path) => path,
            Err(_) => {
                // TODO: set better errno
                // path contains nul
                reply.error(-1);
                return
            }
        };
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let ret = unsafe {libc::rmdir(path_ptr) };

        if ret == -1 {
            let err = nix::Error::last();
            trace!("return with err: {}", err);
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);
        } else {
            reply.ok();
        }

    }
    #[tracing::instrument(skip(_req))]
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
    #[tracing::instrument(skip(_req))]
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
    #[tracing::instrument(skip(_req))]
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
    #[tracing::instrument(skip(_req))]
    fn open(&mut self, _req: &fuse::Request, ino: u64, flags: u32, reply: fuse::ReplyOpen) {
        // filter out append. The kernel layer will translate the
        // offsets for us appropriately.
        let filtered_flags = flags & (!(libc::O_APPEND as u32));
        let filtered_flags = OFlag::from_bits_truncate(filtered_flags as i32);

        if let Some(path) = self.inode_map.get(&ino) {
            match open(path, filtered_flags, stat::Mode::S_IRWXU) {
                Ok(fd) => {
                    let fh = self.files_counter;
                    self.files_counter += 1;
                    self.opened_files.insert(fh, fd);

                    trace!("return with fh: {}, flags: {}", fh, 0);

                    // TODO: force DIRECT_IO is not a great option
                    reply.opened(fh as u64, 0)
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
    #[tracing::instrument(skip(_req))]
    fn read(
        &mut self,
        _req: &fuse::Request,
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
    #[tracing::instrument(skip(_req))]
    fn write(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _flags: u32,
        reply: fuse::ReplyWrite,
    ) {
        let fd = match self.opened_files.get(&(fh as usize)) {
            Some(fd) => *fd,
            None => {
                trace!("cannot find fh {}", fh);
                reply.error(-1);
                return
            }
        };

        if let Err(err) = lseek(fd, offset, Whence::SeekSet) {
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);
            return;
        }

        match write(fd, data) {
            Ok(size) => {
                reply.written(size as u32)
            }
            Err(err) => {
                trace!("return with err: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);
                return;
            }
        }
    }
    #[tracing::instrument(skip(_req))]
    fn flush(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        fh: u64,
        _lock_owner: u64,
        reply: fuse::ReplyEmpty,
    ) {
        // flush is implemented with fsync. Is it the correct way?
        if let Some(fd) = self.opened_files.get(&(fh as usize)) {
            if let Err(err) = fsync(*fd) {
                trace!("return with err: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno)
            } else {
                reply.ok()
            }
        }
    }
    #[tracing::instrument(skip(_req))]
    fn release(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        fh: u64,
        _flags: u32,
        _lock_owner: u64,
        _flush: bool,
        reply: fuse::ReplyEmpty,
    ) {
        self.opened_files.remove(&(fh as usize));
        reply.ok();
    }
    #[tracing::instrument(skip(_req))]
    fn fsync(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        fh: u64,
        _datasync: bool,
        reply: fuse::ReplyEmpty,
    ) {
        if let Some(fd) = self.opened_files.get(&(fh as usize)) {
            if let Err(err) = fsync(*fd) {
                trace!("return with err: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno)
            } else {
                reply.ok()
            }
        }
    }
    #[tracing::instrument(skip(_req))]
    fn opendir(&mut self, _req: &fuse::Request, ino: u64, flags: u32, reply: fuse::ReplyOpen) {
        let path = self.inode_map[&ino].as_path();

        let filtered_flags = flags & (!(libc::O_APPEND as u32));
        let filtered_flags = OFlag::from_bits_truncate(filtered_flags as i32);
        
        let dir = match dir::Dir::open(path, filtered_flags, stat::Mode::S_IRWXU) {
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
    #[tracing::instrument(skip(_req))]
    fn readdir(
        &mut self,
        _req: &fuse::Request,
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
    #[tracing::instrument(skip(_req))]
    fn releasedir(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        fh: u64,
        _flags: u32,
        reply: fuse::ReplyEmpty,
    ) {
        self.opened_dirs.remove(&(fh as usize));
        reply.ok();
    }
    #[tracing::instrument(skip(_req))]
    fn fsyncdir(
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
    #[tracing::instrument(skip(_req))]
    fn statfs(&mut self, _req: &fuse::Request, _ino: u64, reply: fuse::ReplyStatfs) {
        match statfs::statfs(&self.original_path) {
            Ok(stat) => {
                // return f_bsize as f_frsize
                // it's fine for linux in most case, but it's still better to fix it.
                reply.statfs(stat.blocks(), stat.blocks_free(), stat.blocks_available(), stat.files(), stat.files_free(), stat.block_size() as u32, stat.maximum_name_length() as u32, stat.block_size() as u32);
            }
            Err(err) => {
                trace!("return with error: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);
                return;
            }
        }
    }
    #[tracing::instrument(skip(_req))]
    fn setxattr(
        &mut self,
        _req: &fuse::Request,
        ino: u64,
        name: &std::ffi::OsStr,
        value: &[u8],
        flags: u32,
        _position: u32,
        reply: fuse::ReplyEmpty,
    ) {

        let path = match self.inode_map.get(&ino) {
            Some(path) => path.as_path(),
            None => {
                debug!("cannot find inode({}) in inode_map", ino);
                reply.error(-1);
                return
            }
        };

        let path = match CString::new(path.as_os_str().as_bytes()) {
            Ok(path) => path,
            Err(_) => {
                // TODO: set better errno
                // path contains nul
                reply.error(-1);
                return
            }
        };
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let name = match CString::new(name.as_bytes()) {
            Ok(name) => name,
            Err(_) => {
                // TODO: set better errno
                // path contains nul
                reply.error(-1);
                return
            }
        };
        let name_ptr = &name.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let value_ptr = &value[0] as *const u8 as *const libc::c_void;

        let ret = unsafe {
            setxattr(path_ptr, name_ptr, value_ptr, value.len(), flags as i32)
        };

        if ret == -1 {
            let err = nix::Error::last();
            trace!("return with err: {}", err);
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);

            return
        }
        reply.ok()
    }
    #[tracing::instrument(skip(_req))]
    fn getxattr(
        &mut self,
        _req: &fuse::Request,
        ino: u64,
        name: &std::ffi::OsStr,
        size: u32,
        reply: fuse::ReplyXattr,
    ) {
        let path = match self.inode_map.get(&ino) {
            Some(path) => path.as_path(),
            None => {
                debug!("cannot find inode({}) in inode_map", ino);
                reply.error(-1);
                return
            }
        };

        let path = match CString::new(path.as_os_str().as_bytes()) {
            Ok(path) => path,
            Err(_) => {
                // TODO: set better errno
                // path contains nul
                reply.error(-1);
                return
            }
        };
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let name = match CString::new(name.as_bytes()) {
            Ok(name) => name,
            Err(_) => {
                // TODO: set better errno
                // path contains nul
                reply.error(-1);
                return
            }
        };
        let name_ptr = &name.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

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
    #[tracing::instrument(skip(_req))]
    fn listxattr(&mut self, _req: &fuse::Request, ino: u64, size: u32, reply: fuse::ReplyXattr) {
        let path = match self.inode_map.get(&ino) {
            Some(path) => path.as_path(),
            None => {
                debug!("cannot find inode({}) in inode_map", ino);
                reply.error(-1);
                return
            }
        };

        let path = match CString::new(path.as_os_str().as_bytes()) {
            Ok(path) => path,
            Err(_) => {
                // TODO: set better errno
                // path contains nul
                reply.error(-1);
                return
            }
        };
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let mut buf = Vec::new();
        buf.resize(size as usize, 0u8);
        let buf_ptr = buf.as_mut_slice() as *mut [u8] as *mut libc::c_char;

        let ret = unsafe {
            listxattr(path_ptr, buf_ptr, size as usize)
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
    #[tracing::instrument(skip(_req))]
    fn removexattr(
        &mut self,
        _req: &fuse::Request,
        ino: u64,
        name: &std::ffi::OsStr,
        reply: fuse::ReplyEmpty,
    ) {
        let path = match self.inode_map.get(&ino) {
            Some(path) => path.as_path(),
            None => {
                debug!("cannot find inode({}) in inode_map", ino);
                reply.error(-1);
                return
            }
        };

        let path = match CString::new(path.as_os_str().as_bytes()) {
            Ok(path) => path,
            Err(_) => {
                // TODO: set better errno
                // path contains nul
                reply.error(-1);
                return
            }
        };
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let name = match CString::new(name.as_bytes()) {
            Ok(name) => name,
            Err(_) => {
                // TODO: set better errno
                // path contains nul
                reply.error(-1);
                return
            }
        };
        let name_ptr = &name.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let ret = unsafe {
            removexattr(path_ptr, name_ptr)
        };

        if ret == -1 {
            let err = nix::Error::last();
            trace!("return with err: {}", err);
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);

            return
        }
        reply.ok()
    }
    #[tracing::instrument(skip(_req))]
    fn access(&mut self, _req: &fuse::Request, ino: u64, mask: u32, reply: fuse::ReplyEmpty) {
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
            trace!("return with error: {}", err);
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);
        } else {
            reply.ok()
        }
    }
    #[tracing::instrument(skip(_req))]
    fn create(
        &mut self,
        _req: &fuse::Request,
        parent: u64,
        name: &std::ffi::OsStr,
        mode: u32,
        flags: u32,
        reply: fuse::ReplyCreate,
    ) {
        let parent_path = match self.inode_map.get(&parent) {
            Some(path) => path.as_path(),
            None => {
                reply.error(-1);
                return;
            },
        };
        let path = parent_path.join(name);

        let filtered_flags = flags & (!(libc::O_APPEND as u32));
        let filtered_flags = OFlag::from_bits_truncate(filtered_flags as i32);

        let mode = stat::Mode::from_bits_truncate(mode);

        trace!("create with flags: {:?}, mode: {:?}", filtered_flags, mode);
        match open(&path, filtered_flags, mode) {
            Ok(fd) => {
                match stat::stat(&path) {
                    Ok(stat) => {
                        match convert_libc_stat_to_fuse_stat(stat) {
                            Some(stat) => {
                                self.inode_map.insert(stat.ino, path);

                                let time = get_time();

                                let fh = self.files_counter;
                                self.files_counter += 1;
                                self.opened_files.insert(fh, fd);

                                // TODO: support generation number
                                // this can be implemented with ioctl FS_IOC_GETVERSION
                                trace!("return with stat: {:?} fh: {}", stat, fh);

                                reply.created(&time, &stat, 0, fh as u64, flags);
                            }
                            None => {
                                trace!("return with errno: -1");
                                reply.error(-1) // TODO: set it with UNKNOWN FILE TYPE errno
                            }
                        }
                    }
                    Err(err) => {
                        trace!("return with error: {}", err);
                        let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                        reply.error(errno);
                    }
                }
            }
            Err(err) => {
                trace!("return with error: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);
            }
        }
    }
    #[tracing::instrument(skip(_req))]
    fn getlk(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        fh: u64,
        _lock_owner: u64,
        start: u64,
        end: u64,
        typ: u32,
        pid: u32,
        reply: fuse::ReplyLock,
    ) {
        // kernel will implement for hookfs
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument(skip(_req))]
    fn setlk(
        &mut self,
        _req: &fuse::Request,
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
        // kernel will implement for hookfs
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument(skip(_req))]
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
