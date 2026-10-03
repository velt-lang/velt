//! `std/fs` roundtrips in a temp directory: async leaves awaited through `velt_rt_block_on`, and
//! the `*_sync` variants.

use super::fake::{arg, block_on_fut, err, ok, take_string};
use crate::bytes::{velt_rt_bytes_drop, VeltBytes};
use crate::fs::sync::*;
use crate::fs::*;
use crate::result::{code, IoResult};
use crate::str::VeltStr;
use crate::str_array::{velt_rt_str_array_drop, VeltStrArray};
use std::mem::MaybeUninit;
use std::path::PathBuf;

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("velt_rt_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn names(mut a: VeltStrArray) -> Vec<String> {
    let v = (0..a.len as usize)
        .map(|i| String::from_utf8(unsafe { (*a.ptr.add(i)).as_bytes() }.to_vec()).unwrap())
        .collect();
    unsafe { velt_rt_str_array_drop(&mut a) };
    v
}

fn path(base: &std::path::Path, rel: &str) -> String {
    base.join(rel).to_string_lossy().into_owned()
}

#[test]
fn promises_are_lazy() {
    let dir = temp_dir("fs_lazy");
    std::fs::create_dir_all(&dir).unwrap();
    let file = path(&dir, "never.txt");
    // const p = writeFile(file, "x");  (dropped without await/spawn: nothing happens)
    let p = unsafe { velt_rt_fs_write_file(&arg(&file), &arg("x")) };
    std::thread::sleep(std::time::Duration::from_millis(50));
    unsafe { crate::task::velt_rt_fut_drop(p) };
    assert!(!std::path::Path::new(&file).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn async_roundtrip() {
    let dir = temp_dir("fs_async");
    let (d, a, b, c) = (
        path(&dir, "x/y"),
        path(&dir, "x/y/a.txt"),
        path(&dir, "x/y/b.txt"),
        path(&dir, "x/c.bin"),
    );
    unsafe {
        ok(block_on_fut::<IoResult<()>>(velt_rt_fs_mkdir(&arg(&d), 1)));
        ok(block_on_fut::<IoResult<()>>(velt_rt_fs_write_file(
            &arg(&a),
            &arg("héllo"),
        )));
        ok(block_on_fut::<IoResult<()>>(velt_rt_fs_append_file(
            &arg(&a),
            &arg(" wörld"),
        )));
        let text = ok(block_on_fut::<IoResult<VeltStr>>(velt_rt_fs_read_file(
            &arg(&a),
        )));
        assert_eq!(take_string(text), "héllo wörld");

        let st = ok(block_on_fut::<IoResult<VeltStat>>(velt_rt_fs_stat(&arg(
            &a,
        ))));
        assert_eq!((st.size, st.is_file, st.is_dir), (13, 1, 0));
        assert!(st.mtime_ms > 1.6e12);
        let st = ok(block_on_fut::<IoResult<VeltStat>>(velt_rt_fs_stat(&arg(
            &d,
        ))));
        assert_eq!((st.is_file, st.is_dir), (0, 1));

        ok(block_on_fut::<IoResult<()>>(velt_rt_fs_copy_file(
            &arg(&a),
            &arg(&b),
        )));
        ok(block_on_fut::<IoResult<()>>(velt_rt_fs_rename(
            &arg(&b),
            &arg(&c),
        )));
        let listing = ok(block_on_fut::<IoResult<VeltStrArray>>(velt_rt_fs_read_dir(
            &arg(&path(&dir, "x")),
        )));
        assert_eq!(names(listing), ["c.bin", "y"]);

        // Bytes that are not UTF-8, which no string can hold: written from outside.
        let raw = [0u8, 0xff, 7];
        std::fs::write(&c, raw).unwrap();
        let mut got = ok(block_on_fut::<IoResult<VeltBytes>>(
            velt_rt_fs_read_file_bytes(&arg(&c)),
        ));
        assert_eq!(got.as_bytes(), raw);
        velt_rt_bytes_drop(&mut got);
        let msg = err(
            block_on_fut::<IoResult<VeltStr>>(velt_rt_fs_read_file(&arg(&c))),
            code::INVALID_DATA,
        );
        assert!(msg.contains("UTF-8"));

        assert_eq!(block_on_fut::<u8>(velt_rt_fs_exists(&arg(&c))), 1);
        let x = path(&dir, "x");
        err(
            block_on_fut::<IoResult<()>>(velt_rt_fs_remove(&arg(&x), 0)),
            code::DIRECTORY_NOT_EMPTY,
        );
        ok(block_on_fut::<IoResult<()>>(velt_rt_fs_remove(&arg(&x), 1)));
        assert_eq!(block_on_fut::<u8>(velt_rt_fs_exists(&arg(&c))), 0);
        err(
            block_on_fut::<IoResult<VeltStr>>(velt_rt_fs_read_file(&arg(&a))),
            code::NOT_FOUND,
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
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
    unsafe {
        ok(sync(|o| velt_rt_fs_mkdir_sync(&arg(&d), 1, o)));
        err(
            sync(|o| velt_rt_fs_mkdir_sync(&arg(&d), 0, o)),
            code::ALREADY_EXISTS,
        );
        ok(sync(|o| {
            velt_rt_fs_write_file_sync(&arg(&a), &arg("one"), o)
        }));
        ok(sync(|o| {
            velt_rt_fs_append_file_sync(&arg(&a), &arg("+two"), o)
        }));
        assert_eq!(
            take_string(ok(sync(|o| velt_rt_fs_read_file_sync(&arg(&a), o)))),
            "one+two"
        );
        let mut bytes = ok(sync(|o| velt_rt_fs_read_file_bytes_sync(&arg(&a), o)));
        assert_eq!(bytes.as_bytes(), b"one+two");
        velt_rt_bytes_drop(&mut bytes);
        assert_eq!(ok(sync(|o| velt_rt_fs_stat_sync(&arg(&a), o))).size, 7);
        ok(sync(|o| velt_rt_fs_copy_file_sync(&arg(&a), &arg(&b), o)));
        ok(sync(|o| {
            velt_rt_fs_rename_sync(&arg(&b), &arg(&path(&dir, "n/c.txt")), o)
        }));
        assert_eq!(
            names(ok(sync(|o| velt_rt_fs_read_dir_sync(&arg(&d), o)))),
            ["a.txt", "c.txt"]
        );
        assert_eq!(velt_rt_fs_exists_sync(&arg(&a)), 1);
        ok(sync(|o| velt_rt_fs_remove_sync(&arg(&a), 0, o)));
        assert_eq!(velt_rt_fs_exists_sync(&arg(&a)), 0);
        err(sync(|o| velt_rt_fs_stat_sync(&arg(&a), o)), code::NOT_FOUND);
        ok(sync(|o| velt_rt_fs_remove_sync(&arg(&d), 1, o)));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
