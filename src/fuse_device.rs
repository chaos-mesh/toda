use nix::sys::stat::{mknod, makedev, Mode, SFlag};
use nix::Error as NixError;

pub fn mkfuse_node() -> anyhow::Result<()> {
    let mode = unsafe { Mode::from_bits_unchecked(0o666) };
    let dev = makedev(10, 229);
    match mknod("/dev/fuse", SFlag::S_IFCHR, mode, dev) {
        Ok(()) => Ok(()),
        Err(NixError::Sys(errno)) => {
            if errno == nix::errno::Errno::EEXIST {
                Ok(())
            } else {
                Err(NixError::from_errno(errno).into())
            }
        }
        Err(err) => Err(err.into()),
    }
}
