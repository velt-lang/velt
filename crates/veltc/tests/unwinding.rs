//! Catching a Velt panic that unwinds (docs/internals/design/unwinding.md), built with each
//! backend: `catchUnwind` returns the panic's message and the process goes on, a panic outside
//! any catch still ends the process, and another language's exception passes the catch. The
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
/// platform's unwinder, which returns when no frame claims it. What it returns is no reliable
/// reason code: 5 (end of stack) or 3 (a frame that ends the search, a Rust `extern "C"`
/// function) on macOS, any word on Linux arm64 (libgcc, from C too), so only the return counts.
const PROBE: &str = r#"
import { Ptr, StrBytes } from "velt/ffi";

export { catchVeltPanic } from "velt/task/unwind";

declare c("c") function malloc(n: usize): Ptr<u8> | null;
declare c("c") function memset(p: Ptr<u8>, c: i32, n: usize): Ptr<u8>;
declare c("c") function memcpy(dst: Ptr<u8>, src: StrBytes, n: usize): Ptr<u8>;
declare c("c") function _Unwind_RaiseException(e: Ptr<u8>): i32;

export function raiseForeign(): i32 {
  // SAFETY: a zeroed header of 32 bytes with a class, which the unwinder only reads.
  unsafe {
    const e = malloc(32);
    if (e === null) {
      return -1;
    }
    memset(e, 0, 32);
    memcpy(e, "OTHRLANG", 8);
    return _Unwind_RaiseException(e);
  }
}
"#;

const MAIN: &str = r#"
import { catchVeltPanic, raiseForeign } from "velt/zz_unwind";

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
  let returned = false;
  const foreign = catchVeltPanic(() => { raiseForeign(); returned = true; });
  console.log(first(foreign), returned);
  panic("uncaught");
}
"#;

const OUT: &str = "null 1
boom
index out of bounds: the len is 3 but the index is 6 0
null inner
null true
";

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
