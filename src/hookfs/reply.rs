use fuse::*;
use time::{get_time, Timespec};
use tracing::{debug, trace};

use super::errors::Result;

use std::fmt::Debug;

#[derive(Debug)]
pub struct Entry {
    pub time: Timespec,
    pub stat: FileAttr,
    pub generation: u64,
}
impl Entry {
    pub fn new(stat: FileAttr, generation: u64) -> Self {
        Self {
            time: get_time(),
            stat,
            generation,
        }
    }
}

#[derive(Debug)]
pub struct Open {
    pub fh: u64,
    pub flags: u32,
}
impl Open {
    pub fn new(fh: u64, flags: u32) -> Self {
        Self { fh, flags }
    }
}

#[derive(Debug)]
pub struct Attr {
    pub time: Timespec,
    pub attr: FileAttr,
}
impl Attr {
    pub fn new(attr: FileAttr) -> Self {
        Self {
            time: get_time(),
            attr,
        }
    }
}

#[derive(Debug)]
pub struct Data {
    pub data: Vec<u8>,
}
impl Data {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }
}

#[derive(Debug)]
pub struct StatFs {
    pub blocks: u64, 
    pub bfree: u64, 
    pub bavail: u64, 
    pub files: u64, 
    pub ffree: u64, 
    pub bsize: u32, 
    pub namelen: u32, 
    pub frsize: u32
}
impl StatFs {
    pub fn new(blocks: u64, bfree: u64, bavail: u64, files: u64, ffree: u64, bsize: u32, namelen: u32, frsize: u32) -> Self {
        Self {
            blocks, bfree, bavail, files, ffree, bsize, namelen, frsize
        }
    }
}

pub trait FsReply<T: Debug>: Sized {
    fn reply_ok(self, item: T);
    fn reply_err(self, err: libc::c_int);

    #[tracing::instrument(skip(self))]
    fn reply(self, result: Result<T>) {
        match result {
            Ok(item) => {
                trace!("ok. reply with: {:?}", item);
                self.reply_ok(item)
            }
            Err(err) => {
                debug!("err. reply with {}", err);
                self.reply_err(err.into())
            }
        }
    }
}

impl FsReply<Entry> for ReplyEntry {
    fn reply_ok(self, item: Entry) {
        self.entry(&item.time, &item.stat, item.generation);
    }
    fn reply_err(self, err: libc::c_int) {
        self.error(err);
    }
}

impl FsReply<Open> for ReplyOpen {
    fn reply_ok(self, item: Open) {
        self.opened(item.fh, item.flags);
    }
    fn reply_err(self, err: libc::c_int) {
        self.error(err);
    }
}

impl FsReply<Attr> for ReplyAttr {
    fn reply_ok(self, item: Attr) {
        self.attr(&item.time, &item.attr);
    }
    fn reply_err(self, err: libc::c_int) {
        self.error(err);
    }
}

impl FsReply<Data> for ReplyData {
    fn reply_ok(self, item: Data) {
        self.data(item.data.as_slice());
    }
    fn reply_err(self, err: libc::c_int) {
        self.error(err);
    }
}

impl FsReply<StatFs> for ReplyStatfs {
    fn reply_ok(self, item: StatFs) {
        self.statfs(item.blocks, item.bfree, item.bavail, item.files, item.ffree, item.bsize, item.namelen, item.frsize)
    }
    fn reply_err(self, err: libc::c_int) {
        self.error(err);
    }
}

impl FsReply<()> for ReplyEmpty {
    fn reply_ok(self, _: ()) {
        self.ok();
    }
    fn reply_err(self, err: libc::c_int) {
        self.error(err);
    }
}
