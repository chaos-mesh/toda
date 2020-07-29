mod async_fs;
mod errors;
mod reply;

use async_trait::async_trait;
use fuse::*;
use time::{get_time, Timespec};

use libc::{lgetxattr, llistxattr, lremovexattr, lsetxattr};

use nix::dir;
use nix::errno::Errno;
use nix::fcntl::{open, readlink, OFlag};
use nix::sys::stat;
use nix::sys::statfs;
use nix::sys::time::{TimeVal, TimeValLike};
use nix::unistd::{
    chown, fsync, linkat, lseek, mkdir, read, symlinkat, truncate, unlink, write, AccessFlags, Gid,
    LinkatFlags, Uid, Whence,
};

use tracing::{debug, error, trace};

use std::collections::HashMap;
use std::ffi::{CString, OsStr, OsString};
use std::ops::{Deref, DerefMut};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use async_fs::{AsyncFileSystem, AsyncFileSystemImpl};
pub use errors::{HookFsError as Error, Result};
use reply::*;

use tokio::sync::RwLock;

// use fuse::consts::FOPEN_DIRECT_IO;

#[derive(Debug)]
struct CounterMap<T> {
    map: HashMap<usize, T>,
    counter: usize,
}

impl<T> CounterMap<T> {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            counter: 0,
        }
    }

    pub fn insert(&mut self, item: T) -> usize {
        self.map.insert(self.counter, item);
        self.counter += 1;

        self.counter - 1
    }

    pub fn get(&self, key: usize) -> Option<&T> {
        self.map.get(&key)
    }

    pub fn get_mut(&mut self, key: usize) -> Option<&mut T> {
        self.map.get_mut(&key)
    }

    pub fn delete(&mut self, key: usize) -> Option<T> {
        self.map.remove(&key)
    }
}

#[derive(Clone, Debug)]
pub struct HookFs {
    mount_path: Arc<PathBuf>,
    original_path: Arc<PathBuf>,

    opened_files: Arc<RwLock<CounterMap<RawFd>>>,

    opened_dirs: Arc<RwLock<CounterMap<Dir>>>,

    // map from inode to real path
    inode_map: Arc<RwLock<HashMap<u64, PathBuf>>>,
}

#[derive(Debug)]
struct Dir(dir::Dir);

impl Deref for Dir {
    type Target = dir::Dir;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Dir {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<dir::Dir> for Dir {
    fn from(dir: dir::Dir) -> Self {
        Dir(dir)
    }
}

unsafe impl Send for Dir {}
unsafe impl Sync for Dir {}

impl HookFs {
    pub fn new<P1: AsRef<Path>, P2: AsRef<Path>>(mount_path: P1, original_path: P2) -> HookFs {
        let mut inode_map = HashMap::new();
        inode_map.insert(1, original_path.as_ref().to_owned());

        let inode_map = Arc::new(RwLock::new(inode_map));

        return HookFs {
            mount_path: Arc::new(mount_path.as_ref().to_owned()),
            original_path: Arc::new(original_path.as_ref().to_owned()),
            opened_files: Arc::new(RwLock::new(CounterMap::new())),
            opened_dirs: Arc::new(RwLock::new(CounterMap::new())),
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

#[async_trait]
impl AsyncFileSystemImpl for HookFs {
    #[tracing::instrument]
    async fn lookup(&self, parent: u64, name: OsString) -> Result<Entry> {
        trace!("lookup");

        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = inode_map
                .get(&parent)
                .ok_or(Error::InodeNotFound { inode: parent })?;
            parent_path.join(name)
        };

        let stat = stat::lstat(&path)?;

        let stat = convert_libc_stat_to_fuse_stat(stat).ok_or(Error::UnknownFileType)?;

        self.inode_map.write().await.insert(stat.ino, path);
        // TODO: support generation number
        // this can be implemented with ioctl FS_IOC_GETVERSION
        trace!("return with {:?}", stat);

        return Ok(Entry::new(stat, 0));
    }

    #[tracing::instrument]
    async fn forget(&self, _ino: u64, _nlookup: u64) {
        trace!("forget not implemented yet");
        // Maybe hookfs doesn't need forget
    }

    #[tracing::instrument]
    async fn getattr(&self, ino: u64, reply: ReplyAttr) {
        trace!("getattr");

        let time = get_time();

        let inode_map = self.inode_map.read().await;
        let path = match inode_map.get(&ino) {
            Some(path) => path.as_path(),
            None => {
                error!("cannot find inode({}) in inode_map", ino);
                reply.error(libc::EFAULT);
                return;
            }
        };

        let stat = match stat::lstat(path) {
            Ok(stat) => stat,
            Err(err) => {
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                trace!("return with errno: {}", errno);
                reply.error(errno);
                return;
            }
        };

        let stat = match convert_libc_stat_to_fuse_stat(stat) {
            Some(stat) => stat,
            None => {
                error!(
                    "return with unknown file type {}",
                    stat.st_mode & libc::S_IFMT
                );
                reply.error(libc::EINVAL);
                return;
            }
        };

        trace!("return with {:?}", stat);

        reply.attr(&time, &stat);
    }

    #[tracing::instrument]
    async fn setattr(
        &self,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<Timespec>,
        mtime: Option<Timespec>,
        fh: Option<u64>,
        crtime: Option<Timespec>,
        chgtime: Option<Timespec>,
        bkuptime: Option<Timespec>,
        flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        trace!("setattr");

        let inode_map = self.inode_map.read().await;
        let path = match inode_map.get(&ino) {
            Some(path) => path.as_path(),
            None => {
                error!("cannot find inode({}) in inode_map", ino);
                reply.error(libc::EFAULT);
                return;
            }
        };

        if let Err(err) = chown(
            path,
            uid.map(|uid| Uid::from_raw(uid)),
            gid.map(|gid| Gid::from_raw(gid)),
        ) {
            trace!("return with error: {}", err);
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);
            return;
        }
        if let Some(mode) = mode {
            if let Err(err) = stat::fchmodat(
                None,
                path,
                stat::Mode::from_bits_truncate(mode),
                stat::FchmodatFlags::FollowSymlink,
            ) {
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

        self.getattr(ino, reply).await
    }

    #[tracing::instrument]
    async fn readlink(&self, ino: u64, reply: ReplyData) {
        trace!("readlink");
        let inode_map = self.inode_map.read().await;
        let path = match inode_map.get(&ino) {
            Some(path) => path.as_path(),
            None => {
                error!("cannot find inode({}) in inode_map", ino);
                reply.error(libc::EFAULT);
                return;
            }
        };

        let path = match readlink(path) {
            Ok(path) => path,
            Err(err) => {
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);
                return;
            }
        };

        let path = match CString::new(path.as_os_str().as_bytes()) {
            Ok(path) => path,
            Err(_) => {
                debug!("converting path to CString failed");
                reply.error(libc::EINVAL);
                return;
            }
        };

        let data = path.as_bytes_with_nul();
        trace!("reply with data: {:?}", data);
        reply.data(data);
    }

    #[tracing::instrument]
    async fn mknod(&self, parent: u64, name: OsString, mode: u32, rdev: u32) -> Result<Entry> {
        trace!("mknod");

        let inode_map = self.inode_map.read().await;
        let parent_path = inode_map
            .get(&parent)
            .ok_or(Error::InodeNotFound { inode: parent })?;
        let path = parent_path.join(&name);
        let path = CString::new(path.as_os_str().as_bytes())?;

        trace!("mknod for {:?}", path);

        let ret = unsafe {
            let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *mut i8;

            libc::mknod(path_ptr, mode, rdev as u64)
        };
        if ret == -1 {
            return Err(Error::from(nix::Error::last()));
        }
        self.lookup(parent, name).await
    }

    #[tracing::instrument]
    async fn mkdir(&self, parent: u64, name: OsString, mode: u32) -> Result<Entry> {
        trace!("mkdir");
        let inode_map = self.inode_map.read().await;
        let parent_path = inode_map
            .get(&parent)
            .ok_or(Error::InodeNotFound { inode: parent })?;
        let path = parent_path.join(&name);

        let mode = stat::Mode::from_bits_truncate(mode);
        mkdir(&path, mode)?;
        self.lookup(parent, name).await
    }
    #[tracing::instrument]
    async fn unlink(&self, parent: u64, name: OsString) -> Result<()> {
        trace!("unlink");
        let inode_map = self.inode_map.read().await;
        let parent_path = inode_map
            .get(&parent)
            .ok_or(Error::InodeNotFound { inode: parent })?;
        let path = parent_path.join(name);
        unlink(&path)?;
        Ok(())
    }
    #[tracing::instrument]
    async fn rmdir(&self, parent: u64, name: OsString) -> Result<()> {
        trace!("rmdir");
        let inode_map = self.inode_map.read().await;
        let parent_path = inode_map
            .get(&parent)
            .ok_or(Error::InodeNotFound { inode: parent })?;
        let path = parent_path.join(name);

        let path = CString::new(path.as_os_str().as_bytes())?;
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let ret = unsafe { libc::rmdir(path_ptr) };

        if ret == -1 {
            Err(Error::from(nix::Error::last()))
        } else {
            Ok(())
        }
    }
    #[tracing::instrument]
    async fn symlink(&self, parent: u64, name: OsString, link: PathBuf) -> Result<Entry> {
        trace!("symlink");
        let inode_map = self.inode_map.read().await;
        let parent_path = inode_map
            .get(&parent)
            .ok_or(Error::InodeNotFound { inode: parent })?;
        let path = parent_path.join(&name);

        trace!("create symlink: {} => {}", path.display(), link.display());
        symlinkat(link.as_path(), None, &path)?;

        self.lookup(parent, name).await
    }
    #[tracing::instrument]
    async fn rename(
        &self,
        parent: u64,
        name: OsString,
        newparent: u64,
        newname: OsString,
        reply: ReplyEmpty,
    ) {
        trace!("rename");
        error!("unimplimented");
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    async fn link(&self, ino: u64, newparent: u64, newname: OsString) -> Result<Entry> {
        trace!("link");
        let inode_map = self.inode_map.read().await;
        let original_path = inode_map
            .get(&ino)
            .ok_or(Error::InodeNotFound { inode: ino })?;

        let new_parent_path = inode_map
            .get(&newparent)
            .ok_or(Error::InodeNotFound { inode: newparent })?;
        let new_path = new_parent_path.join(&newname);

        linkat(
            None,
            original_path,
            None,
            &new_path,
            LinkatFlags::NoSymlinkFollow,
        )?;
        self.lookup(newparent, newname).await
    }
    #[tracing::instrument]
    async fn open(&self, ino: u64, flags: u32) -> Result<Open> {
        trace!("open");
        // filter out append. The kernel layer will translate the
        // offsets for us appropriately.
        let filtered_flags = flags & (!(libc::O_APPEND as u32));
        let filtered_flags = OFlag::from_bits_truncate(filtered_flags as i32);

        let inode_map = self.inode_map.read().await;
        let path = inode_map
            .get(&ino)
            .ok_or(Error::InodeNotFound { inode: ino })?;

        trace!("open with flags: {:?}", filtered_flags);
        let fd = open(path, filtered_flags, stat::Mode::S_IRWXU)?;

        let fh = self.opened_files.write().await.insert(fd) as u64;

        trace!("return with fh: {}, flags: {}", fh, 0);

        // TODO: force DIRECT_IO is not a great option
        Ok(Open::new( fh, 0 ))
    }
    #[tracing::instrument]
    async fn read(&self, ino: u64, fh: u64, offset: i64, size: u32, reply: ReplyData) {
        trace!("read");
        let opened_files = self.opened_files.read().await;
        let fd: RawFd = match opened_files.get(fh as usize) {
            Some(fd) => *fd,
            None => {
                trace!("cannot find fh {} in opened_files", fh);
                reply.error(libc::EFAULT);
                return;
            }
        };
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
    async fn write(
        &self,
        ino: u64,
        fh: u64,
        offset: i64,
        data: Vec<u8>,
        flags: u32,
        reply: ReplyWrite,
    ) {
        trace!("write");
        let opened_files = self.opened_files.read().await;
        let fd: RawFd = match opened_files.get(fh as usize) {
            Some(fd) => *fd,
            None => {
                trace!("cannot find fh {} in opened_files", fh);
                reply.error(libc::EFAULT);
                return;
            }
        };

        if let Err(err) = lseek(fd, offset, Whence::SeekSet) {
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);
            return;
        }

        match write(fd, &data) {
            Ok(size) => reply.written(size as u32),
            Err(err) => {
                trace!("return with err: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);
                return;
            }
        }
    }
    #[tracing::instrument]
    async fn flush(&self, ino: u64, fh: u64, lock_owner: u64) -> Result<()> {
        trace!("flush");
        // flush is implemented with fsync. Is it the correct way?
        let opened_files = self.opened_files.read().await;
        let fd: RawFd = *opened_files
            .get(fh as usize)
            .ok_or(Error::FhNotFound { fh })?;
        fsync(fd)?;
        Ok(())
    }
    #[tracing::instrument]
    async fn release(
        &self,
        ino: u64,
        fh: u64,
        flags: u32,
        lock_owner: u64,
        flush: bool,
        reply: ReplyEmpty,
    ) {
        trace!("release");
        let mut opened_files = self.opened_files.write().await;
        opened_files.delete(fh as usize);
        reply.ok();
    }
    #[tracing::instrument]
    async fn fsync(&self, ino: u64, fh: u64, datasync: bool) -> Result<()> {
        trace!("fsync");
        let opened_files = self.opened_files.read().await;
        let fd: RawFd = *opened_files
            .get(fh as usize)
            .ok_or(Error::FhNotFound { fh })?;

        fsync(fd)?;

        Ok(())
    }
    #[tracing::instrument]
    async fn opendir(&self, ino: u64, flags: u32) -> Result<Open> {
        trace!("opendir");
        let inode_map = self.inode_map.read().await;
        let path = inode_map
            .get(&ino)
            .ok_or(Error::InodeNotFound { inode: ino })?;

        let filtered_flags = flags & (!(libc::O_APPEND as u32));
        let filtered_flags = OFlag::from_bits_truncate(filtered_flags as i32);

        let dir = dir::Dir::open(path, filtered_flags, stat::Mode::S_IRWXU)?;
        let fh = self.opened_dirs.write().await.insert(Dir::from(dir)) as u64;

        trace!("return with fh: {}, flags: {}", fh, flags);

        Ok(Open::new( fh, flags ))
    }

    #[tracing::instrument]
    async fn readdir(&self, ino: u64, fh: u64, offset: i64, mut reply: ReplyDirectory) {
        trace!("readdir");
        let offset = offset as usize;

        let mut opened_dirs = self.opened_dirs.write().await;
        let dir = match opened_dirs.get_mut(fh as usize) {
            Some(dir) => dir,
            None => {
                trace!("cannot find fh {} in opened_dirs", fh);
                reply.error(libc::EFAULT);
                return;
            }
        };

        // TODO: optimize the implementation
        let all_entries: Vec<_> = dir.iter().collect();
        if offset >= all_entries.len() {
            trace!("empty reply");
            reply.ok();
            return;
        }
        for (index, entry) in all_entries.iter().enumerate().skip(offset as usize) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    trace!("return with error: {}", err);
                    let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                    reply.error(errno);
                    return;
                }
            };

            let name = entry.file_name();
            let name = OsStr::from_bytes(name.to_bytes());

            let file_type = match entry.file_type() {
                Some(file_type) => convert_filetype(file_type),
                None => {
                    debug!("unknown file type {:?}", entry.file_type());
                    reply.error(libc::EINVAL);
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

        trace!("iterated all files");
        reply.ok();
        return;
    }
    #[tracing::instrument]
    async fn releasedir(&self, ino: u64, fh: u64, flags: u32) -> Result<()> {
        trace!("releasedir");
        self.opened_dirs.write().await.delete(fh as usize);
        Ok(())
    }
    #[tracing::instrument]
    async fn fsyncdir(&self, ino: u64, fh: u64, datasync: bool) -> Result<()> {
        debug!("unimplemented");
        Err(Error::Sys(Errno::ENOSYS))
    }
    #[tracing::instrument]
    async fn statfs(&self, ino: u64, reply: ReplyStatfs) {
        trace!("statfs");

        let stat = match statfs::statfs(self.original_path.as_path()) {
            Ok(stat) => stat,
            Err(err) => {
                trace!("return with error: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);
                return;
            }
        };

        reply.statfs(
            stat.blocks(),
            stat.blocks_free(),
            stat.blocks_available(),
            stat.files(),
            stat.files_free(),
            stat.block_size() as u32,
            stat.maximum_name_length() as u32,
            stat.block_size() as u32,
        );
    }
    #[tracing::instrument]
    async fn setxattr(
        &self,
        ino: u64,
        name: OsString,
        value: Vec<u8>,
        flags: u32,
        position: u32,
        reply: ReplyEmpty,
    ) {
        trace!("setxattr");

        let inode_map = self.inode_map.read().await;
        let path = match inode_map.get(&ino) {
            Some(path) => path,
            None => {
                error!("cannot find inode({}) in inode_map", ino);
                reply.error(libc::EFAULT);
                return;
            }
        };

        let path = match CString::new(path.as_os_str().as_bytes()) {
            Ok(path) => path,
            Err(_) => {
                debug!("converting path to CString failed");
                reply.error(libc::EINVAL);
                return;
            }
        };
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let name = match CString::new(name.as_bytes()) {
            Ok(name) => name,
            Err(_) => {
                debug!("converting name to CString failed");
                reply.error(libc::EINVAL);
                return;
            }
        };
        let name_ptr = &name.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let value_ptr = &value[0] as *const u8 as *const libc::c_void;

        let ret = unsafe { lsetxattr(path_ptr, name_ptr, value_ptr, value.len(), flags as i32) };

        if ret == -1 {
            let err = nix::Error::last();
            trace!("return with err: {}", err);
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);

            return;
        }
        reply.ok()
    }
    #[tracing::instrument]
    async fn getxattr(&self, ino: u64, name: OsString, size: u32, reply: ReplyXattr) {
        trace!("getxattr");
        let inode_map = self.inode_map.read().await;
        let path = match inode_map.get(&ino) {
            Some(path) => path,
            None => {
                error!("cannot find inode({}) in inode_map", ino);
                reply.error(libc::EFAULT);
                return;
            }
        };

        let path = match CString::new(path.as_os_str().as_bytes()) {
            Ok(path) => path,
            Err(_) => {
                debug!("converting path to CString failed");
                reply.error(libc::EINVAL);
                return;
            }
        };
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let name = match CString::new(name.as_bytes()) {
            Ok(name) => name,
            Err(_) => {
                debug!("converting name to CString failed");
                reply.error(libc::EINVAL);
                return;
            }
        };
        let name_ptr = &name.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let mut buf = Vec::new();
        buf.resize(size as usize, 0u8);
        let buf_ptr = buf.as_mut_slice() as *mut [u8] as *mut libc::c_void;

        let ret = unsafe { lgetxattr(path_ptr, name_ptr, buf_ptr, size as usize) };

        if ret == -1 {
            let err = nix::Error::last();
            trace!("return with err: {}", err);
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);

            return;
        }

        if size == 0 {
            trace!("return with size {}", ret);
            reply.size(ret as u32);
        } else {
            trace!("return with data {:?}", buf);
            reply.data(buf.as_slice());
        }
    }
    #[tracing::instrument]
    async fn listxattr(&self, ino: u64, size: u32, reply: ReplyXattr) {
        trace!("listxattr");
        let inode_map = self.inode_map.read().await;
        let path = match inode_map.get(&ino) {
            Some(path) => path,
            None => {
                error!("cannot find inode({}) in inode_map", ino);
                reply.error(libc::EFAULT);
                return;
            }
        };

        let path = match CString::new(path.as_os_str().as_bytes()) {
            Ok(path) => path,
            Err(_) => {
                debug!("converting path to CString failed");
                reply.error(libc::EINVAL);
                return;
            }
        };
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let mut buf = Vec::new();
        buf.resize(size as usize, 0u8);
        let buf_ptr = buf.as_mut_slice() as *mut [u8] as *mut libc::c_char;

        let ret = unsafe { llistxattr(path_ptr, buf_ptr, size as usize) };

        if ret == -1 {
            let err = nix::Error::last();
            trace!("return with err: {}", err);
            let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
            reply.error(errno);

            return;
        }

        if size == 0 {
            trace!("return with size {}", ret);
            reply.size(ret as u32);
        } else {
            trace!("return with data {:?}", buf);
            reply.data(buf.as_slice());
        }
    }
    #[tracing::instrument]
    async fn removexattr(&self, ino: u64, name: OsString) -> Result<()> {
        trace!("removexattr");
        let inode_map = self.inode_map.read().await;
        let path = inode_map
            .get(&ino)
            .ok_or(Error::InodeNotFound { inode: ino })?;

        let path = CString::new(path.as_os_str().as_bytes())?;
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let name = CString::new(name.as_bytes())?;
        let name_ptr = &name.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let ret = unsafe { lremovexattr(path_ptr, name_ptr) };

        if ret == -1 {
            return Err(Error::from(nix::Error::last()));
        }
        Ok(())
    }
    #[tracing::instrument]
    async fn access(&self, ino: u64, mask: u32) -> Result<()> {
        trace!("access");
        let inode_map = self.inode_map.read().await;
        let path = inode_map
            .get(&ino)
            .ok_or(Error::InodeNotFound { inode: ino })?;

        let mask = AccessFlags::from_bits_truncate(mask as i32);
        nix::unistd::access(path, mask)?;

        Ok(())
    }
    #[tracing::instrument]
    async fn create(&self, parent: u64, name: OsString, mode: u32, flags: u32, reply: ReplyCreate) {
        trace!("create");
        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = match inode_map.get(&parent) {
                Some(path) => path,
                None => {
                    error!("cannot find inode({}) in inode_map", parent);
                    reply.error(libc::EFAULT);
                    return;
                }
            };
            parent_path.join(name)
        };

        let filtered_flags = flags & (!(libc::O_APPEND as u32));
        let filtered_flags = OFlag::from_bits_truncate(filtered_flags as i32);

        let mode = stat::Mode::from_bits_truncate(mode);

        trace!("create with flags: {:?}, mode: {:?}", filtered_flags, mode);

        let fd = match open(&path, filtered_flags, mode) {
            Ok(fd) => fd,
            Err(err) => {
                trace!("return with error: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);
                return;
            }
        };

        let stat = match stat::lstat(&path) {
            Ok(stat) => stat,
            Err(err) => {
                trace!("return with error: {}", err);
                let errno = err.as_errno().map(|errno| errno as i32).unwrap_or(-1);
                reply.error(errno);
                return;
            }
        };

        let stat = match convert_libc_stat_to_fuse_stat(stat) {
            Some(stat) => stat,
            None => {
                error!(
                    "return with unknown file type {}",
                    stat.st_mode & libc::S_IFMT
                );
                reply.error(libc::EINVAL);
                return;
            }
        };

        self.inode_map.write().await.insert(stat.ino, path);

        let time = get_time();

        let fh = self.opened_files.write().await.insert(fd);

        // TODO: support generation number
        // this can be implemented with ioctl FS_IOC_GETVERSION
        trace!("return with stat: {:?} fh: {}", stat, fh);

        reply.created(&time, &stat, 0, fh as u64, flags);
    }
    #[tracing::instrument]
    async fn getlk(
        &self,
        ino: u64,
        _fh: u64,
        _lock_owner: u64,
        _start: u64,
        _end: u64,
        _typ: u32,
        _pid: u32,
        reply: ReplyLock,
    ) {
        trace!("getlk");
        // kernel will implement for hookfs
        reply.error(nix::libc::ENOSYS);
    }
    #[tracing::instrument]
    async fn setlk(
        &self,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        _start: u64,
        _end: u64,
        _typ: u32,
        _pid: u32,
        _sleep: bool,
    ) -> Result<()> {
        trace!("setlk");
        Err(Error::Sys(Errno::ENOSYS))
    }
    #[tracing::instrument]
    async fn bmap(&self, _ino: u64, _blocksize: u32, _idx: u64, reply: ReplyBmap) {
        error!("unimplemented");
        reply.error(nix::libc::ENOSYS);
    }
}
