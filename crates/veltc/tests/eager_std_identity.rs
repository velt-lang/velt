//! Loading std modules during the check changes nothing but the cost (#2161 step 3,
//! docs/internals/design/lazy-std.md "Loading on demand"): every golden, built with `--emit vir`
//! with its std modules loaded on demand and with `VELT_EAGER_STD=1` (all loaded up front), gives
//! the same bytes, and a golden that does not build fails the same way. A difference is a source
//! rewrite an added module missed, or something in the VIR that depends on the order sema numbers
//! things in.
//!
//! Long: selected by `cargo xtask affected`, which runs it with `--ignored`.

mod no_window;
mod test_dir;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Compared, and reported apart from the rest: generated function names vary between runs (#2226).
const KNOWN: &[&str] = &["lang/union_erased_layout.velt"];

fn goldens(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if name != "errors" {
                goldens(&p, out);
            }
        } else if name.ends_with(".velt") && !name.starts_with('_') {
            out.push(p);
        }
    }
}

/// `--emit vir` of `file`: whether it built, the VIR, and what it reported.
fn vir(file: &Path, home: &Path, eager: bool) -> (bool, Vec<u8>, Vec<u8>) {
    let mut c = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    c.arg("build")
        .arg(file)
        .args(["--emit", "vir"])
        .env("VELT_HOME", home)
        .env("VELT_BUILD_CACHE", "off")
        .env("VELT_BUILD_STAMP", "off");
    match eager {
        true => c.env("VELT_EAGER_STD", "1"),
        false => c.env_remove("VELT_EAGER_STD"),
    };
    let out = c.output().unwrap();
    (out.status.success(), out.stdout, out.stderr)
}

/// The goldens, whose first different line per golden is reported; `built` counts those that build
/// both ways.
fn differs(file: &Path, home: &Path, built: &AtomicUsize) -> Option<String> {
    let (lazy_ok, lazy, lazy_err) = vir(file, home, false);
    let (eager_ok, eager, eager_err) = vir(file, home, true);
    if lazy_ok && eager_ok {
        built.fetch_add(1, Ordering::Relaxed);
    }
    if lazy_ok != eager_ok {
        let err = if lazy_ok { &eager_err } else { &lazy_err };
        let err: String = String::from_utf8_lossy(err).chars().take(400).collect();
        return Some(format!(
            "builds on demand: {lazy_ok}; up front: {eager_ok}: {err}"
        ));
    }
    let (a, b) = match lazy_ok {
        true => (lazy, eager),
        false => (lazy_err, eager_err),
    };
    if a == b {
        return None;
    }
    let (a, b) = (String::from_utf8_lossy(&a), String::from_utf8_lossy(&b));
    let found = a.lines().zip(b.lines()).find(|(x, y)| x != y);
    Some(match found {
        Some((x, y)) => format!("on demand: {x}\n  up front: {y}"),
        None => "one is longer".into(),
    })
}

#[test]
#[ignore = "builds every golden twice: `cargo xtask check` runs it when sema, the loader, lowering or std change"]
fn every_golden_has_the_same_vir_with_std_up_front_and_on_demand() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden");
    let mut files = vec![];
    goldens(&root, &mut files);
    files.sort();
    assert!(files.len() > 1000, "{} goldens", files.len());
    // Only part of them, for a look while working (`VELT_GOLDEN=m1/`).
    if let Some(f) = std::env::var_os("VELT_GOLDEN") {
        let f = f.to_string_lossy().into_owned();
        files.retain(|p| p.to_string_lossy().contains(&f));
    }
    let filtered = std::env::var_os("VELT_GOLDEN").is_some();
    let next = AtomicUsize::new(0);
    let built = AtomicUsize::new(0);
    let found = Mutex::new(vec![]);
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get());
    let home = test_dir::TestDir::new();
    std::thread::scope(|s| {
        for w in 0..workers {
            let (next, built, found, files, root) = (&next, &built, &found, &files, &root);
            let home = home.path().join(w.to_string());
            s.spawn(move || {
                while let Some(file) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if let Some(why) = differs(file, &home, built) {
                        let name = file.strip_prefix(root).unwrap_or(file);
                        let name = name.to_string_lossy().replace('\\', "/");
                        found.lock().unwrap().push((name, why));
                    }
                }
            });
        }
    });
    // Two identical compile failures count as a match: a broken setup must not pass that way.
    let built = built.into_inner();
    assert!(
        filtered || built > 1000,
        "only {built} of {} goldens built",
        files.len()
    );
    let mut found = found.into_inner().unwrap();
    found.sort();
    let (known, new): (Vec<_>, Vec<_>) = found
        .into_iter()
        .partition(|(name, _)| KNOWN.contains(&name.as_str()));
    for (name, why) in &known {
        eprintln!("known difference (#2226): {name}\n  {why}");
    }
    let listed: Vec<String> = new.iter().map(|(n, why)| format!("{n}\n  {why}")).collect();
    assert!(
        new.is_empty(),
        "{} of {} goldens differ:\n{}",
        new.len(),
        files.len(),
        listed.join("\n")
    );
}

/// A hook module with every kind of item the rule allows (#2161 §2: functions, constants, its own
/// structs, classes, interfaces, enums and aliases, an `extend` of its own type) and what the
/// source rewrites work on (an overload, a namespace, a generic arrow, a nested closure group, a
/// static method using `this`). No module of std holds all of them yet.
const RICH_ANY_RECORD: &str = r#"
const WEIGHTS = [1, 2, 3];

interface Named {
  name(): string;
}

namespace Space {
  export interface Pair {
    left: i64;
    right: i64;
  }
}

enum Kind {
  Small,
  Large,
}

struct Point {
  x: i64 = 1;
  y: i64 = 2;
}

type Spot = Point;

extend Point {
  sum(): i64 {
    return this.x + this.y;
  }
}

class Base implements Named {
  name(): string {
    return "base";
  }
  static tag(): string {
    return "base tag";
  }
  static label(): string {
    return this.tag();
  }
}

class Derived extends Base {
  override name(): string {
    return "derived";
  }
  static tag(): string {
    return "derived tag";
  }
}

function over(a: string): string;
function over(a: i64): string;
function over(a: any): string {
  return `${a}`;
}

const same = <T>(x: T): T => x;

function nested(): i64 {
  let seen = 0;
  function even(n: i64): bool {
    seen = seen + 1;
    return n == 0 || odd(n - 1);
  }
  function odd(n: i64): bool {
    seen = seen + 1;
    return n != 0 && even(n - 1);
  }
  return even(2) ? seen : 0;
}

type Tree = { kids: Tree[] };

interface Doubler {
  (x: i64): i64;
}


function touched(): string {
  const p: Spot = { x: 1, y: 2 };
  const named: Named[] = [new Base(), new Derived()];
  const pair: Space.Pair = { left: 1, right: 2 };
  const k = Kind.Large;
  const tree: Tree = { kids: [{ kids: [] }] };
  const twice: Doubler = (x: i64): i64 => x * 2;
  const local = <U>(x: U): U => x;
  let text = `${p.sum()} ${WEIGHTS[1]} ${same(pair.right)} ${k == Kind.Large} ${nested()} ${tree.kids.length} ${twice(2)} ${local(5)}`;
  for (const n of named) {
    text = text + " " + n.name();
  }
  return text + " " + Derived.label() + " " + over("s") + over(7);
}

export function anyToRecordAny(o: any): Record<string, any> {
  const r: Record<string, any> = {};
  r["touched"] = touched();
  for (const k of Object.keys(o)) {
    r[k] = o[k];
  }
  return r;
}

export function anyToRecord<V>(o: any): Record<string, V> {
  const r: Record<string, V> = {};
  for (const k of Object.keys(o)) {
    r[k] = o[k] as V;
  }
  return r;
}
"#;

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let (p, q) = (entry.path(), to.join(entry.file_name()));
        match p.is_dir() {
            true => copy_tree(&p, &q),
            false => {
                std::fs::copy(&p, &q).unwrap();
            }
        }
    }
}

#[test]
fn a_module_with_every_kind_of_item_collects_in_the_check_as_up_front() {
    let tmp = test_dir::TestDir::new();
    let std = tmp.path().join("std");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        &std,
    );
    std::fs::write(std.join("lang/hooks/any_record.velt"), RICH_ANY_RECORD).unwrap();
    let main = tmp.path().join("main.velt");
    std::fs::write(
        &main,
        "class Box {\n  size: i64 = 1;\n}\nfunction main() {\n  const a: any = new Box();\n  const r = a as Record<string, any>;\n  console.log(r[\"touched\"]);\n}\n",
    )
    .unwrap();
    let build = |eager: bool| {
        let mut c = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
        c.arg("build")
            .arg(&main)
            .args(["--emit", "vir"])
            .env("VELT_STD", &std)
            .env("VELT_HOME", tmp.path().join("home"))
            .env("VELT_BUILD_CACHE", "off")
            .env("VELT_BUILD_STAMP", "off");
        if eager {
            c.env("VELT_EAGER_STD", "1");
        } else {
            c.env_remove("VELT_EAGER_STD");
        }
        let out = c.output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "eager: {eager}\n{err}");
        out.stdout
    };
    let (lazy, eager) = (build(false), build(true));
    // The rich module is what was built, and it took a demand to get there.
    let text = String::from_utf8_lossy(&lazy);
    assert!(text.contains("touched"), "{text}");
    assert!(text.contains("derived"), "{text}");
    // And it does what it says.
    let run = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .arg("run")
        .arg(&main)
        .env("VELT_STD", &std)
        .env("VELT_HOME", tmp.path().join("home"))
        .env("VELT_BUILD_CACHE", "off")
        .env("VELT_BUILD_STAMP", "off")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "3 2 2 true 3 1 4 5 base derived derived tag s7\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    if lazy != eager {
        let (a, b) = (
            String::from_utf8_lossy(&lazy),
            String::from_utf8_lossy(&eager),
        );
        let found = a.lines().zip(b.lines()).find(|(x, y)| x != y);
        panic!("the VIR differs: {found:?}");
    }
}

/// Prints floats through a class field, a parameter, `+` and an array, none marked as a float by
/// syntax: the values below and 2,000 from random bit patterns (fixed seed).
const FLOATS: &str = r#"
class P {
  x: number = 0;
}
function show(x: number): void {
  const p = new P();
  p.x = x;
  console.log(`${p.x} ${String(x)} ${x + ""} ${[x]}`);
}
function main(): void {
  const xs: number[] = [
    0, -0, 1, -1, 0.1, 0.5, 1.5, 4.35, 0.000001, 1e-7, 1e21, 1e20, 123456789012345680000,
    9007199254740992, 1.7976931348623157e308, 5e-324, 2.2250738585072014e-308, 1e300, 0.3,
    NaN, Infinity, -Infinity,
  ];
  for (const x of xs) {
    show(x);
  }
  let s: u64 = 0x9e3779b97f4a7c15;
  for (let i = 0; i < 2000; i++) {
    s = s ^ (s << 13);
    s = s ^ (s >> 7);
    s = s ^ (s << 17);
    show(f64.fromBits(s));
  }
}
"#;

/// What Node prints for the first values of [`FLOATS`], one column of each line.
const NODE_FLOATS: &[&str] = &[
    "0",
    "0",
    "1",
    "-1",
    "0.1",
    "0.5",
    "1.5",
    "4.35",
    "0.000001",
    "1e-7",
    "1e+21",
    "100000000000000000000",
    "123456789012345680000",
    "9007199254740992",
    "1.7976931348623157e+308",
    "5e-324",
    "2.2250738585072014e-308",
    "1e+300",
    "0.3",
    "NaN",
    "Infinity",
    "-Infinity",
];

/// A std root and a compiler run over it, for the tests of a std without its on-demand modules.
fn velt_with_std(std: &Path, home: &Path) -> std::process::Command {
    let mut c = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    c.env("VELT_STD", std)
        .env("VELT_HOME", home)
        .env("VELT_BUILD_CACHE", "off")
        .env("VELT_BUILD_STAMP", "off")
        .env_remove("VELT_EAGER_STD");
    c
}

#[test]
fn a_missing_std_checks_as_it_did() {
    let tmp = test_dir::TestDir::new();
    let main = tmp.path().join("main.velt");
    std::fs::write(&main, "const n: number = 3;\nconsole.log(`x ${n}`);\n").unwrap();
    let out = velt_with_std(&tmp.path().join("nonexistent"), &tmp.path().join("home"))
        .arg("check")
        .arg(&main)
        .output()
        .unwrap();
    assert!(
        out.status.success() && out.stderr.is_empty() && out.stdout.is_empty(),
        "{out:?}"
    );
}

/// The claim that lets `number_text::may_hold_float` answer "no" for what it can prove: std's
/// float printing and the runtime's, which a program without the hook falls back to, print the
/// same text. A std root without the module also must report nothing (`Ctx::demand_std`).
#[test]
fn a_std_without_the_float_module_prints_the_same_through_the_runtime() {
    let tmp = test_dir::TestDir::new();
    let std = tmp.path().join("std");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        &std,
    );
    let main = tmp.path().join("main.velt");
    std::fs::write(&main, FLOATS).unwrap();
    let home = tmp.path().join("home");
    let vir = |std: &Path| {
        let out = velt_with_std(std, &home)
            .arg("build")
            .arg(&main)
            .args(["--emit", "vir"])
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success() && err.is_empty(), "{err}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let run = |std: &Path| {
        let out = velt_with_std(std, &home)
            .arg("run")
            .arg(&main)
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success() && err.is_empty(), "{err}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let in_std = run(&std);
    assert!(vir(&std).contains("floatPush"), "std's printer is not used");
    std::fs::remove_file(std.join("lang/hooks/number_float.velt")).unwrap();
    assert!(!vir(&std).contains("floatPush"), "std's printer is used");
    let in_runtime = run(&std);
    assert_eq!(in_std.lines().count(), NODE_FLOATS.len() + 2000);
    for (line, node) in in_std.lines().zip(NODE_FLOATS) {
        let columns: Vec<&str> = line.split(' ').collect();
        assert_eq!(columns, [*node; 4], "{line}");
    }
    let found = in_std.lines().zip(in_runtime.lines());
    let found = found.enumerate().find(|(_, (a, b))| a != b);
    assert_eq!(found, None, "std's printer and the runtime's differ");
    assert_eq!(in_std.lines().count(), in_runtime.lines().count());
}
