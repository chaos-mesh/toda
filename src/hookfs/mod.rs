mod async_fs;
mod errors;
mod reply;

use async_trait::async_trait;
use fuse::*;
use time::Timespec;
use derive_more::{Deref, DerefMut, From};

use libc::{lgetxattr, llistxattr, lremovexattr, lsetxattr};

use nix::dir;
use nix::errno::Errno;
use nix::fcntl::{renameat, open, readlink, OFlag};
use nix::sys::stat;
use nix::sys::statfs;
use nix::sys::time::{TimeVal, TimeValLike};
use nix::unistd::{
    chown, fsync, linkat, lseek, mkdir, read, symlinkat, truncate, unlink, write, AccessFlags, Gid,
    LinkatFlags, Uid, Whence,
};

use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use tracing::{debug, error, trace};

use std::collections::HashMap;
use std::ffi::{CString, OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{RawFd, AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::io::SeekFrom;

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

    opened_files: Arc<RwLock<FhMap<File>>>,

    opened_dirs: Arc<RwLock<FhMap<Dir>>>,

    // map from inode to real path
    inode_map: Arc<RwLock<InodeMap>>,
}

#[derive(Debug, Deref, DerefMut, From)]
struct InodeMap(HashMap<u64, PathBuf>);

impl InodeMap {
    fn get_path(&self, inode: u64) -> Result<&Path> {
        self.0.get(&inode).map(|item| item.as_path())
                    .ok_or(Error::InodeNotFound{inode})
    }
}

#[derive(Debug, Deref, DerefMut, From)]
struct FhMap<T>(CounterMap<T>);

impl<T> FhMap<T> {
    fn get(&self, key: usize) -> Result<&T> {
        self.0.get(key).ok_or(Error::FhNotFound{fh:key as u64})
    }
    fn get_mut(&mut self, key: usize) -> Result<&mut T> {
        self.0.get_mut(key).ok_or(Error::FhNotFound{fh:key as u64})
    }
}

#[derive(Debug, Deref, DerefMut, From)]
struct Dir(dir::Dir);

#[derive(Debug, Deref, DerefMut, From)]
pub struct File(fs::File);


unsafe impl Send for Dir {}
unsafe impl Sync for Dir {}

impl HookFs {
    pub fn new<P1: AsRef<Path>, P2: AsRef<Path>>(mount_path: P1, original_path: P2) -> HookFs {
        let mut inode_map = InodeMap::from(HashMap::new());
        inode_map.insert(1, original_path.as_ref().to_owned());

        let inode_map = Arc::new(RwLock::new(inode_map));

        return HookFs {
            mount_path: Arc::new(mount_path.as_ref().to_owned()),
            original_path: Arc::new(original_path.as_ref().to_owned()),
            opened_files: Arc::new(RwLock::new(FhMap::from(CounterMap::new()))),
            opened_dirs: Arc::new(RwLock::new(FhMap::from(CounterMap::new()))),
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
    return Ok(FileAttr {
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
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(name)
        };
        trace!("lookup in {}", path.display());

        let stat = stat::lstat(&path)?;

        let stat = convert_libc_stat_to_fuse_stat(stat)?;

        trace!("insert ({}, {}) into inode_map", stat.ino, path.display());
        self.inode_map.write().await.entry(stat.ino).or_insert(path);
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
    async fn getattr(&self, ino: u64) -> Result<Attr> {
        trace!("getattr");

        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?;
        trace!("getting attr from path {}", path.display());

        let stat = stat::lstat(path)?;

        let stat = convert_libc_stat_to_fuse_stat(stat)?;

        trace!("return with {:?}", stat);

        Ok(Attr::new(stat))
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
    ) -> Result<Attr> {
        trace!("setattr");

        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?;

        chown(
            path,
            uid.map(|uid| Uid::from_raw(uid)),
            gid.map(|gid| Gid::from_raw(gid)),
        )?;
        if let Some(mode) = mode {
            stat::fchmodat(
                None,
                path,
                stat::Mode::from_bits_truncate(mode),
                stat::FchmodatFlags::FollowSymlink,
            )?;
        }

        if let Some(size) = size {
            truncate(path, size as i64)?;
        }

        if let (Some(atime), Some(mtime)) = (atime, mtime) {
            let atime = TimeVal::seconds(atime.sec) + TimeVal::nanoseconds(atime.nsec as i64);
            let mtime = TimeVal::seconds(mtime.sec) + TimeVal::nanoseconds(mtime.nsec as i64);
            // TODO: check whether one of them is Some
            stat::utimes(path, &atime, &mtime)?;
        }

        self.getattr(ino).await
    }

    #[tracing::instrument]
    async fn readlink(&self, ino: u64) -> Result<Data> {
        trace!("readlink");
        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?;

        let path = readlink(path)?;

        let path = CString::new(path.as_os_str().as_bytes())?;

        let data = path.as_bytes_with_nul();
        trace!("reply with data: {:?}", data);

        Ok(Data::new(path.into_bytes()))
    }

    #[tracing::instrument]
    async fn mknod(&self, parent: u64, name: OsString, mode: u32, rdev: u32) -> Result<Entry> {
        trace!("mknod");
        
        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = inode_map
                .get_path(parent)?;
            parent_path.join(&name)
        };
        let path = CString::new(path.as_os_str().as_bytes())?;

        trace!("mknod for {:?}", path);

        let ret = unsafe {
            let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *mut i8;

            libc::mknod(path_ptr, mode, rdev as u64)
        };
        if ret == -1 {
            return Err(Error::last());
        }
        self.lookup(parent, name).await
    }

    #[tracing::instrument]
    async fn mkdir(&self, parent: u64, name: OsString, mode: u32) -> Result<Entry> {
        trace!("mkdir");
        
        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(&name)
        };

        let mode = stat::Mode::from_bits_truncate(mode);
        mkdir(&path, mode)?;
        self.lookup(parent, name).await
    }
    #[tracing::instrument]
    async fn unlink(&self, parent: u64, name: OsString) -> Result<()> {
        trace!("unlink");
        
        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(name)
        };

        let stat = stat::lstat(&path)?;
        self.inode_map.write().await.remove(&stat.st_ino);

        trace!("unlinking {}", path.display());
        unlink(&path)?;
        Ok(())
    }
    #[tracing::instrument]
    async fn rmdir(&self, parent: u64, name: OsString) -> Result<()> {
        trace!("rmdir");
        
        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(name)
        };

        let path = CString::new(path.as_os_str().as_bytes())?;
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let ret = unsafe { libc::rmdir(path_ptr) };

        if ret == -1 {
            Err(Error::last())
        } else {
            Ok(())
        }
    }
    #[tracing::instrument]
    async fn symlink(&self, parent: u64, name: OsString, link: PathBuf) -> Result<Entry> {
        trace!("symlink");
        
        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(&name)
        };

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
    ) -> Result<()> {
        trace!("rename");
        
        let mut inode_map = self.inode_map.write().await;
        let parent_path = inode_map.get_path(parent)?;
        let path = parent_path.join(&name);

        trace!("get original path: {}", path.display());

        let new_parent_path = inode_map.get_path(newparent)?;
        let new_path = new_parent_path.join(&newname);

        trace!("get new path: {}", new_path.display());

        trace!("rename from {} to {}", path.display(), new_path.display());

        renameat(None, &path, None, &new_path)?;

        let stat = stat::lstat(&new_path)?;

        trace!("insert inode_map ({}, {})", stat.st_ino, new_path.display());
        inode_map.insert(stat.st_ino, new_path);

        Ok(())
    }
    #[tracing::instrument]
    async fn link(&self, ino: u64, newparent: u64, newname: OsString) -> Result<Entry> {
        trace!("link");
        {
            let inode_map = self.inode_map.read().await;
            let original_path = inode_map.get_path(ino)?;

            let new_parent_path = inode_map.get_path(newparent)?;
            let new_path = new_parent_path.join(&newname);

            linkat(
                None,
                original_path,
                None,
                &new_path,
                LinkatFlags::NoSymlinkFollow,
            )?;
        }
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
        let path = inode_map.get_path(ino)?;

        trace!("open with flags: {:?}", filtered_flags);
        let fd = open(path, filtered_flags, stat::Mode::S_IRWXU)?;

        let std_file = unsafe {std::fs::File::from_raw_fd(fd)};
        let file = fs::File::from_std(std_file);
        let fh = self.opened_files.write().await.insert(File::from(file)) as u64;

        trace!("return with fh: {}, flags: {}", fh, 0);

        // TODO: force DIRECT_IO is not a great option
        Ok(Open::new(fh, 0))
    }
    #[tracing::instrument]
    async fn read(&self, ino: u64, fh: u64, offset: i64, size: u32) -> Result<Data> {
        trace!("read");
        let mut opened_files = self.opened_files.write().await;
        let file = opened_files.get_mut(fh as usize)?;
        
        trace!("seek to {}", offset);
        file.seek(SeekFrom::Start(offset as u64)).await?;

        let mut buf = Vec::new();
        buf.resize(size as usize, 0);

        trace!("read exact");
        match file.read_exact(&mut buf).await {
            Ok(_) => {},
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                trace!("read eof");
            }
            Err(err) => {
                return Err(err.into())
            }
        }
        trace!("return with data: {:?}", buf);

        Ok(Data::new(buf))
    }
    #[tracing::instrument]
    async fn write(
        &self,
        ino: u64,
        fh: u64,
        offset: i64,
        data: Vec<u8>,
        flags: u32,
    ) -> Result<Write> {
        trace!("write");
        let mut opened_files = self.opened_files.write().await;
        let file = opened_files.get_mut(fh as usize)?;

        file.seek(SeekFrom::Start(offset as u64)).await?;

        file.write_all(&data).await?;

        Ok(Write::new(data.len() as u32))
    }
    #[tracing::instrument]
    async fn flush(&self, ino: u64, fh: u64, lock_owner: u64) -> Result<()> {
        trace!("flush");
        // flush is implemented with fsync. Is it the correct way?
        let opened_files = self.opened_files.read().await;
        let fd: RawFd = opened_files
            .get(fh as usize)?.as_raw_fd();
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
    ) -> Result<()> {
        trace!("release");
        // FIXME: implement release
        let mut opened_files = self.opened_files.write().await;
        opened_files.delete(fh as usize);
        Ok(())
    }
    #[tracing::instrument]
    async fn fsync(&self, ino: u64, fh: u64, datasync: bool) -> Result<()> {
        trace!("fsync");
        let opened_files = self.opened_files.read().await;
        let fd: RawFd = opened_files
            .get(fh as usize)?.as_raw_fd();

        fsync(fd)?;

        Ok(())
    }
    #[tracing::instrument]
    async fn opendir(&self, ino: u64, flags: u32) -> Result<Open> {
        trace!("opendir");
        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?;

        let filtered_flags = flags & (!(libc::O_APPEND as u32));
        let filtered_flags = OFlag::from_bits_truncate(filtered_flags as i32);

        let dir = dir::Dir::open(path, filtered_flags, stat::Mode::S_IRWXU)?;
        let fh = self.opened_dirs.write().await.insert(Dir::from(dir)) as u64;

        trace!("return with fh: {}, flags: {}", fh, flags);

        Ok(Open::new(fh, flags))
    }

    #[tracing::instrument]
    async fn readdir(&self, ino: u64, fh: u64, offset: i64, mut reply: ReplyDirectory){
        trace!("readdir");
        let parent_path = {
            let inode_map = self.inode_map.read().await;
            match inode_map.get_path(ino) {
                Ok(path) => path.to_owned(),
                Err(err) => {
                    error!("cannot find inode {} in inode_map", ino);
                    reply.error(err.into());
                    return
                }
            }
        };
        let offset = offset as usize;

        let mut opened_dirs = self.opened_dirs.write().await;
        let dir = match opened_dirs.get_mut(fh as usize) {
            Ok(dir) => dir,
            Err(err) => {
                reply.error(err.into());
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

            let path = parent_path.join(name);
            trace!("insert ({}, {}) into inode_map", entry.ino(), path.display());
            self.inode_map.write().await.entry(entry.ino()).or_insert(path);

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
        // FIXME: please implement releasedir
        // self.opened_dirs.write().await.delete(fh as usize);
        Ok(())
    }
    #[tracing::instrument]
    async fn fsyncdir(&self, ino: u64, fh: u64, datasync: bool) -> Result<()> {
        debug!("unimplemented");
        Err(Error::Sys(Errno::ENOSYS))
    }
    #[tracing::instrument]
    async fn statfs(&self, ino: u64) -> Result<StatFs> {
        trace!("statfs");

        let stat = statfs::statfs(self.original_path.as_path())?;

        Ok(StatFs::new(
            stat.blocks(),
            stat.blocks_free(),
            stat.blocks_available(),
            stat.files(),
            stat.files_free(),
            stat.block_size() as u32,
            stat.maximum_name_length() as u32,
            stat.block_size() as u32,
        ))
    }
    #[tracing::instrument]
    async fn setxattr(
        &self,
        ino: u64,
        name: OsString,
        value: Vec<u8>,
        flags: u32,
        position: u32,
    ) -> Result<()> {
        trace!("setxattr");

        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?;

        let path = CString::new(path.as_os_str().as_bytes())?;
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let name = CString::new(name.as_bytes())?;
        let name_ptr = &name.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let value_ptr = &value[0] as *const u8 as *const libc::c_void;

        let ret = unsafe { lsetxattr(path_ptr, name_ptr, value_ptr, value.len(), flags as i32) };

        if ret == -1 {
            return Err(Error::last());
        }
        Ok(())
    }
    #[tracing::instrument]
    async fn getxattr(&self, ino: u64, name: OsString, size: u32) -> Result<Xattr> {
        trace!("getxattr");
        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?;

        let path = CString::new(path.as_os_str().as_bytes())?;
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let name = CString::new(name.as_bytes())?;
        let name_ptr = &name.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let mut buf = Vec::new();
        buf.resize(size as usize, 0u8);
        let buf_ptr = buf.as_mut_slice() as *mut [u8] as *mut libc::c_void;

        let ret = unsafe { lgetxattr(path_ptr, name_ptr, buf_ptr, size as usize) };

        if ret == -1 {
            return Err(Error::last())
        }

        if size == 0 {
            trace!("return with size {}", ret);
            Ok(Xattr::size(ret as u32))
        } else {
            trace!("return with data {:?}", buf);
            Ok(Xattr::data(buf))
        }
    }
    #[tracing::instrument]
    async fn listxattr(&self, ino: u64, size: u32) -> Result<Xattr> {
        trace!("listxattr");
        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?;

        let path = CString::new(path.as_os_str().as_bytes())?;
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let mut buf = Vec::new();
        buf.resize(size as usize, 0u8);
        let buf_ptr = buf.as_mut_slice() as *mut [u8] as *mut libc::c_char;

        let ret = unsafe { llistxattr(path_ptr, buf_ptr, size as usize) };

        if ret == -1 {
            return Err(Error::last())
        }

        if size == 0 {
            Ok(Xattr::size(ret as u32))
        } else {
            Ok(Xattr::data(buf))
        }
    }
    #[tracing::instrument]
    async fn removexattr(&self, ino: u64, name: OsString) -> Result<()> {
        trace!("removexattr");
        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?;

        let path = CString::new(path.as_os_str().as_bytes())?;
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let name = CString::new(name.as_bytes())?;
        let name_ptr = &name.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;

        let ret = unsafe { lremovexattr(path_ptr, name_ptr) };

        if ret == -1 {
            return Err(Error::last());
        }
        Ok(())
    }
    #[tracing::instrument]
    async fn access(&self, ino: u64, mask: u32) -> Result<()> {
        trace!("access");
        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?;

        let mask = AccessFlags::from_bits_truncate(mask as i32);
        nix::unistd::access(path, mask)?;

        Ok(())
    }
    #[tracing::instrument]
    async fn create(&self, parent: u64, name: OsString, mode: u32, flags: u32) -> Result<Create> {
        trace!("create");
        let path = {
            let inode_map = self.inode_map.read().await;
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(name)
        };

        let filtered_flags = flags & (!(libc::O_APPEND as u32));
        let filtered_flags = OFlag::from_bits_truncate(filtered_flags as i32);

        let mode = stat::Mode::from_bits_truncate(mode);

        trace!("create with flags: {:?}, mode: {:?}", filtered_flags, mode);

        let fd = open(&path, filtered_flags, mode)?;

        let stat = stat::lstat(&path)?;

        let stat = convert_libc_stat_to_fuse_stat(stat)?;

        self.inode_map.write().await.insert(stat.ino, path);

        let std_file = unsafe {std::fs::File::from_raw_fd(fd)};
        let file = fs::File::from_std(std_file);
        let fh = self.opened_files.write().await.insert(File::from(file));

        // TODO: support generation number
        // this can be implemented with ioctl FS_IOC_GETVERSION
        trace!("return with stat: {:?} fh: {}", stat, fh);

        Ok(Create::new(stat, 0, fh as u64, flags))
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
    ) -> Result<Lock> {
        trace!("getlk");
        // kernel will implement for hookfs
        Err(Error::Sys(Errno::ENOSYS))
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
