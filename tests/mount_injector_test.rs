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
    in_mount_namespace(
        "fuse_mount_failure_restores_original_mount_and_io",
        verify_mount_failure_rollback,
    )
}

fn verify_mount_failure_rollback(root: &Path) -> Result<()> {
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

#[test]
#[ignore = "requires Linux CAP_SYS_ADMIN, unshare, and /dev/fuse in a disposable environment"]
fn recovery_removes_empty_backing_directory() -> Result<()> {
    in_mount_namespace("recovery_removes_empty_backing_directory", |root| {
        verify_recovery_cleanup(root, false)
    })
}

#[test]
#[ignore = "requires Linux CAP_SYS_ADMIN, unshare, and /dev/fuse in a disposable environment"]
fn recovery_preserves_nonempty_backing_directory() -> Result<()> {
    in_mount_namespace("recovery_preserves_nonempty_backing_directory", |root| {
        verify_recovery_cleanup(root, true)
    })
}

fn verify_recovery_cleanup(root: &Path, nonempty_backing: bool) -> Result<()> {
    let target = root.join("target");
    let backing = root.join("__chaosfs__target__");
    fs::create_dir(&target)?;
    mount::<str, _, str, str>(
        Some("tmpfs"),
        &target,
        Some("tmpfs"),
        MsFlags::empty(),
        None,
    )?;
    let original_mount_id = mount_at(&target)?.expect("tmpfs is mounted").mnt_id;
    let original_device = fs::metadata(&target)?.dev();
    let file_path = target.join("existing");
    fs::write(&file_path, b"original contents")?;
    let mut held_file = OpenOptions::new().read(true).write(true).open(&file_path)?;
    let original_file = held_file.metadata()?;
    if nonempty_backing {
        fs::create_dir(&backing)?;
        fs::write(backing.join("keep"), b"pre-existing contents")?;
    }

    let mut injection = MountInjector::create_injection(&target, vec![])?;
    let guard = injection.mount()?;
    guard.enable_injection();
    assert!(backing.is_dir());
    assert_eq!(fs::read(&file_path)?, b"original contents");
    fs::write(&file_path, b"through FUSE")?;
    let waiting = PathBuf::from(format!(
        "/sys/fs/fuse/connections/{}/waiting",
        nix::sys::stat::minor(fs::metadata(&target)?.dev())
    ));
    guard.disable_injection();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while fs::read_to_string(&waiting)?.trim() != "0" {
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "FUSE replies did not drain"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    guard.recover_mount()?;

    assert_eq!(
        mount_at(&target)?.map(|entry| entry.mnt_id),
        Some(original_mount_id)
    );
    assert_eq!(fs::metadata(&target)?.dev(), original_device);
    assert!(mount_at(&backing)?.is_none(), "backing mount remains");
    assert_eq!(fs::metadata(&file_path)?.ino(), original_file.ino());
    assert_eq!(fs::read(&file_path)?, b"through FUSE");
    fs::write(&file_path, b"restored contents")?;
    held_file.seek(SeekFrom::Start(0))?;
    let mut contents = String::new();
    held_file.read_to_string(&mut contents)?;
    assert_eq!(contents, "restored contents");
    if nonempty_backing {
        assert_eq!(fs::read(backing.join("keep"))?, b"pre-existing contents");
    } else {
        assert!(
            !backing.exists(),
            "empty backing directory remains after recovery"
        );
    }
    Ok(())
}

fn in_mount_namespace(test_name: &str, test: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    const TEST_ROOT: &str = "TODA_MOUNT_TEST_ROOT";
    const PARENT_NAMESPACE: &str = "TODA_MOUNT_PARENT_NAMESPACE";
    if let Some(root) = std::env::var_os(TEST_ROOT) {
        assert_ne!(
            fs::read_link("/proc/self/ns/mnt")?,
            PathBuf::from(std::env::var_os(PARENT_NAMESPACE).unwrap()),
            "mount changes must run in a separate mount namespace"
        );
        return test(Path::new(&root));
    }
    let root = std::env::temp_dir().join(format!(
        "toda-mount-test-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir(&root)?;
    let output = Command::new("unshare")
        .args(["--mount", "--propagation", "private"])
        .arg(std::env::current_exe()?)
        .args([
            "--ignored",
            "--exact",
            test_name,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(TEST_ROOT, &root)
        .env(PARENT_NAMESPACE, fs::read_link("/proc/self/ns/mnt")?)
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
    Ok(())
}

fn mount_at(path: &Path) -> Result<Option<MountInfo>> {
    Ok(Process::myself()?
        .mountinfo()?
        .0
        .into_iter()
        .find(|entry| entry.mount_point == path))
}
