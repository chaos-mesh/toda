use super::filter;
use super::Injector;

use super::injector_config::{AttrOverrideConfig, FileType as ConfigFileType, FilterConfig};
use crate::hookfs::Result;

use async_trait::async_trait;
use fuser::{FileAttr, FileType};
use log::{debug, trace};


use std::path::Path;

#[derive(Debug)]
pub struct AttrOverrideInjector {
    filter: filter::Filter,

    ino: Option<u64>,
    size: Option<u64>,
    blocks: Option<u64>,
    atime: Option<std::time::SystemTime>,
    mtime: Option<std::time::SystemTime>,
    ctime: Option<std::time::SystemTime>,
    kind: Option<FileType>,
    perm: Option<u16>,
    nlink: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    rdev: Option<u32>,
}

#[async_trait]
impl Injector for AttrOverrideInjector {
    async fn inject(&self, _: &filter::Method, _: &Path) -> Result<()> {
        Ok(())
    }

    fn inject_attr(&self, attr: &mut FileAttr, path: &Path) {
        // AttrOverrideInjector should always pass method filter
        if !self.filter.filter(&filter::Method::LOOKUP, path) {
            return;
        }

        if let Some(ino) = self.ino {
            trace!("overriding ino");
            attr.ino = ino
        }
        if let Some(size) = self.size {
            trace!("overriding size");
            attr.size = size
        }
        if let Some(blocks) = self.blocks {
            trace!("overriding block");
            attr.blocks = blocks
        }
        if let Some(atime) = self.atime {
            trace!("overriding atime");
            attr.atime = atime
        }
        if let Some(mtime) = self.mtime {
            trace!("overriding mtime");
            attr.mtime = mtime
        }
        if let Some(ctime) = self.ctime {
            trace!("overriding ctime");
            attr.ctime = ctime
        }
        if let Some(kind) = self.kind {
            trace!("overriding kind");
            attr.kind = kind
        }
        if let Some(perm) = self.perm {
            trace!("overriding perm");
            attr.perm = perm
        }
        if let Some(nlink) = self.nlink {
            trace!("overriding nlink");
            attr.nlink = nlink
        }
        if let Some(uid) = self.uid {
            trace!("overriding uid");
            attr.uid = uid
        }
        if let Some(gid) = self.gid {
            trace!("overriding gid");
            attr.gid = gid
        }
        if let Some(rdev) = self.rdev {
            trace!("overriding rdev");
            attr.rdev = rdev
        }
    }
}

impl AttrOverrideInjector {
    pub fn build(conf: AttrOverrideConfig) -> anyhow::Result<Self> {
        debug!("build attr override injector");

        let filter = filter::Filter::build(FilterConfig {
            path: Some(conf.path),
            methods: None,
            percent: conf.percent,
        })?;

        let atime = conf.atime;
        let mtime = conf.mtime;
        let ctime = conf.ctime;

        let kind = conf.kind.map(|item| match item {
            ConfigFileType::Directory => FileType::Directory,
            ConfigFileType::NamedPipe => FileType::NamedPipe,
            ConfigFileType::RegularFile => FileType::RegularFile,
            ConfigFileType::Socket => FileType::Socket,
            ConfigFileType::Symlink => FileType::Symlink,
            ConfigFileType::CharDevice => FileType::CharDevice,
            ConfigFileType::BlockDevice => FileType::BlockDevice,
        });

        Ok(Self {
            filter,

            ino: conf.ino,
            size: conf.size,
            blocks: conf.blocks,
            atime,
            mtime,
            ctime,
            kind,
            perm: conf.perm,
            nlink: conf.nlink,
            uid: conf.uid,
            gid: conf.gid,
            rdev: conf.rdev,
        })
    }
}
