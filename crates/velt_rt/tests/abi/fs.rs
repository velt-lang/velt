//! `std/fs` roundtrips in a temp directory: the `*_sync` variants (what a native build runs on the
//! executor's blocking pool), and the file streams' leaves awaited in a task.

use super::fake::{arg, block_on_fut, err, ok, take_string};
use crate::bytes::{velt_rt_bytes_drop, VeltBytes};
use crate::fs::sync::*;
use crate::result::{code, IoResult};
use std::mem::MaybeUninit;
use std::path::PathBuf;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("velt_rt_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn path(base: &std::path::Path, rel: &str) -> String {
    base.join(rel).to_string_lossy().into_owned()
}

fn sync<T>(f: impl FnOnce(*mut IoResult<T>)) -> IoResult<T> {
    let mut out = MaybeUninit::<IoResult<T>>::uninit();
    f(out.as_mut_ptr());
    unsafe { out.assume_init() }
}

#[test]
fn sync_roundtrip() {
    let dir = temp_dir("fs_sync");
    let (d, a, b) = (
        path(&dir, "n"),
        path(&dir, "n/a.txt"),
        path(&dir, "n/b.txt"),
    );
    std::fs::create_dir_all(&d).unwrap();
    unsafe {
        std::fs::write(&a, "one+two").unwrap();
        ok(sync(|o| velt_rt_fs_copy_file_sync(&arg(&a), &arg(&b), o)));
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "one+two");
        ok(sync(|o| {
            velt_rt_fs_rename_sync(&arg(&b), &arg(&path(&dir, "n/c.txt")), o)
        }));
        let mut names: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a.txt", "c.txt"]);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Symlinks need Developer Mode or administrator rights on Windows: without them this test
/// checks nothing (the `fs_symlinks` golden is skipped there too).
#[test]
fn symlinks() {
    let dir = temp_dir("fs_links");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("f.txt"), "hello").unwrap();
    let (f, link) = (path(&dir, "f.txt"), path(&dir, "link"));
    unsafe {
        let made = sync(|o| velt_rt_fs_symlink_sync(&arg("f.txt"), &arg(&link), o));
        if made.err.code == code::PERMISSION_DENIED && cfg!(windows) {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        ok(made);
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        let target = ok(sync(|o| velt_rt_fs_readlink_sync(&arg(&link), o)));
        assert_eq!(take_string(target), "f.txt");
        err(
            sync(|o| velt_rt_fs_readlink_sync(&arg(&f), o)),
            code::INVALID_INPUT,
        );
        // realpath resolves the link to the file's absolute path, without Windows' `\\?\`.
        let real = take_string(ok(sync(|o| velt_rt_fs_realpath_sync(&arg(&link), o))));
        let expected = take_string(ok(sync(|o| velt_rt_fs_realpath_sync(&arg(&f), o))));
        assert_eq!(real, expected);
        assert!(
            real.ends_with("f.txt") && !real.starts_with(r"\\?\"),
            "{real}"
        );
        assert!(std::path::Path::new(&real).is_absolute(), "{real}");
        err(
            sync(|o| velt_rt_fs_realpath_sync(&arg(&path(&dir, "none")), o)),
            code::NOT_FOUND,
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `openRead(path, { start, end })`: both ends included, an end past the file stops at its end,
/// and `openedStats()` describes the opened file, whatever range it reads.
#[test]
fn ranged_readers_and_their_stats() {
    use crate::fs::stream::*;
    let dir = temp_dir("fs_range");
    std::fs::create_dir_all(&dir).unwrap();
    let f = path(&dir, "f.txt");
    std::fs::write(&f, "0123456789").unwrap();
    let read_all = |start: u64, end: u64| -> String {
        unsafe {
            let r = ok(block_on_fut::<IoResult<ReaderHandle>>(
                velt_rt_fs_open_read_range(&arg(&f), start, end),
            ));
            let mut out = Vec::new();
            loop {
                let mut chunk = ok(block_on_fut::<IoResult<VeltBytes>>(velt_rt_fs_reader_read(
                    r, 4,
                )));
                let bytes = chunk.as_bytes().to_vec();
                velt_rt_bytes_drop(&mut chunk);
                if bytes.is_empty() {
                    break;
                }
                out.extend(bytes);
            }
            velt_rt_fs_reader_close(r);
            String::from_utf8(out).unwrap()
        }
    };
    assert_eq!(read_all(2, 5), "2345");
    assert_eq!(read_all(9, 9), "9");
    assert_eq!(read_all(7, 100), "789");
    assert_eq!(read_all(0, u64::MAX), "0123456789");
    assert_eq!(read_all(3, u64::MAX), "3456789");
    assert_eq!(read_all(10, u64::MAX), "");
    assert_eq!(read_all(5, 4), "");
    assert_eq!(read_all(u64::MAX - 1, u64::MAX - 1), "");
    unsafe {
        let r = ok(block_on_fut::<IoResult<ReaderHandle>>(
            velt_rt_fs_open_read_range(&arg(&f), 1, 2),
        ));
        // Replaced after the open: the reader still describes the file it opened.
        std::fs::write(&f, "a longer file than before").unwrap();
        let s = ok(sync(|o| velt_rt_fs_reader_opened_stats(r, o)));
        assert_eq!((s.size, s.is_file, s.is_dir), (10, 1, 0));
        velt_rt_fs_reader_close(r);
        // A closed reader's are EBADF, as its reads are.
        let closed = sync(|o| velt_rt_fs_reader_opened_stats(r, o));
        assert!(closed.err.code != 0);
        err(
            block_on_fut::<IoResult<ReaderHandle>>(velt_rt_fs_open_read_range(
                &arg(&path(&dir, "none")),
                1,
                2,
            )),
            code::NOT_FOUND,
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
