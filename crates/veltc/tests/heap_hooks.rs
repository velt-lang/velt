//! The allocator of release builds (#2196): std's hooks over mimalloc (std/lang/hooks/heap.velt),
//! which `velt_opt`'s `heap_calls` calls in place of the runtime's allocator. The hooks are
//! std-only, so a golden can't call them: std is copied with a probe module added that exports
//! them, and `velt` uses it through `$VELT_STD` (as str_data_once.rs does). Release builds only:
//! a debug build links the runtime's shared library, which keeps mimalloc to itself.
//!
//! What the goldens can't reach is checked here: a size of 0, alignments past 8, growing and
//! shrinking with the contents kept, a reallocation past the array length limit and one larger
//! than memory, each stop with the runtime's message. And which builds call the hooks: release
//! builds, not debug ones, nor a release build asked to count blocks (`VELT_RC_STATS=1`).

use std::path::Path;
use std::process::Output;

mod no_window;
mod test_dir;

const PROBE: &str = r#"
import { addressLoad, addressStore } from "velt/lang/intrinsics";
import { heapAlloc, heapFree, heapRealloc } from "velt/lang/hooks/heap";

export function alloc(size: u64, align: u64): u64 {
  return heapAlloc(size, align);
}

export function realloc(p: u64, old: u64, align: u64, size: u64): u64 {
  return heapRealloc(p, old, align, size);
}

export function free(p: u64, size: u64, align: u64): void {
  heapFree(p, size, align);
}

/** Writes `n` words `i * 3 + 1` from `p`. */
export function fill(p: u64, n: u64): void {
  for (let i: u64 = 0; i < n; i++) {
    // SAFETY: `p` holds at least `n` words.
    unsafe {
      addressStore<u64>((p + i * 8) as usize, i * 3 + 1);
    }
  }
}

/** Whether the `n` words from `p` are what `fill` wrote. */
export function filled(p: u64, n: u64): boolean {
  for (let i: u64 = 0; i < n; i++) {
    // SAFETY: as in `fill`.
    if (unsafe { addressLoad<u64>((p + i * 8) as usize) } != i * 3 + 1) {
      return false;
    }
  }
  return true;
}
"#;

const MAIN: &str = r#"
import { alloc, fill, filled, free, realloc } from "velt/zz_probe";

function main() {
  // A size of 0 takes no memory: its alignment comes back, and its free does nothing.
  console.log(alloc(0, 8), alloc(0, 1), alloc(0, 64));
  free(8, 0, 8);
  // Distinct blocks, aligned as asked.
  const aligns: u64[] = [1, 8, 16, 64, 4096];
  for (const align of aligns) {
    const a = alloc(24, align);
    const b = alloc(24, align);
    fill(a, 3);
    fill(b, 3);
    console.log(align, a != b, a % align, b % align, filled(a, 3) && filled(b, 3));
    free(a, 24, align);
    free(b, 24, align);
  }
  // Growing and shrinking keep the contents.
  const big: u64[] = [8, 64];
  for (const align of big) {
    let p = alloc(16, align);
    fill(p, 2);
    p = realloc(p, 16, align, 1048576);
    const grown = filled(p, 2) && p % align == 0;
    fill(p, 131072);
    p = realloc(p, 1048576, align, 32);
    console.log(align, grown, filled(p, 4), p % align);
    free(p, 32, align);
  }
  // An empty block is allocated, and a size of 0 frees.
  const q = realloc(8, 0, 8, 40);
  fill(q, 5);
  console.log(q != 8, filled(q, 5), realloc(q, 40, 8, 0), realloc(16, 0, 16, 0));
}
"#;

const EXPECTED: &str = "\
8 1 64
1 true 0 0 true
8 true 0 0 true
16 true 0 0 true
64 true 0 0 true
4096 true 0 0 true
8 true true 0
64 true true 0
true true 8 16
";

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

/// std with the probe, in `dir`.
fn probe_std(dir: &test_dir::TestDir) {
    let std = dir.path().join("std");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        &std,
    );
    std::fs::create_dir_all(std.join("zz_probe")).unwrap();
    std::fs::write(std.join("zz_probe/index.velt"), PROBE).unwrap();
}

/// `velt <args>` over `main` (written as `name`.velt) with the probe std, without the variables
/// that keep the runtime's allocator in a release build.
fn velt(dir: &test_dir::TestDir, name: &str, main: &str, args: &[&str]) -> Output {
    let file = format!("{name}.velt");
    std::fs::write(dir.path().join(&file), main).unwrap();
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .arg(&file)
        .env("VELT_STD", dir.path().join("std"))
        .env("VELT_BUILD_CACHE", "off")
        .env_remove("VELT_RC_STATS")
        .env_remove("VELT_RT_DEBUG_ALLOC")
        .current_dir(dir.path())
        .output()
        .unwrap()
}

fn text(what: &str, o: &Output) -> String {
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{what}: {out}\n{err}");
    out
}

#[test]
fn the_hooks_allocate_grow_and_free() {
    let dir = test_dir::TestDir::new();
    probe_std(&dir);
    let out = velt(&dir, "main", MAIN, &["run", "--release"]);
    assert_eq!(text("run", &out), EXPECTED);
}

/// `main` built for release stops with exit code 101 and `message` on stderr.
fn stops(dir: &test_dir::TestDir, name: &str, body: &str, message: &str) {
    let main = format!(
        "import {{ alloc, free, realloc }} from \"velt/zz_probe\";\n\
         function main() {{\n  console.log(\"before\");\n  {body}\n  console.log(\"after\");\n}}\n"
    );
    let out = velt(dir, name, &main, &["run", "--release"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(101), "{name}: {stdout}\n{stderr}");
    assert_eq!(stdout, "before\n", "{name}: {stderr}");
    assert!(stderr.contains(message), "{name}: {stderr}");
}

#[test]
fn past_the_limit_or_memory_the_program_stops_with_the_runtimes_messages() {
    let dir = test_dir::TestDir::new();
    probe_std(&dir);
    let limit = "RangeError: Invalid array length: an array holds at most 2^52 - 1 elements, and \
                 its elements at most 2^52 - 1 bytes";
    // Past the length limit: a block that grows, and an empty one.
    let body = "const p = alloc(16, 8);\n  realloc(p, 16, 8, 4503599627370496);\n  free(p, 16, 8);";
    stops(&dir, "grow_past_limit", body, limit);
    let body = "realloc(8, 0, 8, 9223372036854775808);";
    stops(&dir, "empty_past_limit", body, limit);
    // Within the limit, larger than memory, plain and aligned.
    let body = "const p = alloc(16, 8);\n  realloc(p, 16, 8, 4503599627370495);\n  free(p, 16, 8);";
    stops(
        &dir,
        "grow_past_memory",
        body,
        "memory allocation of 4503599627370495 bytes failed",
    );
    let body =
        "const p = alloc(64, 64);\n  realloc(p, 64, 64, 4503599627370432);\n  free(p, 64, 64);";
    stops(
        &dir,
        "grow_aligned_past_memory",
        body,
        "memory allocation of 4503599627370432 bytes failed",
    );
    let body = "free(alloc(1152921504606846976, 64), 1152921504606846976, 64);";
    stops(
        &dir,
        "aligned_past_memory",
        body,
        "memory allocation of 1152921504606846976 bytes failed",
    );
}

/// The VIR of `main` built with `args` (and `env` set) calls the runtime's allocator, or std's
/// hooks in its place.
fn calls(dir: &test_dir::TestDir, args: &[&str], env: Option<(&str, &str)>) -> (bool, bool) {
    let main = "class Point {\n  constructor(public x: number, public y: number) {}\n}\n\
                function main() {\n  const ps: Point[] = [];\n  for (let i = 0; i < 3; i++) {\n    \
                ps.push(new Point(i, i));\n  }\n  console.log(ps.length);\n}\n";
    std::fs::write(dir.path().join("points.velt"), main).unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["build", "--emit", "vir"])
        .args(args)
        .arg("points.velt")
        .env("VELT_STD", dir.path().join("std"))
        .env("VELT_BUILD_CACHE", "off")
        .env_remove("VELT_RC_STATS")
        .env_remove("VELT_RT_DEBUG_ALLOC")
        .current_dir(dir.path());
    if let Some((k, v)) = env {
        cmd.env(k, v);
    }
    let vir = text("vir", &cmd.output().unwrap());
    let runtime = vir
        .lines()
        .any(|l| l.contains("call") && l.contains("velt_rt_alloc"));
    let hooks = vir.contains("P4heapN9heapAlloc");
    (runtime, hooks)
}

#[test]
fn release_builds_call_the_hooks_and_debug_builds_the_runtime() {
    let dir = test_dir::TestDir::new();
    probe_std(&dir);
    assert_eq!(calls(&dir, &["--release"], None), (false, true));
    assert_eq!(calls(&dir, &[], None), (true, false));
    // Asked to count or check blocks, a release build keeps the runtime's allocator.
    let counted = Some(("VELT_RC_STATS", "1"));
    assert_eq!(calls(&dir, &["--release"], counted), (true, false));
    let checked = Some(("VELT_RT_DEBUG_ALLOC", "1"));
    assert_eq!(calls(&dir, &["--release"], checked), (true, false));
}
