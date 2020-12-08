mod async_fs;
mod errors;
mod reply;
pub mod runtime;

use crate::injector::Injector;
use crate::injector::Method;
use crate::injector::MultiInjector;

use async_trait::async_trait;
use derive_more::{Deref, DerefMut, From};
use fuser::*;
use slab::Slab;

use libc::{lgetxattr, llistxattr, lremovexattr, lsetxattr};

use nix::dir;
use nix::errno::Errno;
use nix::fcntl::{open, readlink, renameat, OFlag};
use nix::sys::stat;
use nix::sys::statfs;
use nix::sys::time::{TimeVal, TimeValLike};
use nix::unistd::{
    chown, fchown, fsync, linkat, mkdir, symlinkat, truncate, unlink, AccessFlags, Gid,
    LinkatFlags, Uid,
};

use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use log::{debug, error, trace};

use std::collections::{HashMap, HashSet};
use std::ffi::{CString, OsStr, OsString};
use std::io::SeekFrom;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub use async_fs::{AsyncFileSystem, AsyncFileSystemImpl};
pub use errors::{HookFsError as Error, Result};
pub use reply::Reply;
use reply::*;
use runtime::spawn_blocking;

use tokio::sync::RwLock;

// use fuse::consts::FOPEN_DIRECT_IO;

macro_rules! inject {
    ($self:ident, $method:ident, $path:expr) => {
        if $self.enable_injection.load(Ordering::SeqCst) {
            $self
                .injector
                .inject(&Method::$method, $self.rebuild_path($path)?.as_path())
                .await?;
        }
    };
}

macro_rules! inject_attr {
    ($self:ident, $attr:ident, $path:expr) => {
        if $self.enable_injection.load(Ordering::SeqCst) {
            $self
                .injector
                .inject_attr(&mut $attr, $self.rebuild_path($path)?.as_path());
        }
    };
}

macro_rules! inject_reply {
    ($self:ident, $method:ident, $path:expr, $reply:ident, $reply_typ:ident) => {
        if $self.enable_injection.load(Ordering::SeqCst) {
            $self.injector.inject_reply(
                &Method::$method,
                $self.rebuild_path($path)?.as_path(),
                &mut Reply::$reply_typ(&mut $reply),
            )?;
        }
    };
}

#[derive(Debug)]
pub struct HookFs {
    mount_path: PathBuf,
    original_path: PathBuf,

    enable_injection: AtomicBool,

    opened_files: RwLock<FhMap<File>>,

    opened_dirs: RwLock<FhMap<Dir>>,

    injector: MultiInjector,

    // map from inode to real path
    inode_map: RwLock<InodeMap>,
}

#[derive(Debug, Deref, DerefMut, From)]
struct InodeMap(HashMap<u64, HashSet<PathBuf>>);

impl InodeMap {
    fn get_path(&self, inode: u64) -> Result<&Path> {
        self.0
            .get(&inode)
            .and_then(|item| item.iter().next())
            .map(|item| item.as_path())
            .ok_or(Error::InodeNotFound { inode })
    }

    fn insert_path<P: AsRef<Path>>(&mut self, inode: u64, path: P) {
        self.0
            .entry(inode)
            .or_default()
            .insert(path.as_ref().to_owned());
    }

    fn remove_path<P: AsRef<Path>>(&mut self, inode: &u64, path: P) {
        match self.0.get_mut(&inode) {
            Some(set) => {
                set.remove(path.as_ref());
            }
            None => {
                error!("cannot find inode {} in inode_map", inode);
            }
        }
    }
}

#[derive(Debug, Deref, DerefMut, From)]
struct FhMap<T>(Slab<T>);

impl<T> FhMap<T> {
    fn get(&self, key: usize) -> Result<&T> {
        self.0.get(key).ok_or(Error::FhNotFound { fh: key as u64 })
    }
    fn get_mut(&mut self, key: usize) -> Result<&mut T> {
        self.0
            .get_mut(key)
            .ok_or(Error::FhNotFound { fh: key as u64 })
    }
}

#[derive(Debug)]
pub struct Dir {
    dir: dir::Dir,
    original_path: PathBuf,
}

impl Dir {
    fn new<P: AsRef<Path>>(dir: dir::Dir, path: P) -> Dir {
        Dir {
            dir,
            original_path: path.as_ref().to_owned(),
        }
    }
    fn original_path(&self) -> &Path {
        &self.original_path
    }
}

impl std::ops::Deref for Dir {
    type Target = dir::Dir;

    fn deref(&self) -> &Self::Target {
        &self.dir
    }
}

impl std::ops::DerefMut for Dir {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.dir
    }
}

#[derive(Debug)]
pub struct File {
    file: fs::File,
    original_path: PathBuf,
}

impl File {
    fn new<P: AsRef<Path>>(file: fs::File, path: P) -> File {
        File {
            file,
            original_path: path.as_ref().to_owned(),
        }
    }
    fn original_path(&self) -> &Path {
        &self.original_path
    }
}

impl std::ops::Deref for File {
    type Target = fs::File;

    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

impl std::ops::DerefMut for File {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.file
    }
}

unsafe impl Send for Dir {}
unsafe impl Sync for Dir {}

impl HookFs {
    pub fn new<P1: AsRef<Path>, P2: AsRef<Path>>(
        mount_path: P1,
        original_path: P2,
        injector: MultiInjector,
    ) -> HookFs {
        let mut inode_map = InodeMap::from(HashMap::new());
        inode_map.insert_path(1, original_path.as_ref().to_owned());

        let inode_map = RwLock::new(inode_map);

        HookFs {
            mount_path: mount_path.as_ref().to_owned(),
            original_path: original_path.as_ref().to_owned(),
            opened_files: RwLock::new(FhMap::from(Slab::new())),
            opened_dirs: RwLock::new(FhMap::from(Slab::new())),
            injector,
            inode_map,
            enable_injection: AtomicBool::from(false),
        }
    }

    pub fn enable_injection(&self) {
        self.enable_injection.store(true, Ordering::SeqCst);
    }

    pub fn disable_injection(&self) {
        self.enable_injection.store(false, Ordering::SeqCst);
    }

    pub fn rebuild_path<P: AsRef<Path>>(&self, path: P) -> Result<PathBuf> {
        let path_tail = path.as_ref().strip_prefix(self.original_path.as_path())?;
        let path = self.mount_path.join(path_tail);

        Ok(path)
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

fn system_time(sec: i64, nsec: i64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(sec as u64)
        + std::time::Duration::from_nanos(nsec as u64)
}

// convert_libc_stat_to_fuse_stat converts file stat from libc form into fuse form.
// returns None if the file type is unknown.
fn convert_libc_stat_to_fuse_stat(stat: libc::stat) -> Result<FileAttr> {
    let kind = match stat.st_mode & libc::S_IFMT {
        libc::S_IFBLK => FileType::BlockDevice,
        libc::S_IFCHR => FileType::CharDevice,
        libc::S_IFDIR => FileType::Directory,
        libc::S_IFIFO => FileType::NamedPipe,
        libc::S_IFLNK => FileType::Symlink,
        libc::S_IFREG => FileType::RegularFile,
        libc::S_IFSOCK => FileType::Socket,
        _ => return Err(Error::UnknownFileType),
    };
    Ok(FileAttr {
        ino: stat.st_ino,
        size: stat.st_size as u64,
        blocks: stat.st_blocks as u64,
        atime: system_time(stat.st_atime, stat.st_atime_nsec),
        mtime: system_time(stat.st_mtime, stat.st_mtime_nsec),
        ctime: system_time(stat.st_ctime, stat.st_ctime_nsec),
        kind,
        perm: (stat.st_mode & 0o777) as u16,
        nlink: stat.st_nlink as u32,
        uid: stat.st_uid,
        gid: stat.st_gid,
        rdev: stat.st_rdev as u32,
        blksize: stat.st_blksize as u32,
        padding: 0,                // unknown attr
        crtime: system_time(0, 0), // It's macOS only
        flags: 0,                  // It's macOS only
    })
}

impl HookFs {
    async fn get_file_attr(&self, path: &Path) -> Result<FileAttr> {
        let mut attr = async_stat(&path)
            .await
            .map(convert_libc_stat_to_fuse_stat)??;

        trace!("before inject attr {:?}", &attr);
        inject_attr!(self, attr, path);
        trace!("after inject attr {:?}", &attr);

        Ok(attr)
    }
}

#[async_trait]
impl AsyncFileSystemImpl for HookFs {
    fn init(&self) -> Result<()> {
        trace!("init");

        Ok(())
    }

    fn destroy(&self) {
        trace!("destroy");
    }

    async fn lookup(&self, parent: u64, name: OsString) -> Result<Entry> {
        trace!("lookup");
        let start_time = std::time::Instant::now();

        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(name)
        };
        trace!("lookup in {}", path.display());

        inject!(self, LOOKUP, path.as_path());

        let stat = self.get_file_attr(&path).await?;

        trace!("insert ({}, {}) into inode_map", stat.ino, path.display());
        self.inode_map
            .write()
            .await
            .insert_path(stat.ino, path.clone());
        // TODO: support generation number
        // this can be implemented with ioctl FS_IOC_GETVERSION
        trace!("return with {:?}", stat);

        let finish_time = std::time::Instant::now();
        let mut reply = Entry::new(finish_time - start_time, stat, 0);
        trace!("before inject {:?}", reply);
        inject_reply!(self, LOOKUP, path.as_path(), reply, Entry);
        trace!("after inject {:?}", reply);

        Ok(reply)
    }

    async fn forget(&self, _ino: u64, _nlookup: u64) {
        trace!("forget not implemented yet");
        // Maybe hookfs doesn't need forget
    }

    async fn getattr(&self, ino: u64) -> Result<Attr> {
        trace!("getattr");
        let start_time = std::time::Instant::now();

        let path = {
            let inode_map = self.inode_map.read().await;
            inode_map.get_path(ino)?.to_owned()
        };
        trace!("getting attr from path {}", path.display());
        inject!(self, GETATTR, &path);

        let stat = self.get_file_attr(&path).await?;

        trace!("return with {:?}", stat);

        let finish_time = std::time::Instant::now();
        let mut reply = Attr::new(finish_time - start_time, stat);
        trace!("before inject {:?}", reply);
        inject_reply!(self, GETATTR, path, reply, Attr);
        trace!("after inject {:?}", reply);

        Ok(reply)
    }

    async fn setattr(
        &self,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<std::time::SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        _flags: Option<u32>,
    ) -> Result<Attr> {
        trace!("setattr");

        // TODO: support setattr with fh

        let path = {
            let inode_map = self.inode_map.read().await;
            inode_map.get_path(ino)?.to_owned()
        };
        inject!(self, SETATTR, &path);

        async_chown(&path, uid, gid).await?;

        if let Some(mode) = mode {
            async_fchmodat(&path, mode).await?;
        }

        if let Some(size) = size {
            async_truncate(&path, size as i64).await?;
        }

        if let (Some(TimeOrNow::SpecificTime(atime)), Some(TimeOrNow::SpecificTime(mtime))) =
            (atime, mtime)
        {
            // TODO: handle error here
            let atime = atime
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as i64;
            let mtime = mtime
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as i64;

            let nano_unit = 1e9 as i64;
            let atime =
                TimeVal::seconds(atime / nano_unit) + TimeVal::nanoseconds(atime % nano_unit);
            let mtime =
                TimeVal::seconds(mtime / nano_unit) + TimeVal::nanoseconds(mtime % nano_unit);
            // TODO: check whether one of them is Some
            async_utimes(&path, atime, mtime).await?;
        }

        self.getattr(ino).await
    }

    async fn readlink(&self, ino: u64) -> Result<Data> {
        trace!("readlink");

        let link_path = {
            let inode_map = self.inode_map.read().await;
            inode_map.get_path(ino)?.to_owned()
        };
        inject!(self, READLINK, &link_path);

        let path = async_readlink(&link_path).await?;

        let path = CString::new(path.as_os_str().as_bytes())?;

        let data = path.as_bytes_with_nul();
        trace!("reply with data: {:?}", data);

        let mut reply = Data::new(path.into_bytes());
        trace!("before inject {:?}", reply);
        inject_reply!(self, READLINK, &link_path, reply, Data);
        trace!("after inject {:?}", reply);

        Ok(reply)
    }

    async fn mknod(
        &self,
        parent: u64,
        name: OsString,
        mode: u32,
        _umask: u32,
        rdev: u32,
    ) -> Result<Entry> {
        trace!("mknod");

        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(&name)
        };
        inject!(self, MKNOD, path.as_path());
        let path = CString::new(path.as_os_str().as_bytes())?;

        trace!("mknod for {:?}", path);

        let ret = async_mknod(path, mode, rdev as u64).await?;
        if ret == -1 {
            return Err(Error::last());
        }
        self.lookup(parent, name).await
    }

    async fn mkdir(&self, parent: u64, name: OsString, _umask: u32, mode: u32) -> Result<Entry> {
        trace!("mkdir");

        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(&name)
        };
        inject!(self, MKDIR, path.as_path());

        let mode = stat::Mode::from_bits_truncate(mode);
        async_mkdir(&path, mode).await?;
        self.lookup(parent, name).await
    }

    async fn unlink(&self, parent: u64, name: OsString) -> Result<()> {
        trace!("unlink");

        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(name)
        };
        inject!(self, UNLINK, path.as_path());

        let stat = self.get_file_attr(&path).await?;
        trace!("remove {} from inode_map", &stat.ino);
        self.inode_map.write().await.remove_path(&stat.ino, &path);

        trace!("unlinking {}", path.display());
        async_unlink(&path).await?;
        Ok(())
    }

    async fn rmdir(&self, parent: u64, name: OsString) -> Result<()> {
        trace!("rmdir");

        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(name)
        };
        inject!(self, RMDIR, path.as_path());

        let path = CString::new(path.as_os_str().as_bytes())?;

        let ret = async_rmdir(path).await?;

        if ret == -1 {
            Err(Error::last())
        } else {
            Ok(())
        }
    }

    async fn symlink(&self, parent: u64, name: OsString, link: PathBuf) -> Result<Entry> {
        trace!("symlink");

        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(&name)
        };
        inject!(self, SYMLINK, path.as_path());

        trace!("create symlink: {} => {}", path.display(), link.display());

        spawn_blocking(move || symlinkat(&link, None, &path)).await??;

        self.lookup(parent, name).await
    }

    async fn rename(
        &self,
        parent: u64,
        name: OsString,
        newparent: u64,
        newname: OsString,
        _flags: u32,
    ) -> Result<()> {
        trace!("rename");

        let mut inode_map = self.inode_map.write().await;
        let parent_path = inode_map.get_path(parent)?;
        let path = parent_path.join(&name);
        trace!("get original path: {}", path.display());
        inject!(self, RENAME, path.as_path());

        let new_parent_path = inode_map.get_path(newparent)?;
        let new_path = new_parent_path.join(&newname);

        trace!("get new path: {}", new_path.display());

        trace!("rename from {} to {}", path.display(), new_path.display());

        let new_path_clone = new_path.clone();
        spawn_blocking(move || renameat(None, &path, None, &new_path_clone)).await??;

        let stat = self.get_file_attr(&new_path).await?;

        trace!("insert ({}, {})", stat.ino, new_path.display());
        inode_map.insert_path(stat.ino, new_path);

        Ok(())
    }

    async fn link(&self, ino: u64, newparent: u64, newname: OsString) -> Result<Entry> {
        trace!("link");
        {
            let (original_path, new_parent_path) = {
                let inode_map = self.inode_map.read().await;
                (
                    inode_map.get_path(ino)?.to_owned(),
                    inode_map.get_path(newparent)?.to_owned(),
                )
            };

            let new_path = new_parent_path.join(&newname);
            inject!(self, LINK, new_path.as_path());

            trace!(
                "link from {} to {}",
                new_path.display(),
                original_path.display()
            );

            spawn_blocking(move || {
                linkat(
                    None,
                    &original_path,
                    None,
                    &new_path,
                    LinkatFlags::NoSymlinkFollow,
                )
            })
            .await??;
        }
        self.lookup(newparent, newname).await
    }

    async fn open(&self, ino: u64, flags: i32) -> Result<Open> {
        trace!("open");
        // TODO: support direct io
        if flags & libc::O_DIRECT != 0 {
            debug!("direct io flag is ignored directly")
        }
        // filter out append. The kernel layer will translate the
        // offsets for us appropriately.
        let filtered_flags = flags & (!libc::O_APPEND) & (!libc::O_DIRECT);
        let filtered_flags = OFlag::from_bits_truncate(filtered_flags as i32);

        let path = {
            let inode_map = self.inode_map.read().await;
            inode_map.get_path(ino)?.to_owned()
        };
        inject!(self, OPEN, &path);

        trace!("open with flags: {:?}", filtered_flags);

        let fd = async_open(&path, filtered_flags, stat::Mode::S_IRWXU).await?;

        let std_file = unsafe { std::fs::File::from_raw_fd(fd) };
        let file = fs::File::from_std(std_file);
        let fh = self.opened_files.write().await.insert(File {
            file,
            original_path: path.clone(),
        }) as u64;

        trace!("return with fh: {}, flags: {}", fh, 0);

        let mut reply = Open::new(fh, 0);
        trace!("before inject {:?}", reply);
        inject_reply!(self, OPEN, path, reply, Open);
        trace!("after inject {:?}", reply);
        // TODO: force DIRECT_IO is not a great option
        Ok(reply)
    }

    async fn read(
        &self,
        _ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
    ) -> Result<Data> {
        trace!("read");

        let mut opened_files = self.opened_files.write().await;
        let file = opened_files.get_mut(fh as usize)?;
        inject!(self, READ, &file.original_path());

        trace!("seek to {}", offset);
        file.seek(SeekFrom::Start(offset as u64)).await?;

        let mut buf = Vec::new();
        buf.resize(size as usize, 0);

        trace!("read exact");
        match file.read_exact(&mut buf).await {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                trace!("read eof");
            }
            Err(err) => {
                error!("unknown error: {}", err);
                return Err(err.into());
            }
        }

        let mut reply = Data::new(buf);
        trace!("before inject DATA[{:?}]", reply.data.len());
        inject_reply!(self, READ, &file.original_path(), reply, Data);
        trace!("after inject DATA[{:?}]", reply.data.len());
        Ok(reply)
    }

    async fn write(
        &self,
        _ino: u64,
        fh: u64,
        offset: i64,
        data: Vec<u8>,
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
    ) -> Result<Write> {
        trace!("write");

        let mut opened_files = self.opened_files.write().await;
        let file = opened_files.get_mut(fh as usize)?;
        inject!(self, WRITE, file.original_path());

        file.seek(SeekFrom::Start(offset as u64)).await?;

        file.write_all(&data).await?;

        let mut reply = Write::new(data.len() as u32);
        trace!("before inject {:?}", reply);
        inject_reply!(self, WRITE, file.original_path(), reply, Write);
        trace!("after inject {:?}", reply);
        Ok(reply)
    }

    async fn flush(&self, _ino: u64, fh: u64, _lock_owner: u64) -> Result<()> {
        trace!("flush");

        // flush is implemented with fsync. Is it the correct way?
        let fd: RawFd = {
            let opened_files = self.opened_files.read().await;
            let file = opened_files.get(fh as usize)?;

            inject!(self, FLUSH, file.original_path());

            file.as_raw_fd()
        };
        spawn_blocking(move || fsync(fd)).await??;
        Ok(())
    }

    async fn release(
        &self,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
    ) -> Result<()> {
        trace!("release");

        let mut opened_files = self.opened_files.write().await;
        opened_files.remove(fh as usize);
        Ok(())
    }

    async fn fsync(&self, _ino: u64, fh: u64, _datasync: bool) -> Result<()> {
        trace!("fsync");

        let fd: RawFd = {
            let opened_files = self.opened_files.read().await;
            let file = opened_files.get(fh as usize)?;

            inject!(self, FLUSH, file.original_path());

            file.as_raw_fd()
        };

        spawn_blocking(move || fsync(fd)).await??;

        Ok(())
    }

    async fn opendir(&self, ino: u64, flags: i32) -> Result<Open> {
        trace!("opendir");

        let path = {
            let inode_map = self.inode_map.read().await;
            inode_map.get_path(ino)?.to_owned()
        };
        inject!(self, OPENDIR, &path);

        let filtered_flags = flags & (!libc::O_APPEND);
        let filtered_flags = OFlag::from_bits_truncate(filtered_flags as i32);

        let path_clone = path.clone();
        let dir = spawn_blocking(move || {
            dir::Dir::open(&path_clone, filtered_flags, stat::Mode::S_IRWXU)
        })
        .await??;
        let fh = self.opened_dirs.write().await.insert(Dir::new(dir, &path)) as u64;

        trace!("return with fh: {}, flags: {}", fh, flags);

        let mut reply = Open::new(fh, flags);
        trace!("before inject {:?}", reply);
        inject_reply!(self, OPENDIR, &path, reply, Open);
        trace!("after inject {:?}", reply);
        Ok(reply)
    }

    async fn readdir(&self, _ino: u64, fh: u64, offset: i64, mut reply: ReplyDirectory) {
        trace!("readdir");

        let offset = offset as usize;

        // TODO: optimize the implementation
        let (parent_path, all_entries): (PathBuf, Vec<_>) = {
            let mut opened_dirs = self.opened_dirs.write().await;
            let dir = match opened_dirs.get_mut(fh as usize) {
                Ok(dir) => dir,
                Err(err) => {
                    reply.error(err.into());
                    return;
                }
            };

            let parent_path = dir.original_path().to_owned();
            let rebuilt_path = match self.rebuild_path(&parent_path) {
                Ok(path) => path,
                Err(err) => {
                    error!("fail to rebuild path {}", err);
                    reply.error(err.into());
                    return;
                }
            };
            if let Err(err) = self
                .injector
                .inject(&Method::READDIR, rebuilt_path.as_path())
                .await
            {
                reply.error(err.into());
                return;
            }

            (parent_path, dir.iter().collect())
        };
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

            let path = parent_path.join(name);
            trace!(
                "insert ({}, {}) into inode_map",
                entry.ino(),
                path.display()
            );
            self.inode_map.write().await.insert_path(entry.ino(), path);

            if !reply.add(entry.ino(), (index + 1) as i64, file_type, name) {
                trace!("add file {:?}", entry);
            } else {
                trace!("buffer is full");
                break;
            }
        }

        trace!("iterated all files");
        reply.ok();
    }

    async fn releasedir(&self, _ino: u64, fh: u64, _flags: i32) -> Result<()> {
        trace!("releasedir");

        // FIXME: please implement releasedir
        self.opened_dirs.write().await.remove(fh as usize);
        Ok(())
    }

    async fn fsyncdir(&self, ino: u64, _fh: u64, _datasync: bool) -> Result<()> {
        // TODO: inject

        let path = {
            let inode_map = self.inode_map.read().await;
            inode_map.get_path(ino)?.to_owned()
        };
        spawn_blocking(move || -> Result<_> {
            std::fs::File::open(path)?.sync_all()?;

            Ok(())
        })
        .await??;
        Ok(())
    }

    async fn statfs(&self, ino: u64) -> Result<StatFs> {
        trace!("statfs");

        let path = {
            let inode_map = self.inode_map.read().await;
            inode_map.get_path(ino)?.to_owned()
        };
        inject!(self, STATFS, &path);

        let origin_path = self.original_path.clone();
        let stat = spawn_blocking(move || statfs::statfs(&origin_path)).await??;

        let mut reply = StatFs::new(
            stat.blocks(),
            stat.blocks_free(),
            stat.blocks_available(),
            stat.files(),
            stat.files_free(),
            stat.block_size() as u32,
            stat.maximum_name_length() as u32,
            stat.block_size() as u32,
        );
        trace!("before inject {:?}", reply);
        inject_reply!(self, STATFS, &path, reply, StatFs);
        trace!("after inject {:?}", reply);

        Ok(reply)
    }

    async fn setxattr(
        &self,
        ino: u64,
        name: OsString,
        value: Vec<u8>,
        flags: i32,
        _position: u32,
    ) -> Result<()> {
        trace!("setxattr");

        let path = {
            let inode_map = self.inode_map.read().await;
            inode_map.get_path(ino)?.to_owned()
        };
        inject!(self, SETXATTR, &path);

        let path = CString::new(path.as_os_str().as_bytes())?;

        let name = CString::new(name.as_bytes())?;

        let ret = spawn_blocking(move || {
            let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;
            let name_ptr = &name.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;
            let value_ptr = &value[0] as *const u8 as *const libc::c_void;
            unsafe { lsetxattr(path_ptr, name_ptr, value_ptr, value.len(), flags as i32) }
        })
        .await?;

        if ret == -1 {
            return Err(Error::last());
        }
        Ok(())
    }

    async fn getxattr(&self, ino: u64, name: OsString, size: u32) -> Result<Xattr> {
        trace!("getxattr");
        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?;
        inject!(self, GETXATTR, path);

        let cpath = CString::new(path.as_os_str().as_bytes())?;

        let name = CString::new(name.as_bytes())?;

        let mut buf = Vec::new();
        buf.resize(size as usize, 0u8);

        let shared_buf = std::sync::Arc::new(buf);
        let buf_clone = shared_buf.clone();

        let ret = spawn_blocking(move || {
            let path_ptr = &cpath.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;
            let name_ptr = &name.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;
            let buf_ptr = buf_clone.as_slice() as *const [u8] as *mut [u8] as *mut libc::c_void;

            unsafe { lgetxattr(path_ptr, name_ptr, buf_ptr, size as usize) }
        })
        .await?;

        if ret == -1 {
            return Err(Error::last());
        }

        let mut reply = if size == 0 {
            trace!("return with size {}", ret);
            Xattr::size(ret as u32)
        } else {
            trace!("return with data {:?}", shared_buf.as_slice());
            Xattr::data(shared_buf.as_slice().to_owned())
        };
        trace!("before inject {:?}", reply);
        inject_reply!(self, GETXATTR, path, reply, Xattr);
        trace!("after inject {:?}", reply);

        Ok(reply)
    }

    async fn listxattr(&self, ino: u64, size: u32) -> Result<Xattr> {
        trace!("listxattr");
        let path = {
            let inode_map = self.inode_map.read().await;
            inode_map.get_path(ino)?.to_owned()
        };
        inject!(self, LISTXATTR, &path);

        let cpath = CString::new(path.as_os_str().as_bytes())?;

        let mut buf = Vec::new();
        buf.resize(size as usize, 0u8);

        let shared_buf = std::sync::Arc::new(buf);
        let buf_clone = shared_buf.clone();

        let ret = spawn_blocking(move || {
            let path_ptr = &cpath.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;
            let buf_ptr = buf_clone.as_slice() as *const [u8] as *mut [u8] as *mut libc::c_char;
            unsafe { llistxattr(path_ptr, buf_ptr, size as usize) }
        })
        .await?;

        if ret == -1 {
            return Err(Error::last());
        }

        let mut reply = if size == 0 {
            Xattr::size(ret as u32)
        } else {
            Xattr::data(shared_buf.as_slice().to_owned())
        };
        trace!("before inject {:?}", reply);
        inject_reply!(self, LISTXATTR, path, reply, Xattr);
        trace!("after inject {:?}", reply);

        Ok(reply)
    }

    async fn removexattr(&self, ino: u64, name: OsString) -> Result<()> {
        trace!("removexattr");
        let path = {
            let inode_map = self.inode_map.read().await;
            inode_map.get_path(ino)?.to_owned()
        };
        inject!(self, REMOVEXATTR, &path);

        let path = CString::new(path.as_os_str().as_bytes())?;

        let name = CString::new(name.as_bytes())?;

        let ret = spawn_blocking(move || {
            let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;
            let name_ptr = &name.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;
            unsafe { lremovexattr(path_ptr, name_ptr) }
        })
        .await?;

        if ret == -1 {
            return Err(Error::last());
        }
        Ok(())
    }

    async fn access(&self, ino: u64, mask: i32) -> Result<()> {
        trace!("access");
        let path = {
            let inode_map = self.inode_map.read().await;
            inode_map.get_path(ino)?.to_owned()
        };
        inject!(self, ACCESS, &path);

        let mask = AccessFlags::from_bits_truncate(mask as i32);

        let path_clone = path.to_path_buf();

        spawn_blocking(move || nix::unistd::access(&path_clone, mask)).await??;

        Ok(())
    }

    async fn create(
        &self,
        parent: u64,
        name: OsString,
        mode: u32,
        _umask: u32,
        flags: i32,
        uid: u32,
        gid: u32,
    ) -> Result<Create> {
        trace!("create");
        let start_time = std::time::Instant::now();

        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(name)
        };
        inject!(self, CREATE, path.as_path());

        let filtered_flags = flags & (!libc::O_APPEND);
        let filtered_flags = OFlag::from_bits_truncate(filtered_flags as i32);

        let mode = stat::Mode::from_bits_truncate(mode);

        trace!("create with flags: {:?}, mode: {:?}", filtered_flags, mode);

        let fd = async_open(&path, filtered_flags, mode).await?;
        trace!("setting owner {}:{} for file", uid, gid);
        fchown(fd, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))?;

        let stat = self.get_file_attr(&path).await?;

        trace!("insert ({}, {}) into inode_map", stat.ino, path.display());
        self.inode_map
            .write()
            .await
            .insert_path(stat.ino, path.clone());

        let std_file = unsafe { std::fs::File::from_raw_fd(fd) };
        let file = fs::File::from_std(std_file);
        let fh = self
            .opened_files
            .write()
            .await
            .insert(File::new(file, &path));

        // TODO: support generation number
        // this can be implemented with ioctl FS_IOC_GETVERSION
        trace!("return with stat: {:?} fh: {}", stat, fh);

        let finish_time = std::time::Instant::now();
        let mut reply = Create::new(finish_time - start_time, stat, 0, fh as u64, flags);
        trace!("before inject {:?}", reply);
        inject_reply!(self, CREATE, path.as_path(), reply, Create);
        trace!("after inject {:?}", reply);
        Ok(reply)
    }

    async fn getlk(
        &self,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        _start: u64,
        _end: u64,
        _typ: i32,
        _pid: u32,
    ) -> Result<Lock> {
        trace!("getlk");
        // kernel will implement for hookfs
        Err(Error::Sys(Errno::ENOSYS))
    }

    async fn setlk(
        &self,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        _start: u64,
        _end: u64,
        _typ: i32,
        _pid: u32,
        _sleep: bool,
    ) -> Result<()> {
        trace!("setlk");
        Err(Error::Sys(Errno::ENOSYS))
    }

    async fn bmap(&self, _ino: u64, _blocksize: u32, _idx: u64, reply: ReplyBmap) {
        error!("unimplemented");
        reply.error(nix::libc::ENOSYS);
    }
}

async fn async_stat(path: &Path) -> Result<stat::FileStat> {
    let path_clone = path.to_path_buf();
    trace!("async read stat from path {}", path_clone.display());
    Ok(spawn_blocking(move || stat::lstat(&path_clone)).await??)
}

async fn async_chown(path: &Path, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
    let path_clone = path.to_path_buf();
    spawn_blocking(move || chown(&path_clone, uid.map(Uid::from_raw), gid.map(Gid::from_raw)))
        .await??;
    Ok(())
}

async fn async_fchmodat(path: &Path, mode: u32) -> Result<()> {
    let path_clone = path.to_path_buf();
    spawn_blocking(move || {
        stat::fchmodat(
            None,
            &path_clone,
            stat::Mode::from_bits_truncate(mode),
            stat::FchmodatFlags::FollowSymlink,
        )
    })
    .await??;
    Ok(())
}

async fn async_truncate(path: &Path, len: i64) -> Result<()> {
    let path_clone = path.to_path_buf();
    spawn_blocking(move || truncate(&path_clone, len)).await??;
    Ok(())
}

async fn async_utimes(path: &Path, atime: TimeVal, mtime: TimeVal) -> Result<()> {
    let path_clone = path.to_path_buf();
    spawn_blocking(move || stat::utimes(&path_clone, &atime, &mtime)).await??;
    Ok(())
}

async fn async_readlink(path: &Path) -> Result<OsString> {
    let path_clone = path.to_path_buf();
    Ok(spawn_blocking(move || readlink(&path_clone)).await??)
}

async fn async_mknod(path: CString, mode: u32, rdev: u64) -> Result<i32> {
    let ret = spawn_blocking(move || {
        let path_ptr = path.as_bytes_with_nul()[0] as *const u8 as *mut i8;
        unsafe { libc::mknod(path_ptr, mode, rdev) }
    })
    .await?;
    Ok(ret)
}

async fn async_mkdir(path: &Path, mode: stat::Mode) -> Result<()> {
    let path_clone = path.to_path_buf();
    spawn_blocking(move || mkdir(&path_clone, mode)).await??;
    Ok(())
}

async fn async_unlink(path: &Path) -> Result<()> {
    let path_clone = path.to_path_buf();
    spawn_blocking(move || unlink(&path_clone)).await??;
    Ok(())
}

async fn async_rmdir(path: CString) -> Result<i32> {
    let ret = spawn_blocking(move || {
        let path_ptr = path.as_bytes_with_nul()[0] as *const u8 as *mut i8;
        unsafe { libc::rmdir(path_ptr) }
    })
    .await?;
    Ok(ret)
}

async fn async_open(path: &Path, filtered_flags: OFlag, mode: stat::Mode) -> Result<RawFd> {
    let path_clone = path.to_path_buf();
    let fd = spawn_blocking(move || open(&path_clone, filtered_flags, mode)).await??;
    Ok(fd)
}
