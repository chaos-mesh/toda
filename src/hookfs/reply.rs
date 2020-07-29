use fuse::*;
use time::{get_time, Timespec};
use tracing::{debug, trace};

use super::errors::{HookFsError, Result};

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

impl FsReply<()> for ReplyEmpty {
    fn reply_ok(self, item: ()) {
        self.ok();
    }
    fn reply_err(self, err: libc::c_int) {
        self.error(err);
    }
}
