//! The std cache (#1728): std functions compiled once per key, stored in the build cache, linked
//! into every program that compiles the same code: release builds with `VELT_STD_OBJECT=1` (by
//! default those marked `noInline()`), debug builds by default, where only `velt cache warm`
//! stores objects.

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

/// The functions a build without objects for them left in the program, from the stats line.
fn left(o: &Output) -> usize {
    let err = text(&o.stderr);
    let line = (err.lines())
        .find(|l| l.contains("left in the program"))
        .unwrap_or_else(|| {
            panic!(
                "no stats in
{err}"
            )
        });
    let n: Vec<usize> = line
        .split([' ', ','])
        .filter_map(|w| w.parse().ok())
        .collect();
    n[2]
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

/// The stamp's `outside` line of `velt build a.velt -o <out>` in `dir`: what `velt test` keeps a
/// result by.
fn outside_line(
    dir: &Path,
    out: &str,
    cache: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> String {
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["build", "a.velt", "-o", out])
        .args(args)
        .current_dir(dir)
        .env("VELT_BUILD_CACHE", cache)
        .env_remove("VELT_BUILD_STAMP")
        .env_remove("VELT_STD_OBJECT");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let o = cmd.output().unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    let stem = format!("{out}{}.build-stamp", std::env::consts::EXE_SUFFIX);
    let stamp = std::fs::read_to_string(dir.join(stem)).unwrap();
    let line = stamp.lines().find(|l| l.starts_with("outside "));
    line.unwrap().to_string()
}

/// What a served function reaches still counts: a declaration hides it, and the stamp's `outside`
/// line must not say a program that reads files reaches nothing.
#[test]
fn what_a_served_function_reaches_counts_as_reached_in_a_release_build() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), READS_A_FILE).unwrap();
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let release = ["--release"];
    let compiled = outside_line(dir, "compiled", &cache, &release, &[]);
    let on = [
        ("VELT_STD_OBJECT", "1"),
        ("VELT_STD_OBJECT_MIN_MISSES", "1"),
    ];
    let served = outside_line(dir, "served", &cache, &release, &on);
    assert!(!entries(&cache).is_empty(), "nothing was served");
    assert!(served.contains("the file system"), "{served}");
    assert_eq!(served, compiled);
}

/// A program whose reach is all in std's code: `readFile`'s polling function is served.
const READS_A_FILE: &str = "import { readFile } from \"velt/fs\";
async function main() {
  const text = await readFile(\"a.velt\");
  console.log(text.length > 0);
}
";

/// `velt <args>` (a `velt dev`) with the cache on prints the program's first line.
fn assert_first_line_of_dev(dir: &Path, cache: &Path, args: &[&str]) {
    use std::io::BufRead;
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    let mut dev = (cmd.args(args))
        .current_dir(dir)
        .env("VELT_STD_OBJECT", "1")
        .env("VELT_BUILD_CACHE", cache)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let out = dev.stdout.take().unwrap();
    let (sent, first) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sent.send(std::io::BufReader::new(out).lines().next());
    });
    // timing: `velt dev` runs until stopped, and a host that can't start prints nothing and waits
    // for an edit, so only a time limit ends this when it is broken.
    let first = first.recv_timeout(std::time::Duration::from_secs(600));
    let _ = dev.kill();
    let _ = dev.wait();
    let first = first
        .expect("the JIT host printed nothing")
        .unwrap()
        .unwrap();
    assert_eq!(first, OUTPUT.lines().next().unwrap());
}

/// The command of a debug build of `file` to `out`, with its objects in `cache`, without a build
/// stamp.
fn debug_command(dir: &Path, file: &str, out: &str, cache: &Path) -> std::process::Command {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["build", file, "-o", out])
        .current_dir(dir)
        .env("VELT_STD_OBJECT_STATS", "1")
        .env("VELT_BUILD_CACHE", cache)
        .env("VELT_BUILD_STAMP", "off")
        .env_remove("VELT_STD_OBJECT")
        .env_remove("VELT_RT_LINK");
    cmd
}

/// A debug build of `file` to `out`, with its objects in `cache`, without a build stamp.
fn debug_build(dir: &Path, file: &str, out: &str, cache: &Path, extra: &[(&str, &str)]) -> Output {
    let mut cmd = debug_command(dir, file, out, cache);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let o = cmd.output().unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    o
}

/// Store what a debug build of `file` can link in `cache`, as `velt cache warm` does.
fn fill(dir: &Path, file: &str, cache: &Path) -> usize {
    let filled = debug_build(dir, file, "fill", cache, &[("VELT_STD_OBJECT_FILL", "1")]);
    let (_, compiled) = counts(&filled);
    assert!(compiled > 0, "{}", text(&filled.stderr));
    compiled
}

/// Without objects in the cache, a debug build compiles all of std itself; it never stores any;
/// and once there are some it links them, and prints the same.
#[test]
fn a_debug_build_links_what_is_stored_and_compiles_the_rest() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), PROGRAM).unwrap();
    let cold = debug_build(dir, "a.velt", "cold", &cache, &[]);
    assert!(
        !text(&cold.stderr).contains("found,"),
        "nothing to look for"
    );
    assert!(entries(&cache).is_empty(), "a debug build stores nothing");
    assert_eq!(run(dir, "cold"), OUTPUT);
    let compiled = fill(dir, "a.velt", &cache);
    assert_eq!(entries(&cache).len(), compiled);
    let warm = debug_build(dir, "a.velt", "warm", &cache, &[]);
    assert_eq!(counts(&warm), (compiled, 0), "{}", text(&warm.stderr));
    assert_eq!(run(dir, "warm"), OUTPUT);
    let off = debug_build(dir, "a.velt", "off", &cache, &[("VELT_STD_OBJECT", "0")]);
    assert!(!text(&off.stderr).contains("found,"), "the escape hatch");
    assert_eq!(run(dir, "off"), OUTPUT);
}

/// `-g` links the objects too: a debugger steps over them, and names their frames. The objects are
/// the same whether the program describes its variables or not.
#[test]
fn a_debug_build_with_debug_info_links_the_same_objects() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), PROGRAM).unwrap();
    let compiled = fill(dir, "a.velt", &cache);
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    let o = (cmd.args(["build", "a.velt", "-g", "-o", "g"]))
        .current_dir(dir)
        .env("VELT_STD_OBJECT_STATS", "1")
        .env("VELT_BUILD_CACHE", &cache)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert_eq!(counts(&o), (compiled, 0), "{}", text(&o.stderr));
    assert_eq!(run(dir, "g"), OUTPUT);
}

/// A program whose objects are only partly there: what is missing stays in the program and what
/// the objects call is found there, with no clang started, in a build whose clang is gone.
#[cfg(unix)]
#[test]
fn a_miss_stays_in_the_program_and_a_debug_build_never_starts_clang() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), PROGRAM).unwrap();
    let compiled = fill(dir, "a.velt", &cache);
    let mut stored = entries(&cache);
    stored.sort();
    for e in stored.iter().step_by(2) {
        std::fs::write(e, b"damaged").unwrap();
    }
    let clang = dir.join("clang-that-tells");
    let started = dir.join("clang-started");
    let script = format!("#!/bin/sh\ntouch {}\nexit 1\n", started.display());
    std::fs::write(&clang, script).unwrap();
    std::fs::set_permissions(&clang, std::fs::Permissions::from_mode(0o755)).unwrap();
    let partly = debug_build(
        dir,
        "a.velt",
        "partly",
        &cache,
        &[("VELT_CLANG", clang.to_str().unwrap())],
    );
    let (found, none) = counts(&partly);
    assert_eq!(none, 0, "{}", text(&partly.stderr));
    assert!(found > 0 && found < compiled, "{}", text(&partly.stderr));
    assert_eq!(run(dir, "partly"), OUTPUT);
    assert!(!started.exists(), "a debug build started clang");
}

/// Objects are for the target's default CPU: a program for this machine's CPU links them (`velt
/// run`), and one for less than the default doesn't.
#[test]
fn a_debug_build_links_objects_only_when_its_cpu_can_run_them() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), PROGRAM).unwrap();
    let compiled = fill(dir, "a.velt", &cache);
    let native = debug_build(dir, "a.velt", "n", &cache, &[("VELT_CPU", "native")]);
    assert_eq!(counts(&native), (compiled, 0), "{}", text(&native.stderr));
    assert_eq!(run(dir, "n"), OUTPUT);
    // Only x86-64's default requires anything a lesser CPU may lack.
    if cfg!(target_arch = "x86_64") {
        let required = velt_codegen_cl::required_features(&velt_codegen_cl::host_triple(), None);
        if required.is_ok_and(|r| r != 0) {
            let lesser = debug_build(dir, "a.velt", "l", &cache, &[("VELT_CPU", "x86-64")]);
            assert!(!text(&lesser.stderr).contains("found,"));
        }
    }
}

/// Builds of one output at once, against a cache holding its objects. Each removes the objects of
/// the build before and places its own, so without turns one removes what another is linking.
#[test]
fn builds_of_one_output_take_turns() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), PROGRAM).unwrap();
    fill(dir, "a.velt", &cache);
    for _ in 0..5 {
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| debug_build(dir, "a.velt", "same", &cache, &[]));
            }
        });
        assert_eq!(run(dir, "same"), OUTPUT);
    }
}

/// A build that finds its output taken says what it waits for once the wait is long (held here
/// until it is said, past `QUIET_WAIT`; a short wait says nothing: `std_cache::turn`), and goes on
/// when it is free.
#[test]
fn a_build_says_what_it_waits_for() {
    use std::io::BufRead;
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), PROGRAM).unwrap();
    let lock = std::fs::File::create(dir.join("taken.std.lock")).unwrap();
    lock.lock().unwrap();
    let mut child = debug_command(dir, "a.velt", "taken", &cache)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = std::io::BufReader::new(child.stderr.take().unwrap());
    let mut lines = stderr.lines().map_while(Result::ok);
    let line = (lines.by_ref()).find(|l| l.contains("waiting for another build"));
    assert!(line.is_some_and(|l| l.contains("taken.std.lock")));
    drop(lock);
    lines.for_each(drop);
    assert!(child.wait().unwrap().success());
    assert_eq!(run(dir, "taken"), OUTPUT);
}

/// A program that doesn't compile leaves no lock beside the output it never had.
#[test]
fn a_failed_compile_takes_no_turn() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), "function main() {\n  nope();\n}\n").unwrap();
    let o = debug_command(dir, "a.velt", "bad", &cache)
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(!dir.join("bad.std.lock").exists());
}

/// What a served function does is what the compiled one does: its output, its panics and where
/// they say they are, its exit code; integer overflow, bounds and throws, from std's code.
#[test]
fn served_functions_fail_the_way_compiled_ones_do() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    let programs = [
        // A throw out of std's URL parser, uncaught.
        "function main() {\n  console.log(new URL(\"https://a.example/x\").href);\n  new URL(\"not a url\");\n}\n",
        // A panic in std on an out-of-range argument.
        "function main() {\n  const u = new URL(\"https://a.example/x\");\n  console.log(u.href);\n  console.log(\"ab\".repeat(-1));\n}\n",
        // Bounds and overflow in the program's code, with std's on the stack before.
        "function main() {\n  const u = new URL(\"https://a.example/x\");\n  const xs: i32[] = [2147483647];\n  console.log(u.href.length);\n  const y: i32 = xs[0] + (u.href.length as i32);\n  console.log(y, xs[process.argv.length + 5]);\n}\n",
    ];
    for (n, program) in programs.iter().enumerate() {
        let file = format!("p{n}.velt");
        std::fs::write(dir.join(&file), program).unwrap();
        // Not `fill`: what the programs before stored may be all this one needs.
        debug_build(dir, &file, "fill", &cache, &[("VELT_STD_OBJECT_FILL", "1")]);
        let run_all = |out: &str| {
            let exe = dir
                .join(out)
                .with_extension(std::env::consts::EXE_EXTENSION);
            let o = crate::no_window::command(&exe).output().unwrap();
            (o.status.code(), text(&o.stdout), text(&o.stderr))
        };
        let served = debug_build(dir, &file, "served", &cache, &[]);
        assert!(counts(&served).0 > 0, "{}", text(&served.stderr));
        let compiled = debug_build(dir, &file, "compiled", &cache, &[("VELT_STD_OBJECT", "0")]);
        assert!(!text(&compiled.stderr).contains("found,"));
        let (a, b) = (run_all("served"), run_all("compiled"));
        assert_ne!(a.0, Some(0), "program {n} is to fail: {a:?}");
        assert_eq!(a, b, "program {n}");
    }
}

/// std's thread-local values are one value each between the objects and what Cranelift compiled:
/// epoch reclamation (`ConcurrentMap`) with half its functions served.
#[test]
fn a_thread_local_is_shared_between_objects_and_the_program() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/std");
    let source = golden.join("concurrent_map.velt");
    std::fs::copy(source, dir.join("a.velt")).unwrap();
    let expected = std::fs::read_to_string(golden.join("concurrent_map.out")).unwrap();
    let compiled = fill(dir, "a.velt", &cache);
    assert_eq!(run(dir, "fill"), expected);
    let mut stored = entries(&cache);
    stored.sort();
    for e in stored.iter().step_by(2) {
        std::fs::write(e, b"damaged").unwrap();
    }
    let partly = debug_build(dir, "a.velt", "partly", &cache, &[]);
    assert!(counts(&partly).0 < compiled, "{}", text(&partly.stderr));
    assert_eq!(run(dir, "partly"), expected);
}

/// `velt cache warm` is what stores objects for debug builds; a build of the same program then
/// compiles none of them.
#[test]
fn the_warm_up_stores_what_debug_builds_link() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    let share = dir.join("share");
    std::fs::create_dir_all(share.join("warm")).unwrap();
    std::fs::write(share.join("warm/a.velt"), PROGRAM).unwrap();
    std::fs::write(dir.join("a.velt"), PROGRAM).unwrap();
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    let warm = (cmd.args(["cache", "warm"]))
        .current_dir(dir)
        .env("VELT_BUILD_CACHE", &cache)
        .env("VELT_SHARE", &share)
        .output()
        .unwrap();
    assert!(warm.status.success(), "{}", text(&warm.stderr));
    assert!(!entries(&cache).is_empty(), "{}", text(&warm.stderr));
    let built = debug_build(dir, "a.velt", "a", &cache, &[]);
    let (found, compiled) = counts(&built);
    assert!(found > 0 && compiled == 0, "{}", text(&built.stderr));
    assert_eq!(run(dir, "a"), OUTPUT);
}

/// The same in a debug build, which is one by default.
#[test]
fn what_a_served_function_reaches_counts_as_reached_in_a_debug_build() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), READS_A_FILE).unwrap();
    assert!(fill(dir, "a.velt", &cache) > 0);
    let served = outside_line(dir, "served", &cache, &[], &[]);
    assert!(served.contains("the file system"), "{served}");
    let off = [("VELT_STD_OBJECT", "0")];
    assert_eq!(served, outside_line(dir, "compiled", &cache, &[], &off));
}

/// The JIT host has no link, so `velt dev` compiles std itself, whatever the cache holds.
#[test]
fn the_jit_host_never_uses_the_std_cache() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), PROGRAM).unwrap();
    fill(dir, "a.velt", &cache);
    assert_first_line_of_dev(dir, &cache, &["dev", "a.velt"]);
}

/// A generator must reach nothing outside its request, and that is read from the host's code: a
/// host never takes std's functions from the cache, where what they reach would hide behind
/// declarations. The fill setting would store and link them all, had the host used the cache.
#[test]
fn a_generator_host_stays_whole() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    let generator = "import { InstanceRequest, Output } from \"velt/generate\";
import { readFileSync } from \"velt/fs\";
export function sized(req: InstanceRequest, out: Output): void {
  out.line(`return ${out.literal(readFileSync(\"gen.velt\").length.toString())};`);
}
";
    std::fs::write(dir.join("gen.velt"), generator).unwrap();
    let program = "declare generated(\"./gen\") function sized<T>(): string;
class User {}
function main() {
  console.log(sized<User>());
}
";
    std::fs::write(dir.join("a.velt"), program).unwrap();
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    let o = (cmd.args(["build", "a.velt", "-o", "a"]))
        .current_dir(dir)
        .env("VELT_BUILD_CACHE", &cache)
        .env("VELT_STD_OBJECT_FILL", "1")
        .env_remove("VELT_STD_OBJECT")
        .output()
        .unwrap();
    let err = text(&o.stderr);
    assert!(!o.status.success(), "built: {err}");
    assert!(err.contains("cannot reach the file system"), "{err}");
}

/// A program reaching std functions marked `noInline()`: number formatting (`pushF64`,
/// `f64Text`, `fixedText`), beside URL code, which has none.
const NO_INLINE_PROGRAM: &str = "function main() {
  const x = process.argv.length * 1.5;
  console.log(`x=${x}`, x / 7, (x * 1000).toFixed(3));
  const u = new URL(\"https://example.com:8080/a/b/../c?q=1\");
  console.log(u.href, u.port);
}
";
const NO_INLINE_OUTPUT: &str =
    "x=3 0.42857142857142855 3000.000\nhttps://example.com:8080/a/c?q=1 8080\n";

/// A release build without `VELT_STD_OBJECT` (the default), in `dir`, to `out`.
fn release_default(
    dir: &Path,
    file: &str,
    out: &str,
    cache: &Path,
    extra: &[(&str, &str)],
) -> Output {
    let mut cmd = debug_command(dir, file, out, cache);
    cmd.arg("--release");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let o = cmd.output().unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    o
}

/// By default a release build links the objects of the `noInline()` std functions that the
/// warm-up stored, and prints what a build compiling all of std prints. Only those: a build
/// serving everything (`VELT_STD_OBJECT=1`) finds the same objects and compiles the URL code's,
/// which the default left in the program.
#[test]
fn a_release_build_links_the_no_inline_functions_stored() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), NO_INLINE_PROGRAM).unwrap();
    let fill = [("VELT_STD_OBJECT_FILL", "1")];
    let cold = release_default(dir, "a.velt", "cold", &cache, &[]);
    assert!(
        !text(&cold.stderr).contains("found,"),
        "nothing to look for"
    );
    assert!(entries(&cache).is_empty(), "a release build stores nothing");
    assert_eq!(run(dir, "cold"), NO_INLINE_OUTPUT);
    let filled = release_default(dir, "a.velt", "filled", &cache, &fill);
    let (_, compiled) = counts(&filled);
    assert!(compiled > 0, "{}", text(&filled.stderr));
    assert_eq!(run(dir, "filled"), NO_INLINE_OUTPUT);
    let served = release_default(dir, "a.velt", "served", &cache, &[]);
    assert_eq!(counts(&served), (compiled, 0), "{}", text(&served.stderr));
    let err = text(&served.stderr);
    assert!(err.contains("std cache: links"), "{err}");
    assert_eq!(run(dir, "served"), NO_INLINE_OUTPUT);
    let off = release_default(dir, "a.velt", "off", &cache, &[("VELT_STD_OBJECT", "0")]);
    assert!(!text(&off.stderr).contains("found,"), "the escape hatch");
    assert_eq!(run(dir, "off"), NO_INLINE_OUTPUT);
    let all = build(
        dir,
        "a.velt",
        "all",
        &cache,
        &[("VELT_STD_OBJECT_MIN_MISSES", "1")],
    );
    let (found, more) = counts(&all);
    assert!(found > 0 && more > 0, "{}", text(&all.stderr));
    assert_eq!(run(dir, "all"), NO_INLINE_OUTPUT);
}

/// A miss is no failure: what the cache lacks stays in the program, and the build prints the same.
/// Objects a debug build links are no release build's, nor the other way round.
#[test]
fn a_release_build_compiles_its_misses_and_never_links_a_debug_object() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), NO_INLINE_PROGRAM).unwrap();
    let debug = fill(dir, "a.velt", &cache);
    let release = release_default(dir, "a.velt", "r", &cache, &[]);
    let (found, compiled) = counts(&release);
    assert_eq!((found, compiled), (0, 0), "{}", text(&release.stderr));
    assert!(left(&release) > 0, "{}", text(&release.stderr));
    assert_eq!(run(dir, "r"), NO_INLINE_OUTPUT);
    let stored = entries(&cache).len();
    release_default(dir, "a.velt", "f", &cache, &[("VELT_STD_OBJECT_FILL", "1")]);
    assert!(entries(&cache).len() > stored);
    let again = debug_build(dir, "a.velt", "d", &cache, &[]);
    assert_eq!(counts(&again), (debug, 0), "{}", text(&again.stderr));
    for e in entries(&cache) {
        std::fs::write(e, b"damaged").unwrap();
    }
    let damaged = release_default(dir, "a.velt", "x", &cache, &[]);
    assert_eq!(counts(&damaged), (0, 0), "{}", text(&damaged.stderr));
    assert_eq!(run(dir, "x"), NO_INLINE_OUTPUT);
}

/// The symbols a build's stats say it served.
fn served_symbols(o: &Output) -> Vec<String> {
    let err = text(&o.stderr);
    let links = (err.lines()).filter_map(|l| l.strip_prefix("velt: std cache: links "));
    links
        .filter_map(|l| l.split(' ').next())
        .map(str::to_string)
        .collect()
}

/// The functions of `file`'s optimized release VIR, each with whether it is marked `noInline()`.
fn vir_functions(dir: &Path, file: &str) -> std::collections::HashMap<String, bool> {
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    let o = (cmd.args(["build", file, "--release", "--emit", "vir"]))
        .current_dir(dir)
        .env("VELT_STD_OBJECT", "0")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o.stderr));
    (text(&o.stdout).lines())
        .filter(|l| l.starts_with("fn#"))
        .filter_map(|l| {
            let symbol = l.split(' ').nth(2)?.split('(').next()?;
            Some((symbol.to_string(), l.contains(" noinline")))
        })
        .collect()
}

/// Every function a release build serves by default is one std marks `noInline()` (as the
/// program's own VIR shows it), and the rest of std stays in the program (`inlinable`).
#[test]
fn every_function_a_release_build_serves_is_marked_no_inline() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    std::fs::write(dir.join("a.velt"), NO_INLINE_PROGRAM).unwrap();
    let fill = [("VELT_STD_OBJECT_FILL", "1")];
    release_default(dir, "a.velt", "filled", &cache, &fill);
    let served = release_default(dir, "a.velt", "served", &cache, &[]);
    let err = text(&served.stderr);
    let symbols = served_symbols(&served);
    assert!(!symbols.is_empty(), "{err}");
    let vir = vir_functions(dir, "a.velt");
    let seen: Vec<&String> = symbols.iter().filter(|s| vir.contains_key(*s)).collect();
    assert!(!seen.is_empty(), "no served function in the VIR:\n{err}");
    for s in seen {
        assert!(vir[s], "{s} is served but not marked `noInline()`");
    }
    let by_reason = (err.lines()).find(|l| l.contains("by reason"));
    assert!(by_reason.is_some_and(|l| l.contains("inlinable")), "{err}");
    assert_eq!(run(dir, "served"), NO_INLINE_OUTPUT);
}

/// `velt cache warm` stores what release builds link too: a release build of a warm-up program
/// then finds its `noInline()` functions and leaves none in the program.
#[test]
fn the_warm_up_stores_what_release_builds_link() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    let share = dir.join("share");
    std::fs::create_dir_all(share.join("warm")).unwrap();
    std::fs::write(share.join("warm/a.velt"), NO_INLINE_PROGRAM).unwrap();
    std::fs::write(dir.join("a.velt"), NO_INLINE_PROGRAM).unwrap();
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    let warm = (cmd.args(["cache", "warm"]))
        .current_dir(dir)
        .env("VELT_BUILD_CACHE", &cache)
        .env("VELT_SHARE", &share)
        .env_remove("VELT_STD_OBJECT")
        .output()
        .unwrap();
    assert!(warm.status.success(), "{}", text(&warm.stderr));
    let built = release_default(dir, "a.velt", "a", &cache, &[]);
    let (found, compiled) = counts(&built);
    assert!(found > 0 && compiled == 0, "{}", text(&built.stderr));
    assert_eq!(left(&built), 0, "{}", text(&built.stderr));
    assert_eq!(run(dir, "a"), NO_INLINE_OUTPUT);
}
