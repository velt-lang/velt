//! The allocator of release builds (#2196): std's hooks over mimalloc (std/lang/hooks/heap.velt),
//! which `velt_opt`'s `heap_calls` calls in place of the runtime's allocator. The hooks are
//! std-only, so a golden can't call them: std is copied with a probe module added that exports
//! them, and `velt` uses it through `$VELT_STD` (as str_data_once.rs does). Release builds only:
//! a debug build links the runtime's shared library, which keeps mimalloc to itself.
//!
//! What the goldens can't reach is checked here: a size of 0, alignments past 8, growing and
//! shrinking with the contents kept, a reallocation past the array length limit and one larger
//! than memory, each stop with the runtime's message. And which builds call the hooks: release
//! builds, not debug ones, nor a release build asked to count blocks (`VELT_RC_STATS=1`), nor one
//! linked against a debug runtime, whose checks and counts are asked for when the program runs.
//!
//! Std code a release build links as an object compiled before it (the std cache's `@noInline`
//! functions, prebuilt hook modules) allocates as the build's own code does: its objects are
//! compiled and found per allocator.
//!
//! The runtime these tests link is the profile's own: a debug one under `cargo test`. The probes
//! set `VELT_HEAP_HOOKS=1`, which makes a release build call the hooks with it too (they never
//! run with `VELT_RT_DEBUG_ALLOC` or `VELT_RC_STATS` set).

use std::path::Path;
use std::process::Output;

mod no_window;
mod runtime_support;
mod test_dir;

const PROBE: &str = r#"
import { addressLoad, addressStore } from "velt/lang/intrinsics";
import { heapAlloc, heapFree, heapRealloc } from "velt/lang/hooks/heap";
import { catchVeltPanic } from "velt/task/unwind";

/** `f` under `catchVeltPanic`: the panic's message, or null. */
export function caught(f: () => void): string | null {
  return catchVeltPanic(f);
}

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
  // Not even a live block's: it stays, with its contents, and the next allocation is another.
  const kept = alloc(24, 8);
  fill(kept, 3);
  free(kept, 0, 8);
  const next = alloc(24, 8);
  console.log(next != kept, filled(kept, 3));
  // `kept` stays (freeing it here would free it twice if the free of size 0 had taken it).
  free(next, 24, 8);
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
true true
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
/// The repository's std, or `$VELT_STD` when set (a mutated copy, to see these tests fail), copied
/// into `dir` with the probe module added.
fn probe_std(dir: &test_dir::TestDir) {
    let std = dir.path().join("std");
    let from = std::env::var_os("VELT_STD").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        std::path::PathBuf::from,
    );
    copy_tree(&from, &std);
    std::fs::create_dir_all(std.join("zz_probe")).unwrap();
    std::fs::write(std.join("zz_probe/index.velt"), PROBE).unwrap();
}

/// `velt <args>` over `main` (written as `name`.velt) with the probe std, without the variables
/// that keep the runtime's allocator in a release build, and with the hooks on whichever runtime
/// is linked.
fn velt(dir: &test_dir::TestDir, name: &str, main: &str, args: &[&str]) -> Output {
    let file = format!("{name}.velt");
    std::fs::write(dir.path().join(&file), main).unwrap();
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .arg(&file)
        .env("VELT_STD", dir.path().join("std"))
        .env("VELT_BUILD_CACHE", "off")
        .env("VELT_HEAP_HOOKS", "1")
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
        "import {{ alloc, caught, free, realloc }} from \"velt/zz_probe\";\n\
         function main() {{\n  console.log(\"before\");\n  {body}\n  console.log(\"after\");\n}}\n"
    );
    let out = velt(dir, name, &main, &["run", "--release"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(101), "{name}: {stdout}\n{stderr}");
    assert_eq!(stdout, "before\n", "{name}: {stderr}");
    // Both stop as the runtime's allocator does: the message alone on its line, no location (a
    // panic would add ` at <file>:<line>:<col>`).
    let line = format!("panic: {message}\n");
    assert!(
        stderr.replace("\r\n", "\n").contains(&line),
        "{name}: {stderr}"
    );
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
    // Not a panic: a `catchVeltPanic` running doesn't catch either stop.
    let body = "caught(() => {\n    realloc(8, 0, 8, 4503599627370496);\n  });";
    stops(&dir, "caught_past_limit", body, limit);
    let body = "caught(() => {\n    alloc(4503599627370495, 8);\n  });";
    stops(
        &dir,
        "caught_past_memory",
        body,
        "memory allocation of 4503599627370495 bytes failed",
    );
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

/// What the VIR of `points.velt` calls: the runtime's allocator, or std's hooks in its place.
#[derive(Debug, PartialEq)]
struct Calls {
    /// `velt_rt_alloc` and `velt_rt_realloc`, each.
    runtime: (bool, bool),
    /// std's `heapAlloc`.
    hooks: bool,
    /// velt_opt's wrappers of the hooks, for the run-time size and the growth, each.
    wrappers: (bool, bool),
}

/// The runtime's allocator throughout.
const RUNTIME: Calls = Calls {
    runtime: (true, true),
    hooks: false,
    wrappers: (false, false),
};

/// std's hooks throughout: the constant sizes directly, the rest through the wrappers.
const HOOKS: Calls = Calls {
    runtime: (false, false),
    hooks: true,
    wrappers: (true, true),
};

/// A program that allocates a constant size (`Point`), a run-time size (`xs`) and grows an
/// array (`push`).
const POINTS: &str = "class Point {\n  constructor(public x: number, public y: number) {}\n}\n\
    function main() {\n  const ps: Point[] = [];\n  for (let i = 0; i < 3; i++) {\n    \
    ps.push(new Point(i, i));\n  }\n  const xs = new Array<number>(ps.length + 5).fill(1);\n  \
    console.log(ps.length, xs.length);\n}\n";

/// `velt build --emit <emit>` of `points.velt` with `args` (and `env` set).
fn emit(dir: &test_dir::TestDir, emit: &str, args: &[&str], env: &[(&str, &str)]) -> String {
    std::fs::write(dir.path().join("points.velt"), POINTS).unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["build", "--emit", emit])
        .args(args)
        .arg("points.velt")
        .env("VELT_STD", dir.path().join("std"))
        .env("VELT_BUILD_CACHE", "off")
        .env_remove("VELT_HEAP_HOOKS")
        .env_remove("VELT_RC_STATS")
        .env_remove("VELT_RT_DEBUG_ALLOC")
        .current_dir(dir.path());
    for (k, v) in env {
        cmd.env(k, v);
    }
    text(emit, &cmd.output().unwrap())
}

/// What the VIR of `points.velt` built with `args` (and `env` set) calls.
fn calls(dir: &test_dir::TestDir, args: &[&str], env: &[(&str, &str)]) -> Calls {
    let vir = emit(dir, "vir", args, env);
    let called = |name: &str| vir.lines().any(|l| l.contains("call") && l.contains(name));
    Calls {
        runtime: (called("velt_rt_alloc"), called("velt_rt_realloc")),
        hooks: vir.contains("P4heapN9heapAlloc"),
        wrappers: (called("velt.heap.alloc"), called("velt.heap.realloc")),
    }
}

#[test]
fn release_builds_call_the_hooks_and_debug_builds_the_runtime() {
    let dir = test_dir::TestDir::new();
    probe_std(&dir);
    let on = ("VELT_HEAP_HOOKS", "1");
    assert_eq!(calls(&dir, &["--release"], &[on]), HOOKS);
    // A debug build, whose runtime counts every block (`VELT_RC_STATS`), and WebAssembly, whose
    // runtime keeps its allocator, call the runtime's.
    assert_eq!(calls(&dir, &[], &[on]), RUNTIME);
    let wasm = ["--release", "--target", "wasm32-wasip1"];
    assert_eq!(calls(&dir, &wasm, &[on]), RUNTIME);
    // Asked to count or check blocks, a release build keeps the runtime's allocator.
    let counted = ("VELT_RC_STATS", "1");
    assert_eq!(calls(&dir, &["--release"], &[on, counted]), RUNTIME);
    let checked = ("VELT_RT_DEBUG_ALLOC", "1");
    assert_eq!(calls(&dir, &["--release"], &[on, checked]), RUNTIME);
    let off = ("VELT_HEAP_HOOKS", "0");
    assert_eq!(calls(&dir, &["--release"], &[off]), RUNTIME);
    // Without the variable, the runtime decides: a debug one (this profile's) keeps its
    // allocator, so the program may still run with its checks and counts.
    let plain = calls(&dir, &["--release"], &[]);
    if cfg!(debug_assertions) {
        assert_eq!(plain, RUNTIME);
    }
}

/// The wrappers are the runtime's allocator to LLVM (rt_abi.md "Memory"): their definitions
/// carry `velt_rt_alloc`'s and `velt_rt_realloc`'s attributes, and stay out of line, so a loop
/// that grows an array sees the same call as with the runtime's.
#[test]
fn the_wrappers_carry_the_runtime_allocators_attributes() {
    let dir = test_dir::TestDir::new();
    probe_std(&dir);
    let ir = emit(&dir, "llvm", &["--release"], &[("VELT_HEAP_HOOKS", "1")]);
    let define = |name: &str| {
        let at = format!("@\"{name}\"(");
        let line = ir
            .lines()
            .find(|l| l.starts_with("define") && l.contains(&at));
        line.unwrap_or_else(|| panic!("no definition of {name}:\n{ir}"))
    };
    let group = |n: &str| {
        let line = ir
            .lines()
            .find(|l| l.starts_with(&format!("attributes {n} =")));
        line.unwrap_or_else(|| panic!("no attribute group {n}"))
    };
    let alloc = define("velt.heap.alloc");
    assert!(alloc.contains(" noalias noundef ptr "), "{alloc}");
    assert!(alloc.contains("i64 noundef allocalign"), "{alloc}");
    let realloc = define("velt.heap.realloc");
    assert!(realloc.contains(" noalias noundef ptr "), "{realloc}");
    assert!(realloc.contains("ptr noundef allocptr"), "{realloc}");
    // Its attributes and its groups'.
    let attrs = |line: &str| {
        let groups = (line.split_whitespace())
            .filter(|w| w.starts_with('#'))
            .map(|w| group(w).to_string());
        groups.fold(line.to_string(), |all, g| all + "\n" + &g)
    };
    let alloc_groups = attrs(alloc);
    assert!(alloc_groups.contains("noinline"), "{alloc_groups}");
    assert!(alloc_groups.contains("allockind(\"alloc,uninitialized,aligned\")"));
    assert!(alloc_groups.contains("allocsize(0)"));
    assert!(alloc_groups.contains("\"alloc-family\"=\"velt_rt_alloc\""));
    let realloc_groups = attrs(realloc);
    assert!(realloc_groups.contains("noinline"), "{realloc_groups}");
    assert!(realloc_groups.contains("allockind(\"realloc,aligned\")"));
    assert!(realloc_groups.contains("allocsize(3)"));
    assert!(realloc_groups.contains("\"alloc-family\"=\"velt_rt_alloc\""));
}

const COUNTED: &str = r#"
class Point {
  constructor(public x: number, public y: number) {}
}

function main() {
  let sum = 0;
  const seen = new Map<string, number>();
  for (let i = 0; i < 1000; i++) {
    const p = new Point(i, i + 1);
    const parts = ("a" + i + ",b,c,d").split(",");
    const add = (n: number) => n + p.x + p.y;
    seen.set(parts[0], add(parts.length));
    const bytes = new TextEncoder().encode(parts[0]);
    const parsed = JSON.parse("[" + i + ", 2]") as number[];
    sum += bytes.length + parsed[0];
  }
  console.log(seen.size, sum);
}
"#;

/// A release program built as usual, then run with the debug runtime's checks and counts asked
/// for (`bench/rc_stats.ps1`, `bench/structs/run.sh`): with a debug runtime linked, it runs and
/// counts every object, as before the hooks; with a release one, whose allocator neither checks
/// nor counts, it runs.
#[test]
fn a_release_build_on_a_debug_runtime_runs_with_its_checks_and_counts() {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("counted.velt"), COUNTED).unwrap();
    let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let exe = dir
        .path()
        .join(format!("counted{}", std::env::consts::EXE_SUFFIX));
    let built = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "--release", "counted.velt", "-o"])
        .arg(&exe)
        .env("VELT_STD", &std)
        .env("VELT_BUILD_CACHE", "off")
        .env_remove("VELT_HEAP_HOOKS")
        .env_remove("VELT_RC_STATS")
        .env_remove("VELT_RT_DEBUG_ALLOC")
        .current_dir(dir.path())
        .output()
        .unwrap();
    text("build", &built);
    let ran = no_window::command(&exe)
        .env("VELT_RC_STATS", "1")
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(text("run", &ran), "1000 503390\n");
    if !cfg!(debug_assertions) {
        return;
    }
    let stderr = String::from_utf8_lossy(&ran.stderr);
    assert!(!stderr.contains("debug-alloc"), "{stderr}");
    let blocks = stderr
        .lines()
        .rev()
        .find(|l| l.starts_with("rc stats:"))
        .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("blocks=")))
        .and_then(|b| b.split_once('/'))
        .and_then(|(a, f)| Some((a.parse::<u64>().ok()?, f.parse::<u64>().ok()?)));
    let Some((allocated, freed)) = blocks else {
        panic!("no `rc stats` blocks report: {stderr}");
    };
    // At least one block per Point, closure and array made in the loop, each freed.
    assert!(allocated >= 3000, "{stderr}");
    assert_eq!(allocated, freed, "{stderr}");
}

/// A program whose release build links std objects that allocate: the std cache's `@noInline`
/// functions (`split`, `JSON.parse`, the set's growth, ...) and prebuilt hook modules (string
/// case, trimming, replacing).
const STD_OBJECTS: &str = r#"
function main() {
  const parts = ("a,b,c," + Date.now()).split(",");
  const s = parts.join("-").replaceAll("-", "+").toUpperCase().padStart(30, "*");
  const v = JSON.parse("[1,2,3]") as number[];
  const arr = [5, 3, 9, 1];
  arr.sort((a, b) => a - b);
  const t = new TextEncoder().encode(s);
  const set = new Set<string>(parts);
  const doubled = [1, 2].map((x) => x * 2);
  console.log(s.length, v.length, arr[0], t.length, set.size, s.trim().length, doubled.length);
}
"#;

/// Per object the build cache at `cache` holds of `kind` (`stdfn`: the std cache's functions,
/// `stdhook`: prebuilt hook modules): whether it calls the runtime's allocator, and whether
/// mimalloc's.
fn allocators_of(cache: &Path, kind: &str) -> Vec<(bool, bool)> {
    use object::{Object, ObjectSymbol};
    let mut files = vec![];
    let mut dirs = vec![cache.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = e.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.components().any(|c| c.as_os_str() == kind) {
                files.push(path);
            }
        }
    }
    let mut out = vec![];
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        let Ok(file) = object::File::parse(&*bytes) else {
            continue;
        };
        let undefined: Vec<String> = (file.symbols())
            .filter(|s| s.is_undefined())
            .filter_map(|s| s.name().ok())
            .map(|n| n.strip_prefix('_').unwrap_or(n).to_string())
            .collect();
        let calls = |names: &[&str]| undefined.iter().any(|u| names.contains(&u.as_str()));
        let runtime = calls(&["velt_rt_alloc", "velt_rt_realloc", "velt_rt_free"]);
        let mimalloc = calls(&["mi_malloc", "mi_malloc_aligned", "mi_realloc", "mi_free"]);
        out.push((runtime, mimalloc));
    }
    out
}

/// `velt run --release` of [`STD_OBJECTS`] storing the std objects it links in the build cache
/// `dir/<cache>` (`VELT_STD_OBJECT_FILL`), with `hooks` for `VELT_HEAP_HOOKS`.
fn fill_std_objects(dir: &test_dir::TestDir, cache: &str, hooks: &str) {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    std::fs::write(dir.path().join("std_objects.velt"), STD_OBJECTS).unwrap();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["run", "--release", "std_objects.velt"])
        .env(
            "VELT_STD",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        )
        .env("VELT_BUILD_CACHE", dir.path().join(cache))
        .env("VELT_STD_OBJECT_FILL", "1")
        .env("VELT_HEAP_HOOKS", hooks)
        .env("VELT_BUILD_STAMP", "off")
        .env_remove("VELT_STD_OBJECT")
        .env_remove("VELT_PREBUILT_HOOKS")
        .env_remove("VELT_RC_STATS")
        .env_remove("VELT_RT_DEBUG_ALLOC")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let expected = "30 3 1 30 4 30 2\n";
    assert_eq!(text("run", &out), expected);
}

#[test]
fn std_objects_allocate_as_the_build_does() {
    let dir = test_dir::TestDir::new();
    // With the hooks, nothing std compiled ahead calls the runtime's allocator: it would
    // bypass mimalloc in a release binary.
    fill_std_objects(&dir, "hooks", "1");
    for kind in ["stdfn", "stdhook"] {
        let objects = allocators_of(&dir.path().join("hooks"), kind);
        assert!(!objects.is_empty(), "no {kind} objects stored");
        assert!(objects.iter().all(|&(rt, _)| !rt), "{kind}: {objects:?}");
        assert!(objects.iter().any(|&(_, mi)| mi), "{kind}: {objects:?}");
    }
    // Without them, nothing calls mimalloc directly: a debug runtime checks and counts every
    // block (driver/heap_hooks.rs). The objects of a build with hooks are another key, so a
    // build without them in the same cache neither links them nor finds its own.
    fill_std_objects(&dir, "hooks", "0");
    for kind in ["stdfn", "stdhook"] {
        let objects = allocators_of(&dir.path().join("hooks"), kind);
        let runtime = objects.iter().filter(|&&(rt, mi)| rt && !mi).count();
        assert!(runtime > 0, "{kind}: {objects:?}");
        assert!(
            objects.iter().all(|&(rt, mi)| !(rt && mi)),
            "{kind}: {objects:?}"
        );
    }
    fill_std_objects(&dir, "runtime", "0");
    for kind in ["stdfn", "stdhook"] {
        let objects = allocators_of(&dir.path().join("runtime"), kind);
        assert!(objects.iter().all(|&(_, mi)| !mi), "{kind}: {objects:?}");
        assert!(objects.iter().any(|&(rt, _)| rt), "{kind}: {objects:?}");
    }
}
