use nix::errno::Errno;
use nix::sys::stat::{makedev, mknod, Mode, SFlag};

pub fn mkfuse_node() -> anyhow::Result<()> {
    let mode = Mode::from_bits_truncate(0o666);
    let dev = makedev(10, 229);
    match mknod("/dev/fuse", SFlag::S_IFCHR, mode, dev) {
        Ok(()) => Ok(()),
        Err(Errno::EEXIST) => Ok(()),
        Err(err) => Err(err.into()),
    }
}
