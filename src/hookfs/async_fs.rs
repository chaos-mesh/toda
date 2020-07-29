use async_trait::async_trait;
use fuse::*;
use time::Timespec;

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

#[async_trait]
pub trait AsyncFileSystem {
    async fn lookup(self: Arc<Self>,  _parent: u64, _name: OsString, reply: ReplyEntry);

    async fn forget(self: Arc<Self>,  _ino: u64, _nlookup: u64);

    async fn getattr(self: Arc<Self>,  _ino: u64, reply: ReplyAttr);

    async fn setattr(self: Arc<Self>,  _ino: u64, _mode: Option<u32>, _uid: Option<u32>, _gid: Option<u32>, _size: Option<u64>, _atime: Option<Timespec>, _mtime: Option<Timespec>, _fh: Option<u64>, _crtime: Option<Timespec>, _chgtime: Option<Timespec>, _bkuptime: Option<Timespec>, _flags: Option<u32>, reply: ReplyAttr);

    async fn readlink(self: Arc<Self>,  _ino: u64, reply: ReplyData);

    async fn mknod(self: Arc<Self>,  _parent: u64, _name: OsString, _mode: u32, _rdev: u32, reply: ReplyEntry);

    async fn mkdir(self: Arc<Self>,  _parent: u64, _name: OsString, _mode: u32, reply: ReplyEntry);

    async fn unlink(self: Arc<Self>,  _parent: u64, _name: OsString, reply: ReplyEmpty);

    async fn rmdir(self: Arc<Self>,  _parent: u64, _name: OsString, reply: ReplyEmpty);

    async fn symlink(self: Arc<Self>,  _parent: u64, _name: OsString, _link: &Path, reply: ReplyEntry);

    async fn rename(self: Arc<Self>,  _parent: u64, _name: OsString, _newparent: u64, _newname: OsString, reply: ReplyEmpty);

    async fn link(self: Arc<Self>,  _ino: u64, _newparent: u64, _newname: OsString, reply: ReplyEntry);

    async fn open(self: Arc<Self>,  _ino: u64, _flags: u32, reply: ReplyOpen);

    async fn read(self: Arc<Self>,  _ino: u64, _fh: u64, _offset: i64, _size: u32, reply: ReplyData);

    async fn write(self: Arc<Self>,  _ino: u64, _fh: u64, _offset: i64, _data: Vec<u8>, _flags: u32, reply: ReplyWrite);

    async fn flush(self: Arc<Self>,  _ino: u64, _fh: u64, _lock_owner: u64, reply: ReplyEmpty);

    async fn release(self: Arc<Self>,  _ino: u64, _fh: u64, _flags: u32, _lock_owner: u64, _flush: bool, reply: ReplyEmpty);

    async fn fsync(self: Arc<Self>,  _ino: u64, _fh: u64, _datasync: bool, reply: ReplyEmpty);

    async fn opendir(self: Arc<Self>,  _ino: u64, _flags: u32, reply: ReplyOpen);

    async fn readdir(self: Arc<Self>,  _ino: u64, _fh: u64, _offset: i64, reply: ReplyDirectory);

    async fn releasedir(self: Arc<Self>,  _ino: u64, _fh: u64, _flags: u32, reply: ReplyEmpty);

    async fn fsyncdir (self: Arc<Self>,  _ino: u64, _fh: u64, _datasync: bool, reply: ReplyEmpty);

    async fn statfs(self: Arc<Self>,  _ino: u64, reply: ReplyStatfs);

    async fn setxattr(self: Arc<Self>,  _ino: u64, _name: OsString, _value: Vec<u8>, _flags: u32, _position: u32, reply: ReplyEmpty);

    async fn getxattr(self: Arc<Self>,  _ino: u64, _name: OsString, _size: u32, reply: ReplyXattr);

    async fn listxattr(self: Arc<Self>,  _ino: u64, _size: u32, reply: ReplyXattr);

    async fn removexattr(self: Arc<Self>,  _ino: u64, _name: OsString, reply: ReplyEmpty);

    async fn access(self: Arc<Self>,  _ino: u64, _mask: u32, reply: ReplyEmpty);

    async fn create(self: Arc<Self>,  _parent: u64, _name: OsString, _mode: u32, _flags: u32, reply: ReplyCreate);

    async fn getlk(self: Arc<Self>,  _ino: u64, _fh: u64, _lock_owner: u64, _start: u64, _end: u64, _typ: u32, _pid: u32, reply: ReplyLock);

    async fn setlk(self: Arc<Self>,  _ino: u64, _fh: u64, _lock_owner: u64, _start: u64, _end: u64, _typ: u32, _pid: u32, _sleep: bool, reply: ReplyEmpty);

    async fn bmap(self: Arc<Self>,  _ino: u64, _blocksize: u32, _idx: u64, reply: ReplyBmap);
}