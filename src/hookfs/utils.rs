use fuser::{FileAttr, FileType, TimeOrNow};
use libc::{UTIME_NOW, UTIME_OMIT};
use nix::dir;

use super::{Error, Result};

pub fn convert_filetype(file_type: dir::Type) -> FileType {
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

pub fn system_time(sec: i64, nsec: i64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(sec as u64)
        + std::time::Duration::from_nanos(nsec as u64)
}

// convert_libc_stat_to_fuse_stat converts file stat from libc form into fuse form.
// returns None if the file type is unknown.
pub fn convert_libc_stat_to_fuse_stat(stat: libc::stat) -> Result<FileAttr> {
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
        perm: (stat.st_mode & 0o7777) as u16,
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

pub fn convert_time(t: Option<TimeOrNow>) -> libc::timespec {
    match t {
        Some(TimeOrNow::SpecificTime(t)) => {
            let nano_unit = 1e9 as i64;

            let t = t.duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as i64;
            libc::timespec {
                tv_sec: t / nano_unit,
                tv_nsec: t % nano_unit,
            }
        }
        Some(TimeOrNow::Now) => libc::timespec {
            tv_sec: 0,
            tv_nsec: UTIME_NOW,
        },
        None => libc::timespec {
            tv_sec: 0,
            tv_nsec: UTIME_OMIT,
        },
    }
}
