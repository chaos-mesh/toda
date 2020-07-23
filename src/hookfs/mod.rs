use anyhow::Result;
use fuse::Filesystem;
use fuse::FileAttr;
use time::{get_time, Timespec};

use nix::sys::stat;

use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct HookFs {
    mount_path: PathBuf,
    original_path: PathBuf,
}

impl HookFs {
    pub fn new<P1: AsRef<Path>, P2: AsRef<Path>>(mount_path: P1, original_path: P2) -> HookFs {
        return HookFs {
            mount_path: mount_path.as_ref().to_owned(),
            original_path: original_path.as_ref().to_owned(),
        };
    }
}

impl Filesystem for HookFs {
    fn init(&mut self, req: &fuse::Request) -> Result<(), nix::libc::c_int> {
        println!("init: {:?}", req);
        Ok(())
    }
    fn destroy(&mut self, req: &fuse::Request) {
        println!("destroy: {:?}", req);
    }
    fn lookup(
        &mut self,
        req: &fuse::Request,
        parent: u64,
        name: &std::ffi::OsStr,
        reply: fuse::ReplyEntry,
    ) {
        let time = get_time();
        println!("lookup: {:?} {:?} {:?} {:?}", req, parent, name, reply);

        let mut sourceMount = self.original_path.clone();
        sourceMount.push(name);
        match stat::stat(&sourceMount) {
            Ok(stat) => {
                reply.entry(&time, , generation);
            }
            Err(err) => {
                reply.error(err.as_errno())
            }
        }
    }
    fn forget(&mut self, req: &fuse::Request, ino: u64, nlookup: u64) {
        println!("forget: {:?} {:?} {:?}", req, ino, nlookup);
    }
    fn getattr(&mut self, req: &fuse::Request, ino: u64, reply: fuse::ReplyAttr) {
        println!("getattr: {:?} {:?} {:?}", req, ino, reply);
        reply.error(nix::libc::ENOSYS);
    }
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
        println!("setattr: {:?}", req);
        reply.error(nix::libc::ENOSYS);
    }
    fn readlink(&mut self, req: &fuse::Request, ino: u64, reply: fuse::ReplyData) {
        println!("readlink: {:?} {:?} {:?}", req, ino, reply);
        reply.error(nix::libc::ENOSYS);
    }
    fn mknod(
        &mut self,
        req: &fuse::Request,
        _parent: u64,
        _name: &std::ffi::OsStr,
        _mode: u32,
        _rdev: u32,
        reply: fuse::ReplyEntry,
    ) {
        println!("mknod: {:?}", req);
        reply.error(nix::libc::ENOSYS);
    }
    fn mkdir(
        &mut self,
        _req: &fuse::Request,
        _parent: u64,
        _name: &std::ffi::OsStr,
        _mode: u32,
        reply: fuse::ReplyEntry,
    ) {
        reply.error(nix::libc::ENOSYS);
    }
    fn unlink(
        &mut self,
        _req: &fuse::Request,
        _parent: u64,
        _name: &std::ffi::OsStr,
        reply: fuse::ReplyEmpty,
    ) {
        reply.error(nix::libc::ENOSYS);
    }
    fn rmdir(
        &mut self,
        _req: &fuse::Request,
        _parent: u64,
        _name: &std::ffi::OsStr,
        reply: fuse::ReplyEmpty,
    ) {
        reply.error(nix::libc::ENOSYS);
    }
    fn symlink(
        &mut self,
        _req: &fuse::Request,
        _parent: u64,
        _name: &std::ffi::OsStr,
        _link: &std::path::Path,
        reply: fuse::ReplyEntry,
    ) {
        reply.error(nix::libc::ENOSYS);
    }
    fn rename(
        &mut self,
        _req: &fuse::Request,
        _parent: u64,
        _name: &std::ffi::OsStr,
        _newparent: u64,
        _newname: &std::ffi::OsStr,
        reply: fuse::ReplyEmpty,
    ) {
        reply.error(nix::libc::ENOSYS);
    }
    fn link(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        _newparent: u64,
        _newname: &std::ffi::OsStr,
        reply: fuse::ReplyEntry,
    ) {
        reply.error(nix::libc::ENOSYS);
    }
    fn open(&mut self, req: &fuse::Request, ino: u64, flags: u32, reply: fuse::ReplyOpen) {
        println!("open: {:?} {:?} {:?} {:?}", req, ino, flags, reply);
        reply.opened(0, 0);
    }
    fn read(
        &mut self,
        req: &fuse::Request,
        _ino: u64,
        _fh: u64,
        _offset: i64,
        _size: u32,
        reply: fuse::ReplyData,
    ) {
        println!("read: {:?}", req);
        reply.error(nix::libc::ENOSYS);
    }
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
        reply.error(nix::libc::ENOSYS);
    }
    fn flush(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        reply: fuse::ReplyEmpty,
    ) {
        reply.error(nix::libc::ENOSYS);
    }
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
        reply.ok();
    }
    fn fsync(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: fuse::ReplyEmpty,
    ) {
        reply.error(nix::libc::ENOSYS);
    }
    fn opendir(&mut self, req: &fuse::Request, ino: u64, flags: u32, reply: fuse::ReplyOpen) {
        println!("opendir: {:?} {:?} {:?} {:?}", req, ino, flags, reply);
        reply.opened(0, 0);
    }
    fn readdir(
        &mut self,
        req: &fuse::Request,
        ino: u64,
        fh: u64,
        offset: i64,
        reply: fuse::ReplyDirectory,
    ) {
        println!("readdir: {:?} {:?} {:?} {:?} {:?}", req, ino, fh, offset, reply);
        reply.error(nix::libc::ENOSYS);
    }
    fn releasedir(
        &mut self,
        req: &fuse::Request,
        _ino: u64,
        _fh: u64,
        _flags: u32,
        reply: fuse::ReplyEmpty,
    ) {
        println!("releasedir: {:?}", req);
        reply.ok();
    }
    fn fsyncdir(
        &mut self,
        req: &fuse::Request,
        _ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: fuse::ReplyEmpty,
    ) {
        println!("fsyncdir: {:?}", req);
        reply.error(nix::libc::ENOSYS);
    }
    fn statfs(&mut self, req: &fuse::Request, _ino: u64, reply: fuse::ReplyStatfs) {
        println!("statfs: {:?}", req);
        reply.statfs(0, 0, 0, 0, 0, 512, 255, 0);
    }
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
        reply.error(nix::libc::ENOSYS);
    }
    fn getxattr(
        &mut self,
        req: &fuse::Request,
        _ino: u64,
        _name: &std::ffi::OsStr,
        _size: u32,
        reply: fuse::ReplyXattr,
    ) {
        println!("getxattr: {:?}", req);
        reply.error(nix::libc::ENOSYS);
    }
    fn listxattr(&mut self, req: &fuse::Request, _ino: u64, _size: u32, reply: fuse::ReplyXattr) {
        println!("listxattr: {:?}", req);
        reply.error(nix::libc::ENOSYS);
    }
    fn removexattr(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        _name: &std::ffi::OsStr,
        reply: fuse::ReplyEmpty,
    ) {
        reply.error(nix::libc::ENOSYS);
    }
    fn access(&mut self, req: &fuse::Request, ino: u64, mask: u32, reply: fuse::ReplyEmpty) {
        println!("access: {:?} {:?} {:?} {:?}", req, ino, mask, reply);
        reply.error(nix::libc::ENOSYS);
    }
    fn create(
        &mut self,
        _req: &fuse::Request,
        _parent: u64,
        _name: &std::ffi::OsStr,
        _mode: u32,
        _flags: u32,
        reply: fuse::ReplyCreate,
    ) {
        reply.error(nix::libc::ENOSYS);
    }
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
        println!("getlk: {:?}", req);
        reply.error(nix::libc::ENOSYS);
    }
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
        println!("setlk: {:?}", req);
        reply.error(nix::libc::ENOSYS);
    }
    fn bmap(
        &mut self,
        _req: &fuse::Request,
        _ino: u64,
        _blocksize: u32,
        _idx: u64,
        reply: fuse::ReplyBmap,
    ) {
        reply.error(nix::libc::ENOSYS);
    }
}
