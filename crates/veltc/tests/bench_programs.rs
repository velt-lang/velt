//! The benchmark programs keep compiling: every `bench/**/*.vlt` goes through parse, sema,
//! lowering and VIR verification (no codegen or linking, so the set checks in seconds).
//!
//! A program inside a package (a directory with `package.vlt`: the server and database benchmarks
//! depend on the repository's packages by path) is compiled with that package's dependencies
//! installed as `velt check` installs them (`--locked`, so the committed lockfile must be current),
//! which builds a native package's library with cargo the first time.
//!
//! Nothing else builds `bench/`, so a change to the standard library or the checker that breaks
//! a benchmark (a driver function gaining an error type, say) would otherwise go unnoticed until
//! someone runs it. The gate runs this test whenever the compiler, the standard library or
//! `bench/` changes (crates/xtask/src/plan.rs).
//!
//! A program that does not name `WeakMap`, `WeakSet` or `WeakRef` must have no weak-capable
//! types (docs/internals/design/weak-refs.md): its VIR is the one it had before weak references
//! existed, which shows as no call of the weak runtime (every release of a weak-capable type
//! has one).

mod no_window;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use veltc::cli::Emit;
use veltc::driver::{self, BuildError, BuildOptions, Session};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// The `.vlt` files under `dir`, recursively, sorted; a file whose name starts with `_` is a
/// module of a program, checked through the program that imports it (as in the goldens).
fn programs(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            out.extend(programs(&path));
        } else if path.extension().is_some_and(|e| e == "vlt")
            && !path.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with('_') || n == vpm::manifest::MANIFEST_FILE
            })
        {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// Whether one of the program's first lines says `// requires: unix` or `// requires: windows`
/// (as in the goldens) and this host is the other: the program is not checked here.
fn other_os_only(path: &Path) -> bool {
    let src = std::fs::read_to_string(path).unwrap_or_default();
    let wants = |what: &str| {
        src.lines()
            .take(10)
            .filter_map(|l| l.trim().strip_prefix("// requires:"))
            .any(|names| names.split_whitespace().any(|n| n == what))
    };
    (wants("unix") && cfg!(windows)) || (wants("windows") && !cfg!(windows))
}

/// The dependencies of the package `path` is in (none outside a package), installed for this
/// machine as `velt check --locked` installs them.
fn package_graph(path: &Path) -> Result<Option<vpm::PackageGraph>, String> {
    let Some(root) = path.parent().and_then(vpm::manifest::find_package_root) else {
        return Ok(None);
    };
    let opts = vpm::InstallOptions {
        locked: true,
        update: false,
        target: Some(velt_codegen_cl::host_triple().to_string()),
    };
    vpm::install(&root, &vpm::Locations::from_env()?, opts).map(|i| Some(i.graph))
}

/// Compiles one program to verified VIR; `Some(message)` when it fails.
fn compile(path: &Path) -> Option<String> {
    if other_os_only(path) {
        return None;
    }
    let packages = match package_graph(path) {
        Ok(graph) => graph,
        Err(e) => return Some(format!("error: installing its package's dependencies: {e}")),
    };
    let opts = BuildOptions {
        input: path.to_path_buf(),
        emit: Emit::Vir,
        packages,
        ..Default::default()
    };
    let mut sess = Session::new();
    match catch_unwind(AssertUnwindSafe(|| driver::compile(&mut sess, &opts))) {
        Ok(Ok(vir)) => weak_without_weak_collections(path, &vir),
        Ok(Err(BuildError::Diagnostics)) => Some(sess.render_diagnostics()),
        Ok(Err(BuildError::Failed(msg))) => {
            Some(format!("{}\nerror: {msg}", sess.render_diagnostics()))
        }
        Ok(Err(BuildError::Ice(msg))) => Some(format!("internal compiler error: {msg}")),
        Err(_) => Some("the compiler panicked".to_string()),
    }
}

/// `Some(message)` when `vir` calls the weak runtime although its program never names a weak
/// collection.
fn weak_without_weak_collections(path: &Path, vir: &velt_vir::vir::Program) -> Option<String> {
    let src = std::fs::read_to_string(path).unwrap_or_default();
    if ["WeakMap", "WeakSet", "WeakRef"]
        .iter()
        .any(|n| src.contains(n))
    {
        return None;
    }
    let weak: Vec<&str> = vir
        .externs
        .iter()
        .map(|e| e.symbol.as_str())
        .filter(|s| s.starts_with("velt_rt_weak"))
        .collect();
    (!weak.is_empty()).then(|| {
        format!("weak-capable types in a program without weak collections (calls {weak:?})")
    })
}

#[test]
fn bench_programs_compile() {
    let root = &root();
    let all = programs(&root.join("bench"));
    assert!(!all.is_empty(), "no programs under bench/");
    let failures: Vec<String> = std::thread::scope(|scope| {
        let workers: Vec<_> = all
            .chunks(all.len().div_ceil(8))
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .filter_map(|p| {
                            let rel = p.strip_prefix(root).unwrap_or(p).display().to_string();
                            compile(p).map(|m| format!("{rel}:\n{m}"))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().expect("worker"))
            .collect()
    });
    println!("bench: {} programs checked", all.len());
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// The std modules under `dir` (`std/` paths without `.vlt`, as `velt/` names them), outside
/// the prelude.
fn std_modules(std: &Path, dir: &Path, out: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry").path();
        let rel = path
            .strip_prefix(std)
            .expect("under std/")
            .to_string_lossy();
        let rel = rel.replace('\\', "/");
        if path.is_dir() {
            if rel != "prelude" {
                std_modules(std, &path, out);
            }
        } else if let Some(module) = rel.strip_suffix(".vlt") {
            // A folder module's file is its folder: `cli/index.vlt` is `velt/cli`.
            out.push(module.strip_suffix("/index").unwrap_or(module).to_string());
        }
    }
}

/// `compile/check_all_std` (bench/nightly.sh) divides the instructions of `velt check
/// bench/compile/all_std.vlt` by the code lines of all of `std/`: a cost per line only while
/// that program loads every std module. A module it misses counts as lines nobody checks, and
/// importing it later from a checked module shows as a slowdown per line.
#[test]
fn all_std_imports_every_std_module() {
    let root = &root();
    let src = std::fs::read_to_string(root.join("bench/compile/all_std.vlt")).expect("all_std");
    let std = root.join("std");
    let mut modules = vec![];
    std_modules(&std, &std, &mut modules);
    modules.sort();
    assert!(!modules.is_empty(), "no modules under std/");
    let missing: Vec<String> = modules
        .into_iter()
        .filter(|m| !src.contains(&format!("from \"velt/{m}\";")))
        .map(|m| {
            format!(
                "import * as std_{} from \"velt/{m}\";",
                m.replace(['/', '-'], "_")
            )
        })
        .collect();
    assert!(
        missing.is_empty(),
        "bench/compile/all_std.vlt does not import every std module; add:\n{}",
        missing.join("\n")
    );
}

/// All of std type-checks (#1658): every program checks the std definitions it reaches only
/// (docs/internals/design/lazy-std.md), so `velt check` of all_std.vlt with laziness off
/// (`VELT_EAGER_STD=1`) is what checks every definition of std, the prelude's free functions
/// included. It must report nothing, not even a warning.
#[test]
fn all_of_std_checks_without_diagnostics() {
    let root = &root();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["check", "bench/compile/all_std.vlt"])
        .current_dir(root)
        .env("VELT_STD", root.join("std"))
        .env("VELT_EAGER_STD", "1")
        .output()
        .expect("run velt check");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stderr.trim().is_empty(),
        "velt check bench/compile/all_std.vlt with VELT_EAGER_STD=1:\n{stderr}"
    );
}
