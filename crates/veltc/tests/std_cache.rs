//! The std cache (`VELT_STD_OBJECT=1`, release builds; #1728): std functions compiled once per
//! key, stored in the build cache, linked into every program that compiles the same code.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
mod runtime_support;
mod test_dir;

/// A program whose std code the cache serves: URL parsing and serialization.
const PROGRAM: &str = "function main() {
  const u = new URL(\"https://user:pw@example.com:8080/a/b/../c?q=1&r=two#frag\");
  console.log(u.href, u.host, u.pathname, u.search, u.hash);
  const v = new URL(\"../d e?x=%20y\", u.href);
  console.log(v.href);
}
";
const OUTPUT: &str = "https://user:pw@example.com:8080/a/c?q=1&r=two#frag example.com:8080 /a/c ?q=1&r=two #frag\nhttps://user:pw@example.com:8080/d%20e?x=%20y\n";

/// `velt build --release` of `file` in `dir` to `out`, with the std cache in `cache`.
fn build(dir: &Path, file: &str, out: &str, cache: &Path, extra: &[(&str, &str)]) -> Output {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["build", file, "--release", "-o", out])
        .current_dir(dir)
        .env("VELT_STD_OBJECT", "1")
        .env("VELT_STD_OBJECT_STATS", "1")
        .env("VELT_BUILD_CACHE", cache)
        .env_remove("VELT_RT_LINK");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let o = cmd.output().unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    o
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn run(dir: &Path, exe: &str) -> String {
    let exe = dir
        .join(exe)
        .with_extension(std::env::consts::EXE_EXTENSION);
    text(&crate::no_window::command(&exe).output().unwrap().stdout)
}

/// `(found, compiled)` from the stats line.
fn counts(o: &Output) -> (usize, usize) {
    let err = text(&o.stderr);
    let line = (err.lines())
        .find(|l| l.contains("found,"))
        .unwrap_or_else(|| panic!("no stats in\n{err}"));
    let n: Vec<usize> = line
        .split([' ', ','])
        .filter_map(|w| w.parse().ok())
        .collect();
    (n[0], n[1])
}

/// The cached objects (`stdfn` file entries, not their sidecars).
fn entries(cache: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut pending = vec![cache.to_path_buf()];
    while let Some(d) = pending.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                pending.push(p);
            } else if p.components().any(|c| c.as_os_str() == "stdfn") && p.extension().is_none() {
                out.push(p);
            }
        }
    }
    out
}

/// A cold build stores std functions, a second program's build links them, and both print what
/// a build without the cache prints.
#[test]
fn a_second_build_links_what_the_first_compiled() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), PROGRAM).unwrap();
    std::fs::write(dir.join("b.velt"), format!("// another program\n{PROGRAM}")).unwrap();
    let off = build(dir, "a.velt", "off", &cache, &[("VELT_STD_OBJECT", "0")]);
    assert!(!text(&off.stderr).contains("found,"));
    assert_eq!(run(dir, "off"), OUTPUT);
    let cold = build(dir, "a.velt", "a", &cache, &[]);
    let (found, compiled) = counts(&cold);
    assert!(found == 0 && compiled > 0, "{}", text(&cold.stderr));
    assert_eq!(run(dir, "a"), OUTPUT);
    let warm = build(dir, "b.velt", "b", &cache, &[]);
    assert_eq!(counts(&warm), (compiled, 0), "{}", text(&warm.stderr));
    assert_eq!(run(dir, "b"), OUTPUT);
}

/// Two builds missing the same keys at once both succeed, and leave one valid entry per key; a
/// damaged entry is a miss (compiled and stored again), never a link error.
#[test]
fn concurrent_and_damaged_entries_are_misses_not_failures() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), PROGRAM).unwrap();
    let outs: Vec<Output> = std::thread::scope(|s| {
        let builds: Vec<_> = ["x", "y"]
            .map(|out| {
                let (dir, cache) = (dir, &cache);
                s.spawn(move || build(dir, "a.velt", out, cache, &[]))
            })
            .into_iter()
            .collect();
        builds.into_iter().map(|b| b.join().unwrap()).collect()
    });
    let compiled = counts(&outs[0]).0 + counts(&outs[0]).1;
    assert_eq!(compiled, counts(&outs[1]).0 + counts(&outs[1]).1);
    assert_eq!(
        (run(dir, "x"), run(dir, "y")),
        (OUTPUT.into(), OUTPUT.into())
    );
    let stored = entries(&cache);
    assert_eq!(stored.len(), compiled, "one entry per key");
    for e in &stored {
        std::fs::write(e, b"truncated").unwrap();
    }
    let again = build(dir, "a.velt", "z", &cache, &[]);
    assert_eq!(counts(&again), (0, compiled), "{}", text(&again.stderr));
    assert_eq!(run(dir, "z"), OUTPUT);
}

/// A stamped build records the std objects it linked: unchanged, it links them again without
/// compiling; one of them rewritten makes it build again.
#[test]
fn the_stamp_records_the_std_objects() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), PROGRAM).unwrap();
    // One environment for every build: the stamp's key includes `VELT_*` variables.
    let stamped = || {
        let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
        let o = (cmd.args(["build", "a.velt", "--release", "-o", "a", "-v"]))
            .current_dir(dir)
            .env("VELT_STD_OBJECT", "1")
            .env("VELT_BUILD_CACHE", &cache)
            .env_remove("VELT_STD_OBJECT_STATS")
            .env_remove("VELT_BUILD_STAMP")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", text(&o.stderr));
        text(&o.stderr).contains("nothing to compile")
    };
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    assert!(!stamped(), "first build");
    assert!(stamped(), "unchanged: the std objects are linked again");
    assert_eq!(run(dir, "a"), OUTPUT);
    let member = entries(&cache).into_iter().next().unwrap();
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&member)
        .unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
        .unwrap();
    assert!(!stamped(), "a rewritten std object is a change");
    assert_eq!(run(dir, "a"), OUTPUT);
}

/// A build whose misses come to fewer statements than the threshold leaves them in the program
/// (a clang process each would cost more than they save); a build over it stores them, and then
/// the first links them. The threshold is set around this program's own size
/// (`VELT_STD_OBJECT_MIN_MISSES`), so the test doesn't depend on how large std grows.
#[test]
fn a_build_with_few_misses_leaves_them_in_the_program() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), PROGRAM).unwrap();
    let high = [("VELT_STD_OBJECT_MIN_MISSES", "100000000")];
    let cold = build(dir, "a.velt", "a1", &cache, &high);
    assert_eq!(counts(&cold), (0, 0), "{}", text(&cold.stderr));
    assert_eq!(run(dir, "a1"), OUTPUT);
    let filled = build(
        dir,
        "a.velt",
        "a2",
        &cache,
        &[("VELT_STD_OBJECT_MIN_MISSES", "1")],
    );
    let compiled = counts(&filled).1;
    assert!(compiled > 0, "{}", text(&filled.stderr));
    let warm = build(dir, "a.velt", "a3", &cache, &high);
    assert_eq!(counts(&warm), (compiled, 0), "{}", text(&warm.stderr));
    assert_eq!(run(dir, "a3"), OUTPUT);
}

/// Only release executables built by LLVM without debug info use the std cache: an object, VIR
/// or LLVM IR would name std functions it does not define, cached objects have no line info, and
/// the cache holds LLVM's code.
#[test]
fn only_release_executables_use_the_std_cache() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), PROGRAM).unwrap();
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    for extra in [
        &["--emit", "obj", "-o", "a.o"][..],
        &["-g", "-o", "g"][..],
        &["--backend", "cranelift", "-o", "c"][..],
    ] {
        let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
        let o = (cmd.args(["build", "a.velt", "--release"]).args(extra))
            .current_dir(dir)
            .env("VELT_STD_OBJECT", "1")
            .env("VELT_STD_OBJECT_STATS", "1")
            .env("VELT_BUILD_CACHE", &cache)
            .output()
            .unwrap();
        let err = text(&o.stderr);
        assert!(o.status.success(), "{extra:?}: {err}");
        assert!(
            !err.contains("std cache"),
            "{extra:?} used the std cache:\n{err}"
        );
    }
}

/// A program whose std code includes what a std generator wrote for it: `JSON.parse<Item[]>`'s
/// reader of `Item`.
const JSON_PROGRAM: &str = "struct Item {
  id: i64;
  name: string;
  tags: string[];
}

function main() {
  const items = JSON.parse<Item[]>(`[{\"id\":1,\"name\":\"a\",\"tags\":[\"x\",\"y\"]},{\"id\":2,\"name\":\"b\",\"tags\":[]}]`);
  let n: i64 = 0;
  for (const it of items) n += it.id + (it.tags.length as i64);
  console.log(items.length, n, items[0].name);
}
";

/// Editing the program's own code compiles none of its std code again: the second build finds
/// every std function the first compiled, the code std's JSON generator wrote for the program
/// among them, and compiles nothing (counted, not timed).
#[test]
fn an_edited_program_compiles_no_std_code_again() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    let all = [("VELT_STD_OBJECT_MIN_MISSES", "1")];
    std::fs::write(dir.join("a.velt"), JSON_PROGRAM).unwrap();
    let cold = build(dir, "a.velt", "a1", &cache, &all);
    let (found, compiled) = counts(&cold);
    assert!(found == 0 && compiled > 0, "{}", text(&cold.stderr));
    // Mangled `<generated>`: a function std's generator wrote is served.
    let generated = |o: &Output| {
        (text(&o.stderr).lines())
            .any(|l| l.contains("std cache: links") && l.contains("_3cgenerated_3e"))
    };
    assert!(generated(&cold), "{}", text(&cold.stderr));
    assert_eq!(run(dir, "a1"), "2 5 a\n");
    let edited = JSON_PROGRAM.replace(
        "function main() {\n",
        "function main() {\n  console.log(\"edited\");\n",
    );
    std::fs::write(dir.join("a.velt"), edited).unwrap();
    let warm = build(dir, "a.velt", "a2", &cache, &all);
    assert_eq!(counts(&warm), (compiled, 0), "{}", text(&warm.stderr));
    assert!(generated(&warm), "{}", text(&warm.stderr));
    assert_eq!(run(dir, "a2"), "edited\n2 5 a\n");
}

/// Std code that reaches statics with identity (the JSON reader's type descriptors and tables)
/// is served too: the statics are named and the cached objects use the program's copies. The
/// program prints what a build without the cache prints, cold and warm.
#[test]
fn code_reaching_statics_with_identity_is_served() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    let all = [("VELT_STD_OBJECT_MIN_MISSES", "1")];
    std::fs::write(dir.join("a.velt"), JSON_PROGRAM).unwrap();
    let cold = build(dir, "a.velt", "a1", &cache, &all);
    let err = text(&cold.stderr);
    let named: usize = (err.lines())
        .find_map(|l| {
            l.strip_prefix("velt: std cache: ")?
                .strip_suffix(" statics named")
        })
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no count of named statics in\n{err}"));
    assert!(named > 0, "{err}");
    let by_reason = (err.lines())
        .find(|l| l.contains("by reason"))
        .unwrap_or_else(|| panic!("{err}"));
    assert!(!by_reason.contains("statics with identity"), "{by_reason}");
    assert_eq!(run(dir, "a1"), "2 5 a\n");
    let warm = build(dir, "a.velt", "a2", &cache, &all);
    assert_eq!(
        counts(&warm),
        (counts(&cold).1, 0),
        "{}",
        text(&warm.stderr)
    );
    assert_eq!(run(dir, "a2"), "2 5 a\n");
}

/// In a build that clang compiles as several units, the statics only served code uses are
/// still defined by the program: no unit's bodies reach them any more, but the cached objects
/// refer to them (`velt_codegen_llvm` units: an `Export` static gets an owner). Cold and warm
/// builds link and print what a build without the cache prints.
#[test]
fn named_statics_are_defined_in_a_build_of_several_units() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    let units = [
        ("VELT_STD_OBJECT_MIN_MISSES", "1"),
        ("VELT_CODEGEN_UNITS", "4"),
    ];
    std::fs::write(dir.join("a.velt"), JSON_PROGRAM).unwrap();
    let cold = build(dir, "a.velt", "a1", &cache, &units);
    assert!(counts(&cold).1 > 0, "{}", text(&cold.stderr));
    assert_eq!(run(dir, "a1"), "2 5 a\n");
    let warm = build(dir, "a.velt", "a2", &cache, &units);
    assert_eq!(
        counts(&warm),
        (counts(&cold).1, 0),
        "{}",
        text(&warm.stderr)
    );
    assert_eq!(run(dir, "a2"), "2 5 a\n");
}
