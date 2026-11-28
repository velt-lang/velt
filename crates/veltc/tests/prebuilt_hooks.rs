//! Prebuilt hook modules (velt_common `std_hooks/prebuilt.rs`, veltc `std_cache/prebuilt.rs`): a
//! `hookCall` of a `prebuilt` hook calls its module's object where the build cache holds it, and
//! loads the module where it doesn't, with the same output. The test hooks
//! (`std/lang/hooks/prebuilt_probe.velt`) are called by role from a module of a std copied for
//! the test (`velt/zz_prebuilt`), as no program can name them.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod runtime_support;
mod test_dir;
#[allow(dead_code)]
mod wasm_tools;

/// The module the programs import: the test hooks by role.
const PROBE: &str = r#"import { hookCall } from "velt/lang/intrinsics";
import { STR_EPOCH } from "velt/lang/hooks/process_statics";

/** `probeText` by role. */
export function digits(x: f64, n: i64): string {
  return hookCall("prebuilt_probe_text", x, n);
}

/** `probeValue` by role. */
export function value(s: string, start: usize, end: usize): f64 {
  return hookCall("prebuilt_probe_value", s, start, end, false);
}

/** `probeValue` by role, negated: a `bool` crosses. */
export function negated(s: string, start: usize, end: usize): f64 {
  return hookCall("prebuilt_probe_value", s, start, end, true);
}

/** `probeEpoch` by role: `STR_EPOCH` as the hook reads it. */
export function epoch(): u64 {
  return hookCall("prebuilt_probe_epoch");
}

/** `STR_EPOCH` as the program reads it. */
export function ownEpoch(): u64 {
  return STR_EPOCH.load();
}

/** Bump `STR_EPOCH` by `n`, as the runtime does. */
export function bump(n: u64): void {
  STR_EPOCH.add(n);
}
"#;

/// A program calling both hooks, the second with a `bool` either way.
const FIRST: &str = r#"import { digits, negated, value } from "velt/zz_prebuilt";

function main() {
  console.log(digits(9876543.5, 4), digits(-42, 9), digits(0, 1));
  const s = "ab12c3d4" + "5";
  console.log(value(s, 2, s.length as usize), value(s, 0, 3), negated(s, 0, 4));
}
"#;
const FIRST_OUT: &str = "9876 -42 0\n12345 1 -12\n";

/// Another program: its own `u8[]`s, classes held in arrays and a sorted array of objects
/// holding strings (whole-program facts that change how `u8[]` and `string` parameters are
/// compiled), then the same hooks.
const SECOND: &str = r#"import { digits, value } from "velt/zz_prebuilt";

class Row {
  name: string;
  bytes: u8[];
  constructor(name: string, bytes: u8[]) {
    this.name = name;
    this.bytes = bytes;
  }
}

function main() {
  const rows: Row[] = [];
  for (let i = 0; i < 5; i++) {
    const b: u8[] = [];
    b.push(i as u8);
    rows.push(new Row(`r${5 - i}`, b));
  }
  rows.sort((a, b) => a.name.localeCompare(b.name));
  let text = "";
  for (const r of rows) {
    text += r.name + r.bytes.length;
  }
  console.log(text, digits(123456, 3), value(text, 0, text.length as usize));
}
"#;
const SECOND_OUT: &str = "r11r21r31r41r51 123 1121314151\n";

/// A program bumping std's process static `STR_EPOCH` and reading it through the hook: the
/// object reads the program's word, which the runtime bumps (`prebuilt.rs` "State").
const EPOCH: &str = r#"import { bump, epoch, ownEpoch } from "velt/zz_prebuilt";

function main() {
  const before = epoch();
  bump(3);
  const after = epoch();
  console.log(after - before, after == ownEpoch());
  bump(1);
  console.log(epoch() - after, epoch() == ownEpoch());
}
"#;
const EPOCH_OUT: &str = "3 true\n1 true\n";

/// A std with `velt/zz_prebuilt` added, in `dir` (made once).
fn std_in(dir: &Path) -> PathBuf {
    let std = dir.join("std");
    if !std.exists() {
        wasm_tools::copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
            &std,
        );
        std::fs::create_dir_all(std.join("zz_prebuilt")).unwrap();
        std::fs::write(std.join("zz_prebuilt/index.velt"), PROBE).unwrap();
    }
    std
}

/// `velt <args> <file>` in `dir`, whose `src` is written to `file`, with the copied std, the build
/// cache `dir/cache` and the stats of the std cache; `env` adds variables.
fn velt(dir: &Path, file: &str, src: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    std::fs::write(dir.join(file), src).unwrap();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(args)
        .arg(file)
        .current_dir(dir)
        .env("VELT_STD", std_in(dir))
        .env("VELT_BUILD_CACHE", dir.join("cache"))
        .env("VELT_STD_OBJECT_STATS", "1")
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

/// What the std cache's stats say of the test hooks' module.
fn found(o: &Output) -> &'static str {
    let err = text(&o.stderr);
    if err.contains("prebuilt_probe: object found") {
        "object"
    } else if err.contains("prebuilt_probe: no object") {
        "module"
    } else {
        panic!("no stats of the hooks in\n{err}")
    }
}

const FILL: (&str, &str) = ("VELT_PREBUILT_HOOKS", "fill");

/// Without the object the module is loaded (and, filling, the object stored); with it the program
/// calls the object. Same output, in debug and release builds, and `velt check` takes it too.
#[test]
fn a_prebuilt_hook_calls_its_object_and_loads_its_module_without_one() {
    // Release builds for the default CPU link what debug builds do: a cache each.
    for mode in [&["run", "--release"][..], &["run"]] {
        let tmp = test_dir::TestDir::new();
        let dir = tmp.path();
        let off = velt(dir, "a.velt", FIRST, mode, &[("VELT_PREBUILT_HOOKS", "0")]);
        assert_eq!(text(&off.stdout), FIRST_OUT);
        assert!(!text(&off.stderr).contains("prebuilt_probe"));
        let filled = velt(dir, "a.velt", FIRST, mode, &[FILL]);
        assert_eq!(
            (found(&filled), text(&filled.stdout).as_str()),
            ("module", FIRST_OUT)
        );
        let served = velt(dir, "a.velt", FIRST, mode, &[]);
        assert_eq!(
            (found(&served), text(&served.stdout).as_str()),
            ("object", FIRST_OUT)
        );
        // The probe's object, and std's float printer's (`number_shortest`): the program writes
        // the f64s `value` returns.
        let err = text(&served.stderr);
        assert!(err.contains("number_shortest: object found"), "{err}");
        assert!(err.contains("prebuilt hooks: 2 objects linked"), "{err}");
        if mode == ["run"] {
            let check = velt(dir, "a.velt", FIRST, &["check"], &[]);
            assert_eq!(found(&check), "object");
        }
    }
}

/// A program with an `RwLock` that writes a byte only inside a `read` callback, and prints an
/// array through the objects: the objects call the writer's hooks by their plain symbols, which
/// such a program lowers only in their checked and read versions of its own.
const LOCKED: &str = r#"function main() {
  const rw = new RwLock<number[]>([1, 2]);
  const seen: number[][] = [];
  rw.read((c) => {
    seen.push(structuredClone(c));
  });
  console.log(seen);
  rw.read((c) => console.log(c.length));
}
"#;

/// A program with a lock set links the objects: the writer's call-outs are defined and exported
/// in their plain versions, in debug and release builds.
#[test]
fn a_program_with_a_lock_set_links_the_objects() {
    for mode in [&["run", "--release"][..], &["run"]] {
        let tmp = test_dir::TestDir::new();
        let dir = tmp.path();
        let filled = velt(dir, "l.velt", LOCKED, mode, &[FILL]);
        assert_eq!(text(&filled.stdout), "[ [ 1, 2 ] ]\n2\n");
        let served = velt(dir, "l.velt", LOCKED, mode, &[]);
        let err = text(&served.stderr);
        assert!(err.contains(": object found"), "{mode:?}: {err}");
        assert_eq!(text(&served.stdout), "[ [ 1, 2 ] ]\n2\n");
    }
}

/// The object's key holds no program facts: the object stored for one program serves another
/// whose own code changes how `u8[]` and `string` are compiled.
#[test]
fn an_object_built_for_one_program_serves_another() {
    let dir = test_dir::TestDir::new();
    let dir = dir.path();
    let filled = velt(dir, "a.velt", FIRST, &["run", "--release"], &[FILL]);
    assert_eq!(found(&filled), "module");
    let other = velt(dir, "b.velt", SECOND, &["run", "--release"], &[]);
    assert_eq!(
        (found(&other), text(&other.stdout).as_str()),
        ("object", SECOND_OUT)
    );
    // The module as a program loads it gives the same.
    let loaded = velt(
        dir,
        "b.velt",
        SECOND,
        &["run", "--release"],
        &[("VELT_PREBUILT_HOOKS", "0")],
    );
    assert_eq!(text(&loaded.stdout), SECOND_OUT);
}

/// A changed std file the object was compiled from makes it stale: the build loads the module
/// (and its result follows the change).
#[test]
fn an_edited_std_file_retires_the_object() {
    let dir = test_dir::TestDir::new();
    let dir = dir.path();
    let run: &[&str] = &["run"];
    velt(dir, "a.velt", FIRST, run, &[FILL]);
    // With the build stamp: the std files the object came from are its inputs too.
    let on = ("VELT_BUILD_STAMP", "on");
    let served = velt(dir, "a.velt", FIRST, run, &[on]);
    assert_eq!(found(&served), "object");
    let hooks = std_in(dir).join("lang/hooks/prebuilt_probe.velt");
    let src = std::fs::read_to_string(&hooks).unwrap();
    // Another time, the same contents: the same object.
    let file = std::fs::File::options().write(true).open(&hooks).unwrap();
    let time = file.metadata().unwrap().modified().unwrap();
    file.set_modified(time + std::time::Duration::from_secs(5))
        .unwrap();
    drop(file);
    let touched = velt(dir, "a.velt", FIRST, run, &[]);
    assert_eq!(found(&touched), "object");
    let edited = src.replace(
        "let text = x < 0 ? \"-\" : \"\";",
        "let text = x < 0 ? \"~\" : \"\";",
    );
    assert_ne!(src, edited);
    std::fs::write(&hooks, edited).unwrap();
    let stale = velt(dir, "a.velt", FIRST, run, &[on]);
    assert_eq!(found(&stale), "module");
    assert_eq!(text(&stale.stdout), FIRST_OUT.replace("-42", "~42"));
}

/// `velt cache warm` stores the object of every prebuilt hook module, also one no warm-up
/// program calls: a first build of a program calling the hooks finds it, in debug and release,
/// for the default CPU and `--cpu native`.
#[test]
fn cache_warm_stores_every_prebuilt_module() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let share = dir.join("share");
    std::fs::create_dir_all(share.join("warm")).unwrap();
    std::fs::write(
        share.join("warm/hello.velt"),
        "function main() {\n  console.log(\"hi\");\n}\n",
    )
    .unwrap();
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let warm = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["cache", "warm"])
        .current_dir(dir)
        .env("VELT_STD", std_in(dir))
        .env("VELT_SHARE", &share)
        .env("VELT_BUILD_CACHE", dir.join("cache"))
        .env_remove("VELT_STD_OBJECT_FILL")
        .env_remove("VELT_PREBUILT_HOOKS")
        .env_remove("VELT_RT_LINK")
        .output()
        .unwrap();
    assert!(warm.status.success(), "{}", text(&warm.stderr));
    for mode in [
        &["run"][..],
        &["run", "--release"],
        &["run", "--release", "--cpu", "native"],
    ] {
        let served = velt(dir, "a.velt", FIRST, mode, &[]);
        assert_eq!(
            (found(&served), text(&served.stdout).as_str()),
            ("object", FIRST_OUT),
            "{mode:?}"
        );
    }
}

/// An object reading a process static imports the program's word: it sees each bump, in debug and
/// release builds, where a copy of its own would stay at 0.
#[test]
fn an_object_reads_the_programs_process_static() {
    for mode in [&["run", "--release"][..], &["run"]] {
        let tmp = test_dir::TestDir::new();
        let dir = tmp.path();
        let filled = velt(dir, "e.velt", EPOCH, mode, &[FILL]);
        assert_eq!(
            (found(&filled), text(&filled.stdout).as_str()),
            ("module", EPOCH_OUT)
        );
        let served = velt(dir, "e.velt", EPOCH, mode, &[]);
        assert_eq!(
            (found(&served), text(&served.stdout).as_str()),
            ("object", EPOCH_OUT),
            "{mode:?}"
        );
    }
}

/// A program reading `charCodeAt` of non-ASCII heap strings, each freed before the next is built
/// (often at the same address): the translation's cache (std/lang/string/positions.velt, the
/// object of `lang_string_positions`) keeps an entry only under the program's `STR_EPOCH`, which
/// the runtime bumps when such a buffer is freed. Node 22 prints the same.
const CODE_UNITS: &str = r#"function built(piece: string, n: i64): string {
  let s = "";
  for (let i: i64 = 0; i < n; i++) {
    s += piece;
  }
  return s;
}

function readAt(piece: string, n: i64, at: i64): string {
  const s = built(piece, n);
  return `${s.charCodeAt(at)} ${s.charCodeAt(at + 1)} ${s.charCodeAt(at - 1)}`;
}

function main() {
  const out: string[] = [];
  for (let round: i64 = 0; round < 2; round++) {
    out.push(readAt("é日😀a", 100, 250));
    out.push(readAt("😀é日a", 100, 250));
    out.push(readAt("a😀日é", 100, 251));
  }
  console.log(out.join("|"));
}
"#;
const CODE_UNITS_OUT: &str =
    "233 26085 97|55357 56832 97|55357 56832 97|233 26085 97|55357 56832 97|55357 56832 97\n";

/// `charCodeAt` of non-ASCII text calls the object of positions.velt where the cache holds it,
/// with the output of the module loaded, in debug and release builds.
#[test]
fn char_code_at_calls_the_positions_object() {
    for mode in [&["run", "--release"][..], &["run"]] {
        let tmp = test_dir::TestDir::new();
        let dir = tmp.path();
        let filled = velt(dir, "c.velt", CODE_UNITS, mode, &[FILL]);
        assert_eq!(text(&filled.stdout), CODE_UNITS_OUT, "{mode:?}");
        let served = velt(dir, "c.velt", CODE_UNITS, mode, &[]);
        let err = text(&served.stderr);
        assert!(
            err.contains("std/lang/string/positions: object found"),
            "{err}"
        );
        assert_eq!(text(&served.stdout), CODE_UNITS_OUT, "{mode:?}");
    }
}

/// The same kind of program cutting its strings with `slice`, `charAt` and `substring` (#2816):
/// the object's `sliceWide` translates both ends through the same cache, and its cut of a pair's
/// half is a lone surrogate. Node 22 prints the same.
const SLICES: &str = r#"function built(piece: string, n: i64): string {
  let s = "";
  for (let i: i64 = 0; i < n; i++) {
    s += piece;
  }
  return s;
}

function codes(s: string): string {
  const out: string[] = [];
  for (let i = 0; i < s.length; i++) {
    out.push(`${s.charCodeAt(i)}`);
  }
  return out.join(" ");
}

function cut(piece: string, n: i64, at: i64): string {
  const s = built(piece, n);
  return `${codes(s.slice(at, at + 3))} ${codes(s.charAt(at + 1))} ${s.substring(at, 0).length}`;
}

function main() {
  const out: string[] = [];
  for (let round: i64 = 0; round < 2; round++) {
    out.push(cut("é日😀a", 100, 250));
    out.push(cut("😀é日a", 100, 250));
    out.push(cut("a😀日é", 100, 251));
  }
  console.log(out.join("|"));
}
"#;
const SLICES_OUT: &str = "233 26085 55357 26085 250|55357 56832 233 56832 250|55357 56832 26085 56832 251|\
                          233 26085 55357 26085 250|55357 56832 233 56832 250|55357 56832 26085 56832 251\n";

/// `slice` of non-ASCII text calls the object of positions.velt where the cache holds it, with
/// the output of the module loaded, in debug and release builds.
#[test]
fn slice_calls_the_positions_object() {
    for mode in [&["run", "--release"][..], &["run"]] {
        let tmp = test_dir::TestDir::new();
        let dir = tmp.path();
        let filled = velt(dir, "s.velt", SLICES, mode, &[FILL]);
        assert_eq!(text(&filled.stdout), SLICES_OUT, "{mode:?}");
        let served = velt(dir, "s.velt", SLICES, mode, &[]);
        let err = text(&served.stderr);
        assert!(
            err.contains("std/lang/string/positions: object found"),
            "{err}"
        );
        assert_eq!(text(&served.stdout), SLICES_OUT, "{mode:?}");
    }
}

/// WebAssembly links no objects: the build loads the module, whatever the cache holds.
#[test]
fn webassembly_loads_the_module() {
    if wasm_tools::missing_tools("wasm32-wasip1", "wasmtime").is_some() {
        return;
    }
    wasm_tools::build_runtime(&wasm_tools::root(), "wasm32-wasip1");
    let dir = test_dir::TestDir::new();
    let dir = dir.path();
    velt(dir, "a.velt", FIRST, &["run", "--release"], &[FILL]);
    let wasm = velt(
        dir,
        "a.velt",
        FIRST,
        &["run", "--target", "wasm32-wasip1"],
        &[],
    );
    assert_eq!(text(&wasm.stdout), FIRST_OUT);
    assert!(!text(&wasm.stderr).contains("prebuilt_probe"));
}
