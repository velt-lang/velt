//! `fits(a, j) && fits(b, j)` as one test (velt_opt `fits_pair`), on real code from both release
//! backends: `tests/golden/lang/simd_fits_pair.vlt` compares the result with the arithmetic
//! written out on lengths and indexes at the edges. The goldens run it with the backend a release
//! build picks; this test runs it with each, and checks the conditions did become one test, so
//! the backends' `fits_both` is what those runs compare. `simd_fits_pair_null_*` are the second
//! array being null where the first test fails.

use std::path::{Path, PathBuf};

use veltc::cli::Emit;
use veltc::driver::{self, BuildOptions, Session};

mod no_window;
mod runtime_support;
mod work_dir;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn golden(root: &Path, name: &str) -> PathBuf {
    root.join("tests/golden/lang")
        .join(name)
        .with_extension("vlt")
}

/// A null second array where the first test fails, one way of reaching a field for each: without
/// the pass's null test these segfault in release with either backend, and with it print what the
/// two tests print.
const NULL_SECOND: &[&str] = &[
    "simd_fits_pair_null_override",
    "simd_fits_pair_null_ctor",
    "simd_fits_pair_null_method",
    "simd_fits_pair_null_super",
    "simd_fits_pair_null_direct",
];

fn release_vir(name: &str) -> String {
    let opts = BuildOptions {
        input: golden(&root(), name),
        release: true,
        emit: Emit::Vir,
        ..Default::default()
    };
    let mut sess = Session::new();
    match driver::compile(&mut sess, &opts) {
        Ok(program) => program.to_string(),
        Err(e) => panic!("{e:?}\n{}", sess.render_diagnostics()),
    }
}

#[test]
fn release_builds_test_the_pair_once() {
    let vir = release_vir("simd_fits_pair");
    let calls = |lanes: &str| {
        let call = format!(" velt_simd_{lanes}_fits_both(");
        vir.lines()
            .filter(|l| l.contains(&call) && l.contains(" call "))
            .count()
    };
    // `j`, `j + 48`, the loop's condition, and the two in `others`: fields once the object is
    // locals, and an array given away on the first test's failing path.
    assert_eq!(calls("i8x16"), 5, "{vir}");
    assert_eq!(calls("i16x8"), 0, "two lane types are two tests:\n{vir}");
    for lanes in ["u16x8", "f32x4", "f64x2"] {
        assert!(calls(lanes) >= 1, "{lanes} in\n{vir}");
    }
}

#[test]
fn the_null_shapes_are_pairs_on_params_that_promise_nothing() {
    for &name in NULL_SECOND {
        let vir = release_vir(name);
        let holding: Vec<&str> = (vir.split("\nfn#"))
            .filter(|f| f.contains("_fits_both(") && f.contains(" call "))
            .collect();
        assert!(!holding.is_empty(), "no pair in {name}:\n{vir}");
        // LLVM would remove the null test on a param that says `nonnull`. Boxed arrays' don't.
        for f in holding {
            assert!(!f.contains("nonnull"), "{name}:\n{f}");
        }
    }
}

fn runs_as_the_golden_says(backend: &str, name: &str) {
    let root = root();
    runtime_support::build_native_runtime(&root);
    let work = work_dir::work_dir(&root, "golden-work-fits-both").join(backend);
    std::fs::create_dir_all(&work).expect("work dir");
    let f = golden(&root, name);
    let want = std::fs::read_to_string(f.with_extension("out"))
        .expect(".out")
        .replace("\r\n", "\n");
    let o = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["run", "--release", "--backend", backend])
        .arg(&f)
        .current_dir(&work)
        .output()
        .expect("run velt");
    let got = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    assert!(
        o.status.success(),
        "{name}: {:?}\n{}",
        o.status,
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(got, want, "{name} with {backend}");
}

#[test]
fn llvm_agrees_at_the_edges() {
    if !velt_codegen_llvm::available() {
        eprintln!("note: clang not available; skipping the LLVM run");
        return;
    }
    runs_as_the_golden_says("llvm", "simd_fits_pair");
}

#[test]
fn cranelift_agrees_at_the_edges() {
    runs_as_the_golden_says("cranelift", "simd_fits_pair");
}

/// `--release -g` keeps source locations through the optimizer, so the block the pass adds for the
/// null test has to keep `Function::locs` in step: `simplify_cfg` reorders both.
fn builds_with_locations(backend: &str, name: &str) {
    let root = root();
    runtime_support::build_native_runtime(&root);
    let work = work_dir::work_dir(&root, "golden-work-fits-both-g").join(backend);
    std::fs::create_dir_all(&work).expect("work dir");
    let f = golden(&root, name);
    let exe = work.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let o = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "--release", "-g", "--backend", backend, "-o"])
        .arg(&exe)
        .arg(&f)
        .current_dir(&work)
        .output()
        .expect("run velt");
    assert!(
        o.status.success(),
        "{name} with {backend}: {:?}\n{}",
        o.status,
        String::from_utf8_lossy(&o.stderr)
    );
    let ran = crate::no_window::command(&exe)
        .current_dir(&work)
        .output()
        .expect("run the program");
    let want = std::fs::read_to_string(f.with_extension("out"))
        .expect(".out")
        .replace("\r\n", "\n");
    let got = String::from_utf8_lossy(&ran.stdout).replace("\r\n", "\n");
    assert_eq!(got, want, "{name} with {backend}, built with -g");
}

#[test]
fn llvm_builds_the_pairs_with_locations() {
    if !velt_codegen_llvm::available() {
        eprintln!("note: clang not available; skipping the LLVM run");
        return;
    }
    builds_with_locations("llvm", "simd_fits_pair");
    for &name in NULL_SECOND {
        builds_with_locations("llvm", name);
    }
}

#[test]
fn cranelift_builds_the_pairs_with_locations() {
    builds_with_locations("cranelift", "simd_fits_pair");
    for &name in NULL_SECOND {
        builds_with_locations("cranelift", name);
    }
}

#[test]
fn llvm_reads_no_null_second_array() {
    if !velt_codegen_llvm::available() {
        eprintln!("note: clang not available; skipping the LLVM run");
        return;
    }
    for &name in NULL_SECOND {
        runs_as_the_golden_says("llvm", name);
    }
}

#[test]
fn cranelift_reads_no_null_second_array() {
    for &name in NULL_SECOND {
        runs_as_the_golden_says("cranelift", name);
    }
}
