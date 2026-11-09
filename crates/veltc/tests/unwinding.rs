//! Catching a Velt panic that unwinds (docs/internals/design/unwinding.md), built with each
//! backend: `catchUnwind` returns the panic's message and the process goes on, a panic outside
//! any catch still ends the process, and another language's exception passes the catch (macOS
//! and Linux: on Windows `catchUnwind` is a plain call until its structured exceptions are built). The
//! probe module re-exports std's internal `catchVeltPanic`, so std is copied with it added (as
//! gated_dropped.rs does).

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod test_dir;

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            copy_tree(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}

/// `velt <args> main.velt`, with a std holding `PROBE` as `velt/zz_unwind`.
fn velt(dir: &test_dir::TestDir, main: &str, args: &[&str]) -> Output {
    let std: PathBuf = dir.path().join("std");
    if !std.exists() {
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
            &std,
        );
        std::fs::create_dir_all(std.join("zz_unwind")).unwrap();
        std::fs::write(std.join("zz_unwind/index.velt"), PROBE).unwrap();
    }
    let file = dir.path().join("main.velt");
    std::fs::write(&file, main).unwrap();
    let mut all: Vec<&str> = args.to_vec();
    all.push(file.to_str().unwrap());
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(&all)
        .env("VELT_STD", &std)
        .current_dir(dir.path())
        .output()
        .unwrap()
}

/// std's catch, and an exception of another language (class `OTHRLANG`) raised with the
/// platform's unwinder. Its payload after the 32-byte header reads as a message, so a catch that
/// took it by mistake hands back `FOREIGN-CAUGHT`.
const PROBE: &str = r#"
import { Ptr, StrBytes } from "velt/ffi";
import { ptrAddress, ptrOf } from "velt/ffi/ptr";

export { catchVeltPanic } from "velt/task/unwind";

declare c("c") function malloc(n: usize): Ptr<u8> | null;
declare c("c") function memset(p: Ptr<u8>, c: i32, n: usize): Ptr<u8>;
declare c("c") function memcpy(dst: Ptr<u8>, src: StrBytes, n: usize): Ptr<u8>;
declare c("c") function _Unwind_RaiseException(e: Ptr<u8>): i32;
declare function velt_rt_flush(): void;

export function raiseForeign(): void {
  velt_rt_flush();
  // SAFETY: a zeroed header of 32 bytes with a class, which the unwinder only reads, then a
  // NUL-terminated text inside the allocation.
  unsafe {
    const e = malloc(48);
    if (e === null) {
      return;
    }
    memset(e, 0, 48);
    memcpy(e, "OTHRLANG", 8);
    memcpy(ptrOf<u8>(ptrAddress(e) + 32), "FOREIGN-CAUGHT", 14);
    _Unwind_RaiseException(e);
  }
}
"#;

#[cfg(not(windows))]
const MAIN: &str = r#"
import { catchVeltPanic } from "velt/zz_unwind";

function deep(n: number, xs: number[]): number {
  if (n == 0) {
    return xs[xs.length + 3];
  }
  return deep(n - 1, xs) + 1;
}

function first(m: string | null): string {
  if (m === null) {
    return "null";
  }
  return m.split(" at ")[0];
}

function main() {
  let ran = 0;
  console.log(first(catchVeltPanic(() => { ran = ran + 1; })), ran);
  console.log(first(catchVeltPanic(() => { panic("boom"); })));
  let after = 0;
  console.log(first(catchVeltPanic(() => { deep(30, [1, 2, 3]); after = 1; })), after);
  let inner: string | null = null;
  const outer = catchVeltPanic(() => { inner = catchVeltPanic(() => { panic("inner"); }); });
  console.log(first(outer), first(inner));
  panic("uncaught");
}
"#;

#[cfg(not(windows))]
const OUT: &str = "null 1
boom
index out of bounds: the len is 3 but the index is 6 0
null inner
";

/// Another language's exception raised inside a catch.
#[cfg(not(windows))]
const FOREIGN: &str = r#"
import { catchVeltPanic, raiseForeign } from "velt/zz_unwind";

function first(m: string | null): string {
  return m === null ? "null" : m;
}

function main() {
  console.log("raising");
  let returned = false;
  const foreign = catchVeltPanic(() => { raiseForeign(); returned = true; });
  console.log(first(foreign), returned);
}
"#;

#[cfg(not(windows))]
#[test]
fn a_velt_panic_is_caught_with_every_backend() {
    let dir = test_dir::TestDir::new();
    for args in [&["run"][..], &["run", "--release"][..]] {
        let out = velt(&dir, MAIN, args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            OUT,
            "{args:?}: {stderr}"
        );
        // The uncaught panic ends the process as before; each caught one was printed too.
        assert_eq!(out.status.code(), Some(101), "{args:?}: {stderr}");
        for msg in ["panic: boom", "panic: inner", "panic: uncaught"] {
            assert!(stderr.contains(msg), "{args:?}: {stderr}");
        }
    }
}

/// An exception of another class passes `catchVeltPanic`: the catch never returns it. What
/// happens next depends on the frames above `main`, which this test doesn't rely on: with no
/// frame claiming it the raise returns and the program goes on (`null true`); a Rust frame above
/// `main` (an `extern "C"` function's abort-on-unwind pad, which Rust's personality claims for
/// any class) ends the process there. Either way the catch didn't take it: a catch that claims
/// every class hands back the payload as the message, `FOREIGN-CAUGHT`.
#[cfg(not(windows))]
#[test]
fn another_languages_exception_passes_the_catch() {
    let dir = test_dir::TestDir::new();
    for args in [&["run"][..], &["run", "--release"][..]] {
        let out = velt(&dir, FOREIGN, args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = format!("{args:?}: {:?}\n{stdout}{stderr}", out.status);
        assert!(!stdout.contains("FOREIGN-CAUGHT"), "{why}");
        match stdout.as_ref() {
            "raising\nnull true\n" => assert!(out.status.success(), "{why}"),
            "raising\n" => assert!(!out.status.success(), "{why}"),
            _ => panic!("{why}"),
        }
    }
}

/// A program that never calls `catchUnwind` keeps calling the runtime's panic directly: the
/// std panic path isn't even compiled in.
#[test]
fn a_program_without_catch_keeps_the_runtime_panic() {
    let dir = test_dir::TestDir::new();
    let out = velt(
        &dir,
        "const xs = [1];\nconsole.log(xs[3]);\n",
        &["build", "--emit", "vir"],
    );
    let vir = String::from_utf8_lossy(&out.stdout);
    assert!(vir.contains("velt_rt_panic"), "{vir}");
    assert!(
        !vir.contains("panicRaise") && !vir.contains("velt_eh_try"),
        "{vir}"
    );
}
