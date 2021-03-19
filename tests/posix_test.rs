// Copyright 2020 Chaos Mesh Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// See the License for the specific language governing permissions and
// limitations under the License.

use std::ffi::OsStr;
use std::fs::{read_link, read_to_string, write, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::{Arc, Once};

use nix::sys::stat;
use nix::{fcntl, unistd};
use toda::hookfs;
use toda::injector::MultiInjector;

// These tests are port from go-fuse test

static INIT: Once = Once::new();

fn init(name: &str) -> (PathBuf, fuser::BackgroundSession) {
    let test_path_backend: PathBuf = ["/tmp/test_mnt_backend", name].iter().collect();
    let test_path: PathBuf = ["/tmp/test_mnt", name].iter().collect();

    INIT.call_once(|| {
        env_logger::init();
    });

    std::fs::remove_dir_all(&test_path_backend).ok();
    std::fs::remove_dir_all(&test_path).ok();

    std::fs::create_dir_all(&test_path_backend).ok();
    std::fs::create_dir_all(&test_path).ok();

    let hookfs = Arc::new(hookfs::HookFs::new(
        &test_path,
        &test_path_backend,
        MultiInjector::build(Vec::new()).unwrap(),
    ));

    let fs = hookfs::AsyncFileSystem::from(hookfs);

    let args = [
        "allow_other",
        "nonempty",
        "fsname=toda",
        "default_permissions",
    ];
    let flags: Vec<_> = args
        .iter()
        .flat_map(|item| vec![OsStr::new("-o"), OsStr::new(item)])
        .collect();

    let session = fuser::spawn_mount(fs, &test_path, &flags).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(1));
    (test_path, session)
}

#[test]
fn symlink_readlink() {
    let (test_path, _) = init("symlink_readlink");

    let expected_src = PathBuf::from("/foobar");
    let dst: PathBuf = test_path.join("dst");
    symlink(&expected_src, &dst).unwrap();

    let src = read_link(&dst).unwrap();

    assert_eq!(src, expected_src);
}

#[test]
fn file_basic() {
    let (test_path, _) = init("file_basic");

    let content = "hello world";
    let target_file: PathBuf = test_path.join("target_file");

    write(&target_file, content).unwrap();

    let read_output = read_to_string(&target_file).unwrap();
    assert_eq!(read_output, content);

    let file = File::open(&target_file).unwrap();

    let stat = file.metadata().unwrap();
    assert_eq!(stat.len() as usize, content.len());

    drop(file);
}

#[test]
fn truncate_file() {
    let (test_path, _) = init("truncate_file");

    let content = b"hello world";
    let target_file: PathBuf = test_path.join("target_file");

    write(&target_file, content).unwrap();

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .open(&target_file)
        .unwrap();

    let trunc = 5;
    file.set_len(trunc).unwrap();
    drop(file);

    let read_output = read_to_string(&target_file).unwrap();
    assert_eq!(read_output.as_bytes(), &content[..(trunc as usize)]);
}

#[test]
fn mkdir_rmdir() {
    let (test_path, _) = init("mkdir_rmdir");

    let dir: PathBuf = test_path.join("dir");

    std::fs::create_dir(&dir).unwrap();

    let file = File::open(&dir).unwrap();
    let is_dir = file.metadata().unwrap().is_dir();
    assert!(is_dir);

    std::fs::remove_dir(&dir).unwrap();
}

#[test]
fn nlink_zero() {
    let (test_path, _) = init("nlink_zero");

    let src: PathBuf = test_path.join("src");
    let dst: PathBuf = test_path.join("dst");

    write(&src, "source").unwrap();
    write(&dst, "dst").unwrap();

    let fd = fcntl::open(&dst, fcntl::OFlag::empty(), stat::Mode::empty()).unwrap();
    let st = stat::fstat(fd).unwrap();

    assert_eq!(st.st_nlink, 1);

    fcntl::renameat(None, &src, None, &dst).unwrap();
    let st = stat::fstat(fd).unwrap();

    assert_eq!(st.st_nlink, 0);
}

// FstatDeleted is similar to NlinkZero, but Fstat()s multiple deleted files
// in random order and checks that the results match an earlier Stat().
//
// Excercises the fd-finding logic in rawBridge.GetAttr.
#[test]
fn fstat_deleted() {
    let (test_path, _) = init("fstat_deleted");

    let i_max = 9;

    struct StatFile {
        pub file: std::os::unix::io::RawFd,
        pub stat: stat::FileStat,
    }
    let mut files = std::collections::HashMap::<usize, StatFile>::new();

    for i in 0..(i_max + 1) {
        let path: PathBuf = test_path.join(&format!("fstat_deleted_{}", i));
        let content = vec![0u8; i];
        write(&path, content).unwrap();

        let st = stat::stat(&path).unwrap();

        let fd = fcntl::open(&path, fcntl::OFlag::empty(), stat::Mode::empty()).unwrap();
        let stat_file = StatFile { file: fd, stat: st };
        files.insert(i, stat_file);

        unistd::unlink(&path).unwrap();
    }

    for (_, f) in files.iter_mut() {
        let mut stat = stat::fstat(f.file).unwrap();

        f.stat.st_nlink = 0;

        // ignore ctime, which changes on unlink
        f.stat.st_ctime_nsec = 0;
        f.stat.st_ctime = 0;
        stat.st_ctime_nsec = 0;
        stat.st_ctime = 0;

        assert_eq!(stat, f.stat);
    }
}

#[test]
fn parallel_file_open() {
    let (test_path, _) = init("parallel_file_open");

    let n = 10;

    let path: PathBuf = test_path.join("parallel_file_open_file");

    write(&path, "content").unwrap();

    let mut handlers = Vec::new();
    for i in 0..n {
        let path = path.clone();

        let handler = std::thread::spawn(move || {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();

            let mut buf = [0u8; 7];
            file.read_exact(&mut buf).unwrap();
            buf[0] = i;
            file.write_all(&buf[0..1]).unwrap();
            drop(file);
        });

        handlers.push(handler);
    }

    for handler in handlers {
        handler.join().unwrap();
    }
}

#[test]
fn link() {
    let (test_path, _) = init("parallel_file_open");
    let link = test_path.join("link");
    let target = test_path.join("target");

    write(&target, "hello").unwrap();
    let st = stat::stat(&target).unwrap();
    assert_eq!(st.st_nlink, 1);

    let before_ino = st.st_ino;
    std::fs::hard_link(&target, &link).unwrap();

    let st = stat::stat(&link).unwrap();
    assert_eq!(st.st_ino, before_ino);
    assert_eq!(st.st_nlink, 2);
}

#[test]
fn rename_overwrite_dest_no_exist() {
    let (test_path, _) = init("rename_overwrite_dest_no_exist");
    rename_overwrite(test_path, false)
}

#[test]
fn rename_overwrite_dest_exist() {
    let (test_path, _) = init("rename_overwrite_dest_exist");
    rename_overwrite(test_path, true)
}

fn rename_overwrite(test_path: PathBuf, dest_exist: bool) {
    let dir = test_path.join("dir");
    let dest = dir.join("renamed");
    let src = test_path.join("file");

    std::fs::create_dir(dir).unwrap();

    write(&src, "hello").unwrap();

    if dest_exist {
        write(&dest, "xx").unwrap();
    }

    let st = stat::stat(&src).unwrap();
    let before_ino = st.st_ino;

    fcntl::renameat(None, &src, None, &dest).unwrap();
    let st = stat::stat(&dest).unwrap();

    assert_eq!(before_ino, st.st_ino);
}

#[test]
fn read_unlink() {
    let (test_path, _) = init("rename_overwrite_dest_no_exist");
    let path = test_path.join("file");
    write(&path, "test content").unwrap();

    let mut file = File::open(&path).unwrap();
    unistd::unlink(&path).unwrap();

    let mut content = String::new();
    file.read_to_string(&mut content).unwrap();
    assert_eq!(content, "test content");
}

#[test]
fn append_write() {
    let (test_path, _) = init("append_write");
    let path = test_path.join("file");
    let mut file = OpenOptions::new()
        .append(true)
        .write(true)
        .create(true)
        .open(&path)
        .unwrap();

    file.write_all(b"hello").unwrap();
    file.write_all(b" world").unwrap();

    drop(file);

    let output = read_to_string(&path).unwrap();
    assert_eq!(output, "hello world");
}

#[test]
fn append_unlink_write() {
    let (test_path, _) = init("append_unlink_write");
    let path = test_path.join("file");
    let mut file = OpenOptions::new()
        .append(true)
        .write(true)
        .create(true)
        .open(&path)
        .unwrap();
    let mut read_file = File::open(&path).unwrap();

    file.write_all(b"hello").unwrap();
    unistd::unlink(&path).unwrap();
    file.write_all(b" world").unwrap();

    drop(file);

    let mut output = String::new();
    read_file.read_to_string(&mut output).unwrap();
    assert_eq!(&output, "hello world");
}

// func RenameOpenDir(t *testing.T, mnt string) {
// 	if err := os.Mkdir(mnt+"/dir1", 0755); err != nil {
// 		t.Fatalf("Mkdir: %v", err)
// 	}
// 	// Different permissions so directories are easier to tell apart
// 	if err := os.Mkdir(mnt+"/dir2", 0700); err != nil {
// 		t.Fatalf("Mkdir: %v", err)
// 	}

// 	var st1 syscall.Stat_t
// 	if err := syscall.Stat(mnt+"/dir2", &st1); err != nil {
// 		t.Fatalf("Stat: %v", err)
// 	}

// 	fd, err := syscall.Open(mnt+"/dir2", syscall.O_RDONLY, 0)
// 	if err != nil {
// 		t.Fatalf("Open: %v", err)
// 	}
// 	defer syscall.Close(fd)
// 	if err := syscall.Rename(mnt+"/dir1", mnt+"/dir2"); err != nil {
// 		t.Fatalf("Rename: %v", err)
// 	}

// 	var st2 syscall.Stat_t
// 	if err := syscall.Fstat(fd, &st2); err != nil {
// 		t.Skipf("Fstat failed: %v. Known limitation - see https://github.com/hanwen/go-fuse/issues/55", err)
// 	}
// 	if st2.Mode&syscall.S_IFMT != syscall.S_IFDIR {
// 		t.Errorf("got mode %o, want %o", st2.Mode, syscall.S_IFDIR)
// 	}
// 	if st2.Ino != st1.Ino {
// 		t.Errorf("got ino %d, want %d", st2.Ino, st1.Ino)
// 	}
// 	if st2.Mode&0777 != st1.Mode&0777 {
// 		t.Skipf("got permissions %#o, want %#o. Known limitation - see https://github.com/hanwen/go-fuse/issues/55",
// 			st2.Mode&0777, st1.Mode&0777)
// 	}
// }

// // ReadDir creates 110 files one by one, checking that we get the expected
// // entries after each file creation.
// func ReadDir(t *testing.T, mnt string) {
// 	want := map[string]bool{}
// 	// 40 bytes of filename, so 110 entries overflows a
// 	// 4096 page.
// 	for i := 0; i < 110; i++ {
// 		nm := fmt.Sprintf("file%036x", i)
// 		want[nm] = true
// 		if err := ioutil.WriteFile(filepath.Join(mnt, nm), []byte("hello"), 0644); err != nil {
// 			t.Fatalf("WriteFile %q: %v", nm, err)
// 		}
// 		// Verify that we get the expected entries
// 		f, err := os.Open(mnt)
// 		if err != nil {
// 			t.Fatalf("Open: %v", err)
// 		}
// 		names, err := f.Readdirnames(-1)
// 		if err != nil {
// 			t.Fatalf("ReadDir: %v", err)
// 		}
// 		f.Close()
// 		got := map[string]bool{}
// 		for _, e := range names {
// 			got[e] = true
// 		}
// 		if len(got) != len(want) {
// 			t.Errorf("got %d entries, want %d", len(got), len(want))
// 		}
// 		for k := range got {
// 			if !want[k] {
// 				t.Errorf("got unknown name %q", k)
// 			}
// 		}
// 	}
// }

// // Readdir should pick file created after open, but before readdir.
// func ReadDirPicksUpCreate(t *testing.T, mnt string) {
// 	f, err := os.Open(mnt)
// 	if err != nil {
// 		t.Fatalf("Open: %v", err)
// 	}

// 	if err := ioutil.WriteFile(mnt+"/file", []byte{42}, 0644); err != nil {
// 		t.Fatalf("WriteFile: %v", err)
// 	}
// 	names, err := f.Readdirnames(-1)
// 	if err != nil {
// 		t.Fatalf("ReadDir: %v", err)
// 	}
// 	f.Close()

// 	if len(names) != 1 || names[0] != "file" {
// 		t.Errorf("missing file created after opendir")
// 	}
// }

// // LinkUnlinkRename implements rename with a link/unlink sequence
// func LinkUnlinkRename(t *testing.T, mnt string) {
// 	content := []byte("hello")
// 	tmp := mnt + "/tmpfile"
// 	if err := ioutil.WriteFile(tmp, content, 0644); err != nil {
// 		t.Fatalf("WriteFile %q: %v", tmp, err)
// 	}

// 	dest := mnt + "/file"
// 	if err := syscall.Link(tmp, dest); err != nil {
// 		t.Fatalf("Link %q %q: %v", tmp, dest, err)
// 	}
// 	if err := syscall.Unlink(tmp); err != nil {
// 		t.Fatalf("Unlink %q: %v", tmp, err)
// 	}

// 	if back, err := ioutil.ReadFile(dest); err != nil {
// 		t.Fatalf("Read %q: %v", dest, err)
// 	} else if bytes.Compare(back, content) != 0 {
// 		t.Fatalf("Read got %q want %q", back, content)
// 	}
// }
