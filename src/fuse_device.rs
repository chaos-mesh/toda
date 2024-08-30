use nix::sys::stat::{makedev, mknod, Mode, SFlag};

pub fn mkfuse_node() -> anyhow::Result<()> {
    let mode = Mode::S_IWUSR | Mode::S_IRUSR | Mode::S_IWGRP | Mode::S_IRGRP | Mode::S_IWOTH | Mode::S_IROTH;
    let dev = makedev(10, 229);
    match mknod("/dev/fuse", SFlag::S_IFCHR, mode, dev) {
        Ok(()) => Ok(()),
        Err(errno) => {
            if errno == nix::errno::Errno::EEXIST {
                Ok(())
            } else {
                Err(errno.into())
            }
        }
    }
}
