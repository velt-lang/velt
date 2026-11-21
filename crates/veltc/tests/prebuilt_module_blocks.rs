//! The blocks of module state as a prebuilt hook module (std/lang/hooks/module_blocks.velt, #2196
//! 9c; velt_common `std_hooks/prebuilt.rs`, "State"): its hooks are lowered (every access of a
//! module-level `let` in a program without the executor) and called by role from the executor's
//! module lock (sched_module.velt), and its module keeps the blocks' table as state of its own.
//! Where the build cache holds its object, a native program with module state calls the object's
//! entry points and loads none of the module; where it doesn't, the program loads the module.
//! Same output either way, in debug and release builds, without the executor and on its workers,
//! and a counted normal end gives back every block. A program without module state links no
//! object.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod runtime_support;
mod test_dir;

/// Module state without the executor: numbers, strings held in a module array and a map, written
/// in loops (one lookup per function call, #1571) and read back.
const SYNC: &str = r#"let counter = 0;
const names: string[] = [];
const byName = new Map<string, number>();
let last = "none";

function bump(i: number): void {
  counter += i;
  const name = `n${i}`;
  names.push(name);
  byName.set(name, counter);
  last = name;
}

function total(): number {
  let sum = 0;
  for (const n of names) sum += byName.get(n) ?? 0;
  return sum;
}

function main() {
  for (let i = 0; i < 200; i++) bump(i);
  console.log(counter, names.length, total(), last);
}
"#;

/// `counter` is 0 + 1 + … + 199; `total` the sum of its 200 partial sums.
const SYNC_OUT: &str = "19900 200 1333300 n199\n";

/// Module state on the executor's workers: every task takes the module lock for its poll
/// (sched_module.velt), which reaches the same blocks by role.
const TASKS: &str = r#"import { workerCount } from "velt/task";

let counter = 0;
const names: string[] = [];

async function bump(w: i64): Promise<void> {
  for (let i = 0; i < 100; i++) {
    const was = counter;
    counter = was + 1;
    names.push(`w${w}-${i}`);
    await yieldNow();
  }
}

async function main() {
  const tasks: Promise<void>[] = [];
  for (let w: i64 = 0; w < workerCount(); w++) {
    tasks.push(spawn(bump(w), { worker: w }));
  }
  for (const t of tasks) await t;
  console.log(counter, names.length);
}
"#;

const TASKS_OUT: &str = "400 400\n";

const HELLO: &str = "function main() {\n  console.log(\"hello\");\n}\n";

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

/// What the std cache's stats say of the blocks' module.
fn found(o: &Output) -> &'static str {
    let err = text(&o.stderr);
    if err.contains("lang/hooks/module_blocks: object found") {
        "object"
    } else if err.contains("lang/hooks/module_blocks: no object") {
        "module"
    } else {
        panic!("no stats of the blocks' module in\n{err}")
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
const OFF: (&str, &str) = ("VELT_PREBUILT_HOOKS", "0");

/// Without the object the module is loaded (and, filling, the object stored); with it the program
/// calls the object and loads none of the module. Same output, in debug and release builds, for
/// a program without the executor and for tasks on its workers.
#[test]
fn a_program_with_module_state_calls_the_blocks_object() {
    for (src, expected) in [(SYNC, SYNC_OUT), (TASKS, TASKS_OUT)] {
        // Release builds for the default CPU link what debug builds do: a cache each.
        for mode in [&["run", "--release", "-v"][..], &["run", "-v"]] {
            let tmp = test_dir::TestDir::new();
            let dir = tmp.path();
            let off = velt(dir, "a.velt", src, mode, &[OFF]);
            assert_eq!(text(&off.stdout), expected, "{mode:?}");
            let filled = velt(dir, "a.velt", src, mode, &[FILL]);
            assert_eq!(
                (found(&filled), text(&filled.stdout).as_str()),
                ("module", expected),
                "{mode:?}"
            );
            let served = velt(dir, "a.velt", src, mode, &[]);
            assert_eq!(
                (found(&served), text(&served.stdout).as_str()),
                ("object", expected),
                "{mode:?}"
            );
            // The blocks' object, and those of other prebuilt modules the program calls (the
            // string methods, std/lang/string/).
            assert!(
                text(&served.stderr).contains("objects linked"),
                "{mode:?}\n{}",
                text(&served.stderr)
            );
            // The module is about 370 lines of std, and the program loads none of them.
            assert!(
                std_lines(&served) + 300 < std_lines(&off),
                "{mode:?}: {} std lines with the object, {} without",
                std_lines(&served),
                std_lines(&off)
            );
        }
    }
}

/// A counted normal end gives back every block of module state through the object too
/// (`velt_hook_module_exit` without the executor, `velt_hook_module_drop` at the executor's end),
/// as the loaded module does.
#[test]
fn the_object_s_blocks_are_freed_at_a_counted_normal_exit() {
    let counted = [("VELT_RT_DEBUG_ALLOC", "1"), ("VELT_RC_STATS", "1")];
    for (src, expected) in [(SYNC, SYNC_OUT), (TASKS, TASKS_OUT)] {
        let tmp = test_dir::TestDir::new();
        let dir = tmp.path();
        velt(dir, "a.velt", src, &["run"], &[FILL]);
        let served = velt(dir, "a.velt", src, &["run"], &counted);
        assert_eq!(found(&served), "object");
        let all = format!("{}{}", text(&served.stdout), text(&served.stderr));
        let blocks = all
            .split_whitespace()
            .find_map(|w| w.strip_prefix("blocks="))
            .and_then(|b| b.split_once('/'));
        assert!(
            text(&served.stdout).starts_with(expected) && blocks.is_some_and(|(a, f)| a == f),
            "{all}"
        );
    }
}

/// A program without module state calls none of the hooks: it links no object, with the object
/// in the cache.
#[test]
fn a_program_without_module_state_links_no_object() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    velt(dir, "a.velt", SYNC, &["run"], &[FILL]);
    assert_eq!(found(&velt(dir, "a.velt", SYNC, &["run"], &[])), "object");
    let hello = velt(dir, "hello.velt", HELLO, &["run"], &[]);
    assert_eq!(text(&hello.stdout), "hello\n");
    assert!(
        !text(&hello.stderr).contains("objects linked"),
        "{}",
        text(&hello.stderr)
    );
}
