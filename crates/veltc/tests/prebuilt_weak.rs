//! The weak core as a prebuilt hook module (velt_common `std_hooks/prebuilt.rs`, "State"): its
//! hooks are lowered (`WeakMap`, `WeakSet`, `WeakRef` and a weakly held object's release call
//! them), and its module keeps state of its own (the process's header, a static atomic, and
//! three thread-local values). Where the build cache holds its object, a program calls the
//! object's entry points and loads none of the core, the program defining the object's
//! thread-local values; where it doesn't, the program loads the core. Same output either way, in
//! debug and release builds, with tasks on other threads. A program that loads one of the core's
//! modules itself (the core's white-box tests) calls no object: the state would be in two copies.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod runtime_support;
mod test_dir;

/// An ephemeron cache whose values' closures capture their keys, a plain `WeakMap`, a `WeakSet`
/// and `WeakRef`s, on tasks that run on other threads.
const PROGRAM: &str = r#"class Node {
  n: number;
  constructor(n: number) {
    this.n = n;
  }
}

class Handler {
  f: () => number;
  constructor(key: Node) {
    this.f = () => key.n;
  }
}

async function work(seed: number): Promise<number> {
  const cache = new WeakMap<Node, Handler>();
  const plain = new WeakMap<Node, number>();
  const seen = new WeakSet<Node>();
  let total = 0;
  for (let round = 0; round < 20; round++) {
    const keep: Node[] = [];
    const refs: WeakRef<Node>[] = [];
    for (let i = 0; i < 10; i++) {
      const k = new Node(seed + i);
      cache.set(k, new Handler(k));
      plain.set(k, i);
      seen.add(k);
      refs.push(new WeakRef(k));
      if (i % 3 == 0) keep.push(k);
    }
    await Promise.resolve(0);
    for (const k of keep) {
      total += cache.get(k)?.f() ?? 0;
      total += plain.get(k) ?? 0;
      if (seen.has(k)) total += 1;
    }
    // Only a kept key's: whether the others are still there depends on whether the trial
    // sees through the handlers' closures, which isn't this test's.
    for (let i = 0; i < refs.length; i += 3) {
      if (refs[i].deref() !== undefined) total += 1;
    }
  }
  return total;
}

async function main() {
  const tasks: Promise<number>[] = [];
  for (let t = 0; t < 16; t++) tasks.push(spawn(work(t)));
  let sum = 0;
  for (const p of tasks) sum += await p;
  const k = new Node(7);
  const r = new WeakRef(k);
  console.log(sum, r.deref()?.n ?? -1);
}
"#;

/// Each key a round keeps (4 of 10) reads its handler (`seed + i`), its number (`i`), the set and
/// its `WeakRef`: `4 * seed + 44` a round, 20 rounds of 16 tasks.
const OUT: &str = "23680 7\n";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `velt <args> <file>` in `dir`, whose `src` is written to `file`, with the repository's std,
/// the build cache `dir/cache` and the stats of the std cache; `env` adds variables.
fn velt(dir: &Path, file: &str, src: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
    runtime_support::build_native_runtime(&repo());
    std::fs::write(dir.join(file), src).unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(args)
        .arg(file)
        .current_dir(dir)
        .env("VELT_STD", repo().join("std"))
        .env("VELT_BUILD_CACHE", dir.join("cache"))
        .env("VELT_STD_OBJECT_STATS", "1")
        .env("VELT_THREADS", "4")
        // A build the stamp finds up to date checks nothing: every build here compiles.
        .env("VELT_BUILD_STAMP", "off")
        .env_remove("VELT_STD_OBJECT_FILL")
        .env_remove("VELT_PREBUILT_HOOKS")
        .env_remove("VELT_RT_LINK");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    out
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// What the std cache's stats say of the weak core's module.
fn found(o: &Output) -> &'static str {
    let err = text(&o.stderr);
    if err.contains("lang/hooks/weak: object found") {
        "object"
    } else if err.contains("lang/hooks/weak: no object") {
        "module"
    } else {
        panic!("no stats of the weak core in\n{err}")
    }
}

/// The std lines a `-v` build says it loaded.
fn std_lines(o: &Output) -> usize {
    let err = text(&o.stderr);
    let line = (err.lines())
        .find(|l| l.ends_with("std lines loaded"))
        .unwrap_or_else(|| panic!("no std line count in\n{err}"));
    let n = line.trim_start_matches("velt:").trim().split(' ').next();
    n.and_then(|n| n.parse().ok()).unwrap()
}

const FILL: (&str, &str) = ("VELT_PREBUILT_HOOKS", "fill");

/// Without the object the core is loaded (and, filling, the object stored); with it the program
/// calls the object and loads none of the core. Same output, in debug and release builds.
#[test]
fn a_weak_program_calls_the_core_s_object() {
    // Release builds for the default CPU link what debug builds do: a cache each.
    for mode in [&["run", "--release", "-v"][..], &["run", "-v"]] {
        let tmp = test_dir::TestDir::new();
        let dir = tmp.path();
        let off = velt(
            dir,
            "a.velt",
            PROGRAM,
            mode,
            &[("VELT_PREBUILT_HOOKS", "0")],
        );
        assert_eq!(text(&off.stdout), OUT);
        let filled = velt(dir, "a.velt", PROGRAM, mode, &[FILL]);
        assert_eq!(
            (found(&filled), text(&filled.stdout).as_str()),
            ("module", OUT)
        );
        let served = velt(dir, "a.velt", PROGRAM, mode, &[]);
        assert_eq!(
            (found(&served), text(&served.stdout).as_str()),
            ("object", OUT),
            "{mode:?}"
        );
        // The core's, and that of the blocks of module state (module_blocks.velt), which a
        // program on the executor reaches.
        assert!(text(&served.stderr).contains("prebuilt hooks: 2 objects linked"));
        // The core is about 2,000 lines of std, and the program loads none of them.
        assert!(
            std_lines(&served) + 1500 < std_lines(&off),
            "{} std lines with the object, {} without",
            std_lines(&served),
            std_lines(&off)
        );
    }
}

/// The core's white-box tests import its modules: their program calls no object, which would keep
/// a second copy of the core's state, and they pass with the object in the cache.
#[test]
fn a_program_loading_the_core_calls_no_object() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let run: &[&str] = &["run"];
    velt(dir, "a.velt", PROGRAM, run, &[FILL]);
    let served = velt(dir, "a.velt", PROGRAM, run, &[]);
    assert_eq!(found(&served), "object");
    let test = repo().join("std/lang/hooks/weak_ops.test.velt");
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    let out = cmd
        .arg("test")
        .arg(&test)
        .current_dir(dir)
        .env("VELT_STD", repo().join("std"))
        .env("VELT_BUILD_CACHE", dir.join("cache"))
        .env("VELT_STD_OBJECT_STATS", "1")
        .env("VELT_BUILD_STAMP", "off")
        .env_remove("VELT_STD_OBJECT_FILL")
        .env_remove("VELT_PREBUILT_HOOKS")
        .env_remove("VELT_RT_LINK")
        .output()
        .unwrap();
    let err = text(&out.stderr);
    assert!(out.status.success(), "{}{err}", text(&out.stdout));
    // It still links the object of the blocks of module state (module_blocks.velt).
    assert!(!err.contains("lang/hooks/weak: object found"), "{err}");
    assert!(err.contains("prebuilt hooks: 1 objects linked"), "{err}");
}
