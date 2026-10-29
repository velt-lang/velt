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
    std::fs::write(dir.join("a.vlt"), PROGRAM).unwrap();
    std::fs::write(dir.join("b.vlt"), format!("// another program\n{PROGRAM}")).unwrap();
    let off = build(dir, "a.vlt", "off", &cache, &[("VELT_STD_OBJECT", "0")]);
    assert!(!text(&off.stderr).contains("found,"));
    assert_eq!(run(dir, "off"), OUTPUT);
    let cold = build(dir, "a.vlt", "a", &cache, &[]);
    let (found, compiled) = counts(&cold);
    assert!(found == 0 && compiled > 0, "{}", text(&cold.stderr));
    assert_eq!(run(dir, "a"), OUTPUT);
    let warm = build(dir, "b.vlt", "b", &cache, &[]);
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
    std::fs::write(dir.join("a.vlt"), PROGRAM).unwrap();
    let outs: Vec<Output> = std::thread::scope(|s| {
        let builds: Vec<_> = ["x", "y"]
            .map(|out| {
                let (dir, cache) = (dir, &cache);
                s.spawn(move || build(dir, "a.vlt", out, cache, &[]))
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
    let again = build(dir, "a.vlt", "z", &cache, &[]);
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
    std::fs::write(dir.join("a.vlt"), PROGRAM).unwrap();
    // One environment for every build: the stamp's key includes `VELT_*` variables.
    let stamped = || {
        let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
        let o = (cmd.args(["build", "a.vlt", "--release", "-o", "a", "-v"]))
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
