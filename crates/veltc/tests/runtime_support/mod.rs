//! The runtime libraries must sit next to the `velt` binary, and be current, before a test can
//! link a program: the staticlib (`velt_rt`, release builds) and the shared library
//! (`velt_rt_shared`, which debug builds link and copy next to every executable). Neither is a
//! dependency of `veltc`, so `cargo test -p veltc` alone would keep running programs against
//! whatever runtime an earlier build left there (#509): [`build_native_runtime`] builds both, once
//! per test process, and then checks that they are not older than their sources.
//!
//! The gate (`cargo xtask check`) builds the whole workspace first and sets
//! `VELT_RT_PREBUILT=1`, so the tests skip that build: `-p velt_rt` resolves the runtime's
//! dependencies with other features than `--workspace` does, so it would compile the runtime
//! and part of its dependencies a second time, a parallel test runner would wait on the build
//! lock in every test process, and rebuilding the shared runtime would replace the import
//! library while other tests link against it. The freshness check runs either way.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

/// Build the runtime libraries in this test's profile (unless prebuilt), then check that they
/// are current. `root` is where cargo runs. Does the work once per test process.
pub fn build_native_runtime(root: &Path) {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        if !std::env::var_os("VELT_RT_PREBUILT").is_some_and(|v| v == "1") {
            build(root);
        }
        if let Err(e) = check_fresh() {
            panic!("{e}");
        }
    });
}

fn build(root: &Path) {
    let mut args = vec!["build", "-p", "velt_rt"];
    // musl links statically and cannot build a cdylib; `velt` falls back to the static runtime.
    if !cfg!(target_env = "musl") {
        args.extend(["-p", "velt_rt_shared"]);
    }
    if !cfg!(debug_assertions) {
        args.push("--release");
    }
    let st = crate::no_window::command(env!("CARGO"))
        .args(&args)
        .current_dir(root)
        .status()
        .expect("run cargo");
    assert!(st.success(), "`cargo {}` failed", args.join(" "));
}

/// The runtime libraries next to `velt` (those that exist).
fn runtime_libs() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_BIN_EXE_velt"))
        .parent()
        .expect("ICE: velt has a directory");
    let names: &[&str] = if cfg!(windows) {
        &["velt_rt.lib", "velt_rt_shared.dll"]
    } else if cfg!(target_os = "macos") {
        &["libvelt_rt.a", "libvelt_rt_shared.dylib"]
    } else {
        &["libvelt_rt.a", "libvelt_rt_shared.so"]
    };
    names
        .iter()
        .map(|n| dir.join(n))
        .filter(|p| p.is_file())
        .collect()
}

/// Fails (with what to run) when a runtime library is older than one of its sources.
fn check_fresh() -> Result<(), String> {
    check_libs(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
        &runtime_libs(),
    )
}

/// [`check_fresh`] for `libs`, whose sources are in `repo`.
fn check_libs(repo: &Path, libs: &[PathBuf]) -> Result<(), String> {
    for lib in libs {
        // Cargo writes the dep-info next to the library: `velt_rt.d`, `libvelt_rt_shared.d`.
        let Ok(dep_info) = std::fs::read_to_string(lib.with_extension("d")) else {
            continue;
        };
        if let Some(src) = newer_source(repo, &dep_info, modified(lib)) {
            return Err(format!(
                "{} is older than {}: programs would run against a stale runtime. Build it \
                 first: `cargo build --workspace` (or `cargo build -p velt_rt -p velt_rt_shared`)",
                lib.display(),
                src.strip_prefix(repo).unwrap_or(&src).display()
            ));
        }
    }
    Ok(())
}

fn modified(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// A source of the runtime, in this checkout, modified after `built`. The sources are the files
/// cargo's dep-info (`<lib>.d`) lists under `crates/velt_rt*`, looked up in `repo`: a target
/// directory shared by several checkouts records another checkout's paths.
fn newer_source(repo: &Path, dep_info: &str, built: Option<SystemTime>) -> Option<PathBuf> {
    let built = built?;
    dep_files(dep_info)
        .filter_map(|f| runtime_source(&f))
        .map(|rel| repo.join(rel))
        .filter(|p| p.is_file())
        .find(|p| modified(p).is_some_and(|t| t > built))
}

/// The files a dep-info file lists (after `<output>:`); `\ ` is an escaped space.
fn dep_files(dep_info: &str) -> impl Iterator<Item = String> + '_ {
    let deps = dep_info.lines().next().unwrap_or("");
    // The output path ends at the first `: ` (a Windows drive letter's colon is not followed by
    // a space).
    let deps = deps.find(": ").map_or("", |i| &deps[i + 2..]);
    let mut files = vec![];
    let mut cur = String::new();
    let mut chars = deps.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&' ') => cur.push(chars.next().unwrap_or(' ')),
            ' ' => files.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    files.push(cur);
    files.into_iter().filter(|f| !f.is_empty())
}

/// `crates/velt_rt…/<rest>` of a source path (with `..` resolved), if it is a runtime source.
fn runtime_source(file: &str) -> Option<PathBuf> {
    let normalized = file.replace('\\', "/");
    let mut parts: Vec<&str> = vec![];
    for c in Path::new(&normalized).components() {
        match c {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(s) => parts.push(s.to_str()?),
            _ => {}
        }
    }
    let at = parts.iter().rposition(|p| *p == "crates")?;
    let krate = parts.get(at + 1)?;
    krate
        .starts_with("velt_rt")
        .then(|| parts[at..].iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dep_info_sources_map_to_this_checkout() {
        let d = r"C:/t\debug\velt_rt_shared.dll: C:\w\other\crates\velt_rt_shared\..\velt_rt\src\a.rs C:\Users\x\.cargo\registry\tokio\src\lib.rs /w/my\ dir/crates/velt_rt/src/b\ c.rs";
        let files: Vec<String> = dep_files(d).collect();
        assert_eq!(files.len(), 3);
        assert_eq!(files[2], "/w/my dir/crates/velt_rt/src/b c.rs");
        let rel: Vec<PathBuf> = files.iter().filter_map(|f| runtime_source(f)).collect();
        assert_eq!(
            rel,
            [
                PathBuf::from("crates/velt_rt/src/a.rs"),
                PathBuf::from("crates/velt_rt/src/b c.rs")
            ]
        );
    }

    /// A library is stale once one of its sources (in this checkout, whatever checkout the
    /// dep-info names) is newer, and current again once rebuilt.
    #[test]
    fn a_touched_source_makes_the_library_stale() {
        use std::time::Duration;
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo = tmp.path().join("repo");
        let src = repo.join("crates/velt_rt/src/lib.rs");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, "").unwrap();
        let out = tmp.path().join("target/debug");
        std::fs::create_dir_all(&out).unwrap();
        let lib = out.join(if cfg!(windows) { "velt_rt.lib" } else { "libvelt_rt.a" });
        std::fs::write(&lib, "").unwrap();
        std::fs::write(
            lib.with_extension("d"),
            format!(
                "{}: /elsewhere/crates/velt_rt/src/lib.rs /elsewhere/crates/velt_rt/src/gone.rs
",
                lib.display()
            ),
        )
        .unwrap();
        let set = |p: &Path, t: SystemTime| {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(t)
                .unwrap()
        };
        let now = SystemTime::now();
        set(&src, now - Duration::from_secs(60));
        set(&lib, now - Duration::from_secs(30));
        let libs = [lib.clone(), out.join("missing.dll")];
        assert_eq!(check_libs(&repo, &libs), Ok(()));
        set(&src, now);
        let err = check_libs(&repo, &libs).expect_err("stale");
        assert!(err.contains("stale runtime"), "{err}");
        assert!(err.contains("lib.rs"), "{err}");
        set(&lib, now + Duration::from_secs(1));
        assert_eq!(check_libs(&repo, &libs), Ok(()));
    }
}
