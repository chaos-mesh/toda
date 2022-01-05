mod async_fs;
mod errors;
mod reply;
pub mod runtime;
mod utils;

use std::collections::{HashMap, LinkedList};
use std::ffi::{CString, OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub use async_fs::{AsyncFileSystem, AsyncFileSystemImpl};
use async_trait::async_trait;
use derive_more::{Deref, DerefMut, From};
pub use errors::{HookFsError as Error, Result};
use fuser::*;
use libc::{c_void, lgetxattr, llistxattr, lremovexattr, lsetxattr};
use nix::dir;
use nix::errno::Errno;
use nix::fcntl::{open, readlink, renameat, OFlag};
use nix::sys::{stat, statfs};
use nix::unistd::{
    close, fchownat, fsync, linkat, mkdir, symlinkat, truncate, unlink, AccessFlags, FchownatFlags,
    Gid, LinkatFlags, Uid,
};
pub use reply::Reply;
use reply::*;
use runtime::spawn_blocking;
use slab::Slab;
use tokio::sync::RwLock;
use tracing::{debug, error, instrument, trace};
use utils::*;

use crate::injector::{Injector, Method, MultiInjector};

// use fuse::consts::FOPEN_DIRECT_IO;

macro_rules! inject {
    ($self:ident, $method:ident, $path:expr) => {
        if $self.enable_injection.load(Ordering::SeqCst) {
            $self
                .injector
                .read()
                .await
                .inject(&Method::$method, $self.rebuild_path($path)?.as_path())
                .await?;
        }
    };
}

macro_rules! inject_with_ino {
    ($self:ident, $method:ident, $ino:ident) => {{
        let inode_map = $self.inode_map.read().await;
        if let Ok(path) = inode_map.get_path($ino) {
            let path = path.to_owned();
            trace!("getting attr from path {}", path.display());
            drop(inode_map);
            inject!($self, $method, &path);
        }
    }};
}

macro_rules! inject_with_fh {
    ($self:ident, $method:ident, $fh:ident) => {{
        let opened_files = $self.opened_files.read().await;
        if let Ok(file) = opened_files.get($fh as usize) {
            let path = file.original_path().to_owned();
            drop(opened_files);
            inject!($self, $method, &path);
        }
    }};
}

macro_rules! inject_write_data {
    ($self:ident, $fh:ident, $data:ident) => {{
        let opened_files = $self.opened_files.read().await;
        if let Ok(file) = opened_files.get($fh as usize) {
            let path = file.original_path().to_owned();
            trace!("Write data before inject {:?}", $data);
            $self
                .injector
                .read()
                .await
                .inject_write_data($self.rebuild_path(path)?.as_path(), &mut $data)?;
            trace!("Write data after inject {:?}", $data);
        }
    }};
}

macro_rules! inject_with_dir_fh {
    ($self:ident, $method:ident, $fh:ident) => {{
        let opened_dirs = $self.opened_dirs.read().await;
        if let Ok(dir) = opened_dirs.get($fh as usize) {
            let path = dir.original_path().to_owned();
            drop(opened_dirs);
            inject!($self, $method, &path);
        }
    }};
}

macro_rules! inject_with_parent_and_name {
    ($self:ident, $method:ident, $parent:ident, $name:expr) => {{
        let inode_map = $self.inode_map.read().await;
        if let Ok(parent_path) = inode_map.get_path($parent) {
            let old_path = parent_path.join($name);
            trace!("get path: {}", old_path.display());
            drop(inode_map);
            inject!($self, $method, old_path.as_path());
        }
    }};
}

macro_rules! inject_attr {
    ($self:ident, $attr:ident, $path:expr) => {
        if $self.enable_injection.load(Ordering::SeqCst) {
            $self
                .injector
                .read()
                .await
                .inject_attr(&mut $attr, $self.rebuild_path($path)?.as_path());
        }
    };
}

macro_rules! inject_reply {
    ($self:ident, $method:ident, $path:expr, $reply:ident, $reply_typ:ident) => {
        if $self.enable_injection.load(Ordering::SeqCst) {
            trace!("before inject {:?}", $reply);
            $self.injector.read().await.inject_reply(
                &Method::$method,
                $self.rebuild_path($path)?.as_path(),
                &mut Reply::$reply_typ(&mut $reply),
            )?;
            trace!("after inject {:?}", $reply);
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

    pub injector: RwLock<MultiInjector>,

    // map from inode to real path
    inode_map: RwLock<InodeMap>,
}

#[derive(Debug, Default)]
struct Node {
    pub ref_count: u64,
    // TODO: optimize paths with a combination data structure
    paths: LinkedList<PathBuf>,
}

impl Node {
    fn get_path(&self) -> Option<&Path> {
        self.paths.back().map(|item| item.as_path())
    }

    fn insert(&mut self, path: PathBuf) {
        for p in self.paths.iter() {
            if p == &path {
                return;
            }
        }

        self.paths.push_back(path);
    }

    fn remove(&mut self, path: &Path) {
        self.paths.drain_filter(|x| x == path);
    }
}

#[derive(Debug, Deref, DerefMut, From)]
struct InodeMap(HashMap<u64, Node>);

impl InodeMap {
    fn get_path(&self, inode: u64) -> Result<&Path> {
        self.0
            .get(&inode)
            .and_then(|item| item.get_path())
            .ok_or(Error::InodeNotFound { inode })
    }

    fn increase_ref(&mut self, inode: u64) {
        if let Some(node) = self.0.get_mut(&inode) {
            node.ref_count += 1;
        }
    }

    fn decrease_ref(&mut self, inode: u64, nlookup: u64) {
        if let Some(node) = self.0.get_mut(&inode) {
            if node.ref_count <= nlookup {
                self.0.remove(&inode);
            }
        }
    }

    fn insert_path<P: AsRef<Path>>(&mut self, inode: u64, path: P) {
        self.0
            .entry(inode)
            .or_default()
            .insert(path.as_ref().to_owned());
    }

    fn remove_path<P: AsRef<Path>>(&mut self, inode: u64, path: P) {
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
    pub fd: RawFd,
    original_path: PathBuf,
}

impl File {
    fn new<P: AsRef<Path>>(fd: RawFd, path: P) -> File {
        File {
            fd,
            original_path: path.as_ref().to_owned(),
        }
    }
    fn original_path(&self) -> &Path {
        &self.original_path
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
        inode_map.insert_path(1, original_path.as_ref());

        let inode_map = RwLock::new(inode_map);

        HookFs {
            mount_path: mount_path.as_ref().to_owned(),
            original_path: original_path.as_ref().to_owned(),
            opened_files: RwLock::new(FhMap::from(Slab::new())),
            opened_dirs: RwLock::new(FhMap::from(Slab::new())),
            injector: RwLock::new(injector),
            inode_map,
            enable_injection: AtomicBool::from(false),
        }
    }

    pub fn enable_injection(&self) {
        self.enable_injection.store(true, Ordering::SeqCst);
    }

    pub fn disable_injection(&self) {
        self.enable_injection.store(false, Ordering::SeqCst);

        // TODO: create a standalone runtime only for interrupt is too ugly.
        //       this RWLock is actually redundant, and the injector is rarely written.
        let mut rt  = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let injector = self.injector.read().await;
            injector.interrupt();
        });
    }

    pub fn rebuild_path<P: AsRef<Path>>(&self, path: P) -> Result<PathBuf> {
        let path_tail = path.as_ref().strip_prefix(self.original_path.as_path())?;
        let path = self.mount_path.join(path_tail);

        Ok(path)
    }
}

impl HookFs {
    async fn get_file_attr(&self, path: &Path) -> Result<FileAttr> {
        let mut attr = async_stat(path)
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

        stat::umask(stat::Mode::from_bits_truncate(0));

        Ok(())
    }

    fn destroy(&self) {
        trace!("destroy");
    }

    #[instrument(skip(self))]
    async fn lookup(&self, parent: u64, name: OsString) -> Result<Entry> {
        trace!("lookup");
        inject_with_parent_and_name!(self, LOOKUP, parent, &name);

        let mut inode_map = self.inode_map.write().await;
        let path = {
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(name)
        };
        trace!("lookup in {}", path.display());

        let stat = self.get_file_attr(&path).await?;

        trace!("insert ({}, {}) into inode_map", stat.ino, path.display());
        inode_map.insert_path(stat.ino, path.clone());
        inode_map.increase_ref(stat.ino);
        // TODO: support generation number
        // this can be implemented with ioctl FS_IOC_GETVERSION
        trace!("return with {:?}", stat);

        let mut reply = Entry::new(stat, 0);
        inject_reply!(self, LOOKUP, path.as_path(), reply, Entry);

        Ok(reply)
    }

    #[instrument(skip(self))]
    async fn forget(&self, ino: u64, nlookup: u64) {
        trace!("forget");
        self.inode_map.write().await.decrease_ref(ino, nlookup)
    }

    #[instrument(skip(self))]
    async fn getattr(&self, ino: u64) -> Result<Attr> {
        trace!("getattr");

        inject_with_ino!(self, GETATTR, ino);

        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?;
        trace!("getting attr from path {}", path.display());
        let stat = self.get_file_attr(path).await?;

        trace!("return with {:?}", stat);

        let mut reply = Attr::new(stat);
        inject_reply!(self, GETATTR, path, reply, Attr);

        Ok(reply)
    }

    #[instrument(skip(self))]
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
        inject_with_ino!(self, SETATTR, ino);

        // TODO: support setattr with fh

        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?;

        async_lchown(path, uid, gid).await?;

        if let Some(mode) = mode {
            async_fchmodat(path, mode).await?;
        }

        if let Some(size) = size {
            async_truncate(path, size as i64).await?;
        }

        let times = [convert_time(atime), convert_time(mtime)];
        let cpath = CString::new(path.as_os_str().as_bytes())?;
        async_utimensat(cpath, times).await?;

        let stat = self.get_file_attr(path).await?;
        trace!("return with {:?}", stat);
        let mut reply = Attr::new(stat);
        inject_reply!(self, GETATTR, path, reply, Attr);

        Ok(reply)
    }

    #[instrument(skip(self))]
    async fn readlink(&self, ino: u64) -> Result<Data> {
        trace!("readlink");

        inject_with_ino!(self, READLINK, ino);
        let inode_map = self.inode_map.read().await;
        let link_path = inode_map.get_path(ino)?;

        let path = async_readlink(link_path).await?;

        let path = CString::new(path.as_os_str().as_bytes())?;

        let data = path.as_bytes_with_nul();
        trace!("reply with data: {:?}", data);

        let mut reply = Data::new(path.into_bytes());
        inject_reply!(self, READLINK, &link_path, reply, Data);

        Ok(reply)
    }

    #[instrument(skip(self))]
    async fn mknod(
        &self,
        parent: u64,
        name: OsString,
        mode: u32,
        _umask: u32,
        rdev: u32,
        uid: u32,
        gid: u32,
    ) -> Result<Entry> {
        trace!("mknod");
        inject_with_parent_and_name!(self, MKNOD, parent, &name);

        let mut inode_map = self.inode_map.write().await;
        let parent_path = inode_map.get_path(parent)?;
        let path = parent_path.join(&name);
        inject!(self, MKNOD, path.as_path());
        let cpath = CString::new(path.as_os_str().as_bytes())?;

        trace!("mknod for {:?}", cpath);

        async_mknod(cpath, mode, rdev as u64).await?;
        async_lchown(&path, Some(uid), Some(gid)).await?;

        let stat = self.get_file_attr(&path).await?;
        inode_map.insert_path(stat.ino, path.clone());
        inode_map.increase_ref(stat.ino);
        let mut reply = Entry::new(stat, 0);
        inject_reply!(self, LOOKUP, path.as_path(), reply, Entry);

        Ok(reply)
    }

    #[instrument(skip(self))]
    async fn mkdir(
        &self,
        parent: u64,
        name: OsString,
        mode: u32,
        _umask: u32,
        uid: u32,
        gid: u32,
    ) -> Result<Entry> {
        trace!("mkdir");
        inject_with_parent_and_name!(self, MKDIR, parent, &name);

        let mut inode_map = self.inode_map.write().await;
        let path = {
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(&name)
        };

        let mode = stat::Mode::from_bits_truncate(mode);
        trace!("create directory with mode: {:?}", mode);
        async_mkdir(&path, mode).await?;
        trace!("setting owner {}:{}", uid, gid);
        async_lchown(&path, Some(uid), Some(gid)).await?;

        let stat = self.get_file_attr(&path).await?;
        inode_map.insert_path(stat.ino, path.clone());
        inode_map.increase_ref(stat.ino);
        let mut reply = Entry::new(stat, 0);
        inject_reply!(self, LOOKUP, path.as_path(), reply, Entry);

        Ok(reply)
    }

    #[instrument(skip(self))]
    async fn unlink(&self, parent: u64, name: OsString) -> Result<()> {
        trace!("unlink");
        inject_with_parent_and_name!(self, UNLINK, parent, &name);

        let mut inode_map = self.inode_map.write().await;
        let path = {
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(name)
        };

        let stat = self.get_file_attr(&path).await?;

        trace!("unlinking {}", path.display());
        async_unlink(&path).await?;

        trace!("remove {:x} from inode_map", &stat.ino);
        inode_map.remove_path(stat.ino, &path);

        Ok(())
    }

    #[instrument(skip(self))]
    async fn rmdir(&self, parent: u64, name: OsString) -> Result<()> {
        trace!("rmdir");
        inject_with_parent_and_name!(self, RMDIR, parent, &name);

        let mut inode_map = self.inode_map.write().await;
        let path = {
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(name)
        };

        let stat = self.get_file_attr(&path).await?;

        let cpath = CString::new(path.as_os_str().as_bytes())?;
        async_rmdir(cpath).await?;

        trace!("remove {:x} from inode_map", &stat.ino);
        inode_map.remove_path(stat.ino, &path);

        Ok(())
    }

    #[instrument(skip(self))]
    async fn symlink(
        &self,
        parent: u64,
        name: OsString,
        link: PathBuf,
        uid: u32,
        gid: u32,
    ) -> Result<Entry> {
        trace!("symlink");
        inject_with_parent_and_name!(self, SYMLINK, parent, &name);

        let mut inode_map = self.inode_map.write().await;
        let path = {
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(&name)
        };

        trace!("create symlink: {} => {}", path.display(), link.display());

        let path_clone = path.clone();
        spawn_blocking(move || symlinkat(&link, None, &path_clone)).await??;

        trace!("setting owner {}:{}", uid, gid);
        async_lchown(&path, Some(uid), Some(gid)).await?;

        let stat = self.get_file_attr(&path).await?;
        inode_map.insert_path(stat.ino, path.clone());
        inode_map.increase_ref(stat.ino);
        let mut reply = Entry::new(stat, 0);
        inject_reply!(self, LOOKUP, path.as_path(), reply, Entry);

        Ok(reply)
    }

    #[instrument(skip(self))]
    async fn rename(
        &self,
        parent: u64,
        name: OsString,
        newparent: u64,
        newname: OsString,
        _flags: u32,
    ) -> Result<()> {
        trace!("rename");
        inject_with_parent_and_name!(self, RENAME, parent, &name);

        let mut inode_map = self.inode_map.write().await;
        let old_path = {
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(&name)
        };
        trace!("get original path: {}", old_path.display());

        let parent_path = inode_map.get_path(parent)?;
        let old_path = parent_path.join(&name);

        let new_parent_path = inode_map.get_path(newparent)?;
        let new_path = new_parent_path.join(&newname);

        trace!("get new path: {}", new_path.display());
        trace!(
            "rename from {} to {}",
            old_path.display(),
            new_path.display()
        );

        let new_path_clone = new_path.clone();
        let old_path_clone = old_path.clone();
        spawn_blocking(move || renameat(None, &old_path_clone, None, &new_path_clone)).await??;

        let stat = self.get_file_attr(&new_path).await?;
        trace!("remove ({:x}, {})", stat.ino, old_path.display());
        inode_map.remove_path(stat.ino, &old_path);
        trace!("insert ({:x}, {})", stat.ino, new_path.display());
        inode_map.insert_path(stat.ino, &new_path);

        Ok(())
    }

    #[instrument(skip(self))]
    async fn link(&self, ino: u64, newparent: u64, newname: OsString) -> Result<Entry> {
        trace!("link");
        inject_with_ino!(self, LINK, ino);

        let mut inode_map = self.inode_map.write().await;
        let original_path = inode_map.get_path(ino)?.to_owned();
        let new_parent_path = inode_map.get_path(newparent)?.to_owned();
        let new_path = new_parent_path.join(&newname);

        trace!(
            "link from {} to {}",
            new_path.display(),
            original_path.display()
        );

        let new_path_clone = new_path.clone();
        spawn_blocking(move || {
            linkat(
                None,
                &original_path,
                None,
                &new_path_clone,
                LinkatFlags::NoSymlinkFollow,
            )
        })
        .await??;

        let stat = self.get_file_attr(&new_path).await?;
        inode_map.insert_path(stat.ino, new_path.clone());
        inode_map.increase_ref(stat.ino);
        let mut reply = Entry::new(stat, 0);
        inject_reply!(self, LOOKUP, new_path.as_path(), reply, Entry);

        Ok(reply)
    }

    #[instrument(skip(self))]
    async fn open(&self, ino: u64, flags: i32) -> Result<Open> {
        trace!("open");
        inject_with_ino!(self, OPEN, ino);

        // TODO: support direct io
        if flags & libc::O_DIRECT != 0 {
            debug!("direct io flag is ignored directly")
        }
        // filter out append. The kernel layer will translate the
        // offsets for us appropriately.
        let filtered_flags = flags & (!libc::O_APPEND) & (!libc::O_DIRECT);
        let filtered_flags = OFlag::from_bits_truncate(filtered_flags as i32);

        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?;

        trace!("open with flags: {:?}", filtered_flags);

        let fd = async_open(path, filtered_flags, stat::Mode::S_IRWXU).await?;
        let fh = self.opened_files.write().await.insert(File::new(fd, path)) as u64;

        trace!("return with fh: {}, flags: {}", fh, 0);

        let mut reply = Open::new(fh, 0);
        inject_reply!(self, OPEN, path, reply, Open);
        // TODO: force DIRECT_IO is not a great option
        Ok(reply)
    }

    #[instrument(skip(self))]
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
        inject_with_fh!(self, READ, fh);

        let opened_files = self.opened_files.read().await;
        let file = opened_files.get(fh as usize)?;
        let buf = async_read(file.fd, size as usize, offset).await?;

        let mut reply = Data::new(buf);
        inject_reply!(self, READ, &file.original_path(), reply, Data);
        Ok(reply)
    }

    #[instrument(skip(self, data))]
    async fn write(
        &self,
        _ino: u64,
        fh: u64,
        offset: i64,
        mut data: Vec<u8>,
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
    ) -> Result<Write> {
        trace!("write");
        inject_with_fh!(self, WRITE, fh);
        inject_write_data!(self, fh, data);
        let opened_files = self.opened_files.read().await;
        let file = opened_files.get(fh as usize)?;

        let size = async_write(file.fd, data, offset).await?;
        let mut reply = Write::new(size as u32);
        inject_reply!(self, WRITE, file.original_path(), reply, Write);
        Ok(reply)
    }

    #[instrument(skip(self))]
    async fn flush(&self, _ino: u64, fh: u64, _lock_owner: u64) -> Result<()> {
        trace!("flush");
        inject_with_fh!(self, FLUSH, fh);

        // flush is implemented with fsync. Is it the correct way?
        let opened_files = self.opened_files.read().await;
        let fd: RawFd = {
            let file = opened_files.get(fh as usize)?;
            file.fd
        };
        spawn_blocking(move || fsync(fd)).await??;
        Ok(())
    }

    #[instrument(skip(self))]
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
        if let Ok(file) = opened_files.get(fh as usize) {
            async_close(file.fd).await?;
        }
        opened_files.remove(fh as usize);
        Ok(())
    }

    #[instrument(skip(self))]
    async fn fsync(&self, _ino: u64, fh: u64, _datasync: bool) -> Result<()> {
        trace!("fsync");
        inject_with_fh!(self, FSYNC, fh);

        let opened_files = self.opened_files.read().await;
        let fd: RawFd = {
            let file = opened_files.get(fh as usize)?;
            file.fd
        };

        spawn_blocking(move || fsync(fd)).await??;

        Ok(())
    }

    #[instrument(skip(self))]
    async fn opendir(&self, ino: u64, flags: i32) -> Result<Open> {
        trace!("opendir");
        inject_with_ino!(self, OPENDIR, ino);

        let inode_map = self.inode_map.read().await;
        let path = { inode_map.get_path(ino)?.to_owned() };
        let filtered_flags = flags & (!libc::O_APPEND);
        let filtered_flags = OFlag::from_bits_truncate(filtered_flags as i32);

        let path_clone = path.clone();
        let dir = spawn_blocking(move || {
            trace!("opening directory {}", path_clone.display());
            dir::Dir::open(&path_clone, filtered_flags, stat::Mode::S_IRWXU)
        })
        .await??;
        trace!("directory {} opened", path.display());
        let fh = self.opened_dirs.write().await.insert(Dir::new(dir, &path)) as u64;
        trace!("return with fh: {}, flags: {}", fh, flags);

        let mut reply = Open::new(fh, flags);
        inject_reply!(self, OPENDIR, &path, reply, Open);
        Ok(reply)
    }

    #[instrument(skip(self))]
    async fn readdir(
        &self,
        _ino: u64,
        fh: u64,
        offset: i64,
        reply: &mut ReplyDirectory,
    ) -> Result<()> {
        trace!("readdir");
        inject_with_dir_fh!(self, READDIR, fh);

        let offset = offset as usize;
        let mut opened_dirs = self.opened_dirs.write().await;
        // TODO: optimize the implementation
        let all_entries: Vec<_> = {
            let dir = opened_dirs.get_mut(fh as usize)?;

            dir.iter().collect()
        };
        if offset >= all_entries.len() {
            trace!("empty reply");
            return Ok(());
        }
        for (index, entry) in all_entries.iter().enumerate().skip(offset as usize) {
            let entry = (*entry)?;

            let name = entry.file_name();
            let name = OsStr::from_bytes(name.to_bytes());

            let file_type = convert_filetype(entry.file_type().ok_or(Error::UnknownFileType)?);

            if !reply.add(entry.ino(), (index + 1) as i64, file_type, name) {
                trace!("add file {:?}", entry);
            } else {
                trace!("buffer is full");
                break;
            }
        }

        trace!("iterated all files");
        Ok(())
    }

    #[instrument(skip(self))]
    async fn releasedir(&self, _ino: u64, fh: u64, _flags: i32) -> Result<()> {
        trace!("releasedir");

        self.opened_dirs.write().await.remove(fh as usize);
        Ok(())
    }

    #[instrument(skip(self))]
    async fn fsyncdir(&self, ino: u64, _fh: u64, _datasync: bool) -> Result<()> {
        // TODO: inject

        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?.to_owned();
        spawn_blocking(move || -> Result<_> {
            std::fs::File::open(path)?.sync_all()?;

            Ok(())
        })
        .await??;
        Ok(())
    }

    #[instrument(skip(self))]
    async fn statfs(&self, ino: u64) -> Result<StatFs> {
        trace!("statfs");
        inject_with_ino!(self, STATFS, ino);

        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?.to_owned();

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
        inject_reply!(self, STATFS, &path, reply, StatFs);

        Ok(reply)
    }

    #[instrument(skip(self))]
    async fn setxattr(
        &self,
        ino: u64,
        name: OsString,
        value: Vec<u8>,
        flags: i32,
        _position: u32,
    ) -> Result<()> {
        trace!("setxattr");
        inject_with_ino!(self, SETXATTR, ino);

        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?.to_owned();
        let path = CString::new(path.as_os_str().as_bytes())?;
        let name = CString::new(name.as_bytes())?;

        async_setxattr(path, name, value, flags).await?;

        Ok(())
    }

    #[instrument(skip(self))]
    async fn getxattr(&self, ino: u64, name: OsString, size: u32) -> Result<Xattr> {
        trace!("getxattr");
        inject_with_ino!(self, GETXATTR, ino);

        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?;
        let cpath = CString::new(path.as_os_str().as_bytes())?;
        let name = CString::new(name.as_bytes())?;

        let mut buf = Vec::new();
        buf.resize(size as usize, 0u8);

        let data = async_getxattr(cpath, name, size as usize).await?;

        let mut reply = if size == 0 {
            trace!("return with size {}", data.len());
            Xattr::size(data.len() as u32)
        } else {
            trace!("return with data {:?}", data.as_slice());
            Xattr::data(data)
        };
        inject_reply!(self, GETXATTR, path, reply, Xattr);

        Ok(reply)
    }

    #[instrument(skip(self))]
    async fn listxattr(&self, ino: u64, size: u32) -> Result<Xattr> {
        trace!("listxattr");
        inject_with_ino!(self, LISTXATTR, ino);

        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?.to_owned();
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
        inject_reply!(self, LISTXATTR, path, reply, Xattr);

        Ok(reply)
    }

    #[instrument(skip(self))]
    async fn removexattr(&self, ino: u64, name: OsString) -> Result<()> {
        trace!("removexattr");
        inject_with_ino!(self, REMOVEXATTR, ino);

        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?.to_owned();
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

    #[instrument(skip(self))]
    async fn access(&self, ino: u64, mask: i32) -> Result<()> {
        trace!("access");
        inject_with_ino!(self, ACCESS, ino);

        let inode_map = self.inode_map.read().await;
        let path = inode_map.get_path(ino)?.to_owned();
        let mask = AccessFlags::from_bits_truncate(mask as i32);
        let path_clone = path.to_path_buf();

        spawn_blocking(move || nix::unistd::access(&path_clone, mask)).await??;

        Ok(())
    }

    #[instrument(skip(self))]
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
        inject_with_parent_and_name!(self, CREATE, parent, &name);

        let mut inode_map = self.inode_map.write().await;
        let path = {
            let parent_path = inode_map.get_path(parent)?;
            parent_path.join(name)
        };

        let filtered_flags = flags & (!libc::O_APPEND);
        let filtered_flags = OFlag::from_bits_truncate(filtered_flags as i32);
        let mode = stat::Mode::from_bits_truncate(mode);

        trace!("create with flags: {:?}, mode: {:?}", filtered_flags, mode);
        let fd = async_open(&path, filtered_flags, mode).await?;
        trace!("setting owner {}:{} for file", uid, gid);
        async_lchown(&path, Some(uid), Some(gid)).await?;

        let stat = self.get_file_attr(&path).await?;
        let fh = self.opened_files.write().await.insert(File::new(fd, &path));

        // TODO: support generation number
        // this can be implemented with ioctl FS_IOC_GETVERSION
        trace!("return with stat: {:?} fh: {}", stat, fh);
        inode_map.insert_path(stat.ino, path.clone());
        inode_map.increase_ref(stat.ino);
        let mut reply = Create::new(stat, 0, fh as u64, flags);
        inject_reply!(self, CREATE, path.as_path(), reply, Create);
        Ok(reply)
    }

    #[instrument(skip(self))]
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

    #[instrument(skip(self))]
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

    #[instrument(skip(self))]
    async fn bmap(&self, _ino: u64, _blocksize: u32, _idx: u64, reply: ReplyBmap) {
        error!("unimplemented");
        reply.error(nix::libc::ENOSYS);
    }
}

async fn async_setxattr(path: CString, name: CString, data: Vec<u8>, flags: i32) -> Result<()> {
    spawn_blocking(move || {
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;
        let name_ptr = &name.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;
        let data_ptr = &data[0] as *const u8 as *const libc::c_void;
        let ret = unsafe { lsetxattr(path_ptr, name_ptr, data_ptr, data.len(), flags) };

        if ret == -1 {
            Err(Error::last())
        } else {
            Ok(())
        }
    })
    .await?
}

async fn async_getxattr(path: CString, name: CString, size: usize) -> Result<Vec<u8>> {
    spawn_blocking(move || {
        let mut buf = Vec::new();
        buf.resize(size, 0);

        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;
        let name_ptr = &name.as_bytes_with_nul()[0] as *const u8 as *const libc::c_char;
        let buf_ptr = buf.as_slice() as *const [u8] as *mut [u8] as *mut libc::c_void;

        let ret = unsafe { lgetxattr(path_ptr, name_ptr, buf_ptr, size as usize) };
        if ret == -1 {
            Err(Error::last())
        } else {
            buf.resize(ret as usize, 0);
            Ok(buf)
        }
    })
    .await?
}

async fn async_read(fd: RawFd, count: usize, offset: i64) -> Result<Vec<u8>> {
    spawn_blocking(move || unsafe {
        let mut buf = Vec::new();
        buf.resize(count, 0);
        let ret = libc::pread(fd, buf.as_ptr() as *mut c_void, count, offset);
        if ret == -1 {
            Err(Error::last())
        } else {
            buf.resize(ret as usize, 0);
            Ok(buf)
        }
    })
    .await?
}

async fn async_write(fd: RawFd, data: Vec<u8>, offset: i64) -> Result<isize> {
    spawn_blocking(move || unsafe {
        let ret = libc::pwrite(fd, data.as_ptr() as *const c_void, data.len(), offset);
        if ret == -1 {
            Err(Error::last())
        } else {
            Ok(ret)
        }
    })
    .await?
}

async fn async_stat(path: &Path) -> Result<stat::FileStat> {
    let path_clone = path.to_path_buf();
    trace!("async read stat from path {}", path_clone.display());
    Ok(spawn_blocking(move || stat::lstat(&path_clone)).await??)
}

async fn async_lchown(path: &Path, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
    let path_clone = path.to_path_buf();
    spawn_blocking(move || {
        fchownat(
            None,
            &path_clone,
            uid.map(Uid::from_raw),
            gid.map(Gid::from_raw),
            FchownatFlags::NoFollowSymlink,
        )
    })
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

async fn async_utimensat(path: CString, times: [libc::timespec; 2]) -> Result<()> {
    spawn_blocking(move || unsafe {
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *mut i8;
        let ret = libc::utimensat(
            0,
            path_ptr,
            &times as *const [libc::timespec; 2] as *const libc::timespec,
            libc::AT_SYMLINK_NOFOLLOW,
        );

        if ret != 0 {
            Err(Error::last())
        } else {
            Ok(())
        }
    })
    .await??;
    Ok(())
}

async fn async_readlink(path: &Path) -> Result<OsString> {
    let path_clone = path.to_path_buf();
    Ok(spawn_blocking(move || readlink(&path_clone)).await??)
}

async fn async_mknod(path: CString, mode: u32, rdev: u64) -> Result<()> {
    spawn_blocking(move || {
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *mut i8;
        let ret = unsafe { libc::mknod(path_ptr, mode, rdev) };

        if ret != 0 {
            Err(Error::last())
        } else {
            Ok(())
        }
    })
    .await?
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

async fn async_rmdir(path: CString) -> Result<()> {
    spawn_blocking(move || {
        let path_ptr = &path.as_bytes_with_nul()[0] as *const u8 as *mut i8;
        let ret = unsafe { libc::rmdir(path_ptr) };

        if ret != 0 {
            Err(Error::last())
        } else {
            Ok(())
        }
    })
    .await?
}

async fn async_open(path: &Path, filtered_flags: OFlag, mode: stat::Mode) -> Result<RawFd> {
    let path_clone = path.to_path_buf();
    let fd = spawn_blocking(move || open(&path_clone, filtered_flags, mode)).await??;
    Ok(fd)
}

async fn async_close(fd: RawFd) -> Result<()> {
    Ok(spawn_blocking(move || close(fd)).await??)
}
