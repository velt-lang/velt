//! Generated code and the program built from it are the same in every run (#2243). Two
//! instances of a generated function whose type arguments erase to one layout (two unions that
//! are one type once brands and `readonly` are erased) have the same body, and lowering used to
//! pick between them in a hash map's random order, so the VIR, and every cache key over it,
//! changed from run to run.
//!
//! The check: the generator's output and `--emit vir` of the golden that has such instances,
//! from builds in fresh homes with the build cache off, are byte for byte the same.

mod no_window;
mod test_dir;

use std::path::{Path, PathBuf};

/// The golden whose generated JSON writers erase to one layout.
fn program() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/lang/union_erased_layout.velt")
}

/// The generated modules (path and text, sorted) and the VIR of one build in a fresh home.
fn build_once() -> (Vec<(String, Vec<u8>)>, Vec<u8>) {
    let tmp = test_dir::TestDir::new();
    let generated = tmp.path().join("generated");
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(program())
        .args(["--emit", "vir"])
        .env("VELT_HOME", tmp.path().join("home"))
        .env("VELT_BUILD_CACHE", "off")
        .env("VELT_BUILD_STAMP", "off")
        .env("VELT_EMIT_GENERATED", &generated)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut files = vec![];
    let mut pending = vec![generated.clone()];
    while let Some(dir) = pending.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                pending.push(p);
            } else {
                let rel = p.strip_prefix(&generated).unwrap().display().to_string();
                files.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    files.sort();
    assert!(!files.is_empty(), "no generated module was written");
    (files, out.stdout)
}

/// Every build gives the same generated modules and the same VIR. Each build is a new process,
/// so its hash maps get new random keys: with the bug, either of two bodies was picked about half
/// the time, so six builds were all alike about once in thirty.
#[test]
fn generated_code_and_vir_are_the_same_in_every_build() {
    let (generated, vir) = build_once();
    for n in 1..6 {
        let (g, v) = build_once();
        assert!(g == generated, "build {n}: the generated modules differ");
        assert!(v == vir, "build {n}: the VIR differs");
    }
}
