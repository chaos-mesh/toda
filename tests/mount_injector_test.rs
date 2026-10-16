use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use nix::mount::{mount, MsFlags};
use procfs::process::{MountInfo, Process};
use toda::mount_injector::MountInjector;

#[test]
#[ignore = "requires Linux CAP_SYS_ADMIN, unshare, and /dev/fuse in a disposable environment"]
fn fuse_mount_failure_restores_original_mount_and_io() -> Result<()> {
    const TEST_NAME: &str = "fuse_mount_failure_restores_original_mount_and_io";
    const TEST_ROOT: &str = "TODA_MOUNT_ROLLBACK_TEST_ROOT";
    const PARENT_NAMESPACE: &str = "TODA_MOUNT_ROLLBACK_PARENT_NAMESPACE";

    let root = match std::env::var_os(TEST_ROOT) {
        Some(root) => {
            assert_ne!(
                fs::read_link("/proc/self/ns/mnt")?,
                PathBuf::from(std::env::var_os(PARENT_NAMESPACE).unwrap()),
                "mount changes must run in a separate mount namespace"
            );
            PathBuf::from(root)
        }
        None => {
            let root = std::env::temp_dir().join(format!(
                "toda-mount-rollback-{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            ));
            let namespace = fs::read_link("/proc/self/ns/mnt")?;
            let executable = std::env::current_exe()?;
            fs::create_dir(&root)?;
            let output = Command::new("unshare")
                .args(["--mount", "--propagation", "private"])
                .arg(executable)
                .args([
                    "--ignored",
                    "--exact",
                    TEST_NAME,
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(TEST_ROOT, &root)
                .env(PARENT_NAMESPACE, namespace)
                .output();
            fs::remove_dir_all(&root)?;
            let output = output.context("run regression in a private mount namespace")?;
            assert!(
                output.status.success(),
                "namespace test failed with {}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return Ok(());
        }
    };

    let target = root.join("target");
    let backing = root.join("__chaosfs__target__");
    fs::create_dir(&target)?;
    mount::<str, _, str, str>(
        Some("tmpfs"),
        &target,
        Some("tmpfs"),
        MsFlags::empty(),
        None,
    )
    .context("mount the original tmpfs fixture")?;

    let original_mount_id = mount_at(&target)?.expect("tmpfs is mounted").mnt_id;
    let original_device = fs::metadata(&target)?.dev();
    let file_path = target.join("existing");
    fs::write(&file_path, b"original contents")?;
    let mut held_file = OpenOptions::new().read(true).write(true).open(&file_path)?;
    let original_file = held_file.metadata()?;
    assert_eq!(fs::read(&file_path)?, b"original contents");

    mount::<_, _, str, str>(
        Some(Path::new("/dev/null")),
        Path::new("/dev/fuse"),
        None,
        MsFlags::MS_BIND,
        None,
    )
    .context("replace /dev/fuse only in the private mount namespace")?;

    let mut injection = MountInjector::create_injection(&target, vec![])?;
    let error = match injection.mount() {
        Ok(_) => panic!("invalid FUSE device unexpectedly allowed a mount"),
        Err(error) => error,
    };
    assert!(
        error.downcast_ref::<std::io::Error>().is_some(),
        "expected a FUSE setup I/O error, not an early mount-move failure: {:#}",
        error
    );

    assert_eq!(
        mount_at(&target)?.map(|entry| entry.mnt_id),
        Some(original_mount_id),
        "the original mount must be restored after FUSE setup fails"
    );
    assert_eq!(fs::metadata(&target)?.dev(), original_device);
    assert!(mount_at(&backing)?.is_none(), "backing mount remains");

    let restored_file = fs::metadata(&file_path)?;
    assert_eq!(restored_file.dev(), original_file.dev());
    assert_eq!(restored_file.ino(), original_file.ino());
    assert_eq!(fs::read(&file_path)?, b"original contents");
    let mut contents = String::new();
    held_file.read_to_string(&mut contents)?;
    assert_eq!(contents, "original contents");

    let held_contents = b"held descriptor";
    held_file.seek(SeekFrom::Start(0))?;
    held_file.write_all(held_contents)?;
    held_file.set_len(held_contents.len() as u64)?;
    assert_eq!(fs::read(&file_path)?, held_contents);
    fs::write(&file_path, b"fresh descriptor")?;
    held_file.seek(SeekFrom::Start(0))?;
    contents.clear();
    held_file.read_to_string(&mut contents)?;
    assert_eq!(contents, "fresh descriptor");

    let new_file = target.join("new");
    fs::write(&new_file, b"new contents")?;
    assert_eq!(fs::read(&new_file)?, b"new contents");
    fs::write(&new_file, b"updated contents")?;
    assert_eq!(fs::read(&new_file)?, b"updated contents");
    assert_eq!(fs::metadata(&new_file)?.dev(), original_device);
    Ok(())
}

fn mount_at(path: &Path) -> Result<Option<MountInfo>> {
    Ok(Process::myself()?
        .mountinfo()?
        .0
        .into_iter()
        .find(|entry| entry.mount_point == path))
}
