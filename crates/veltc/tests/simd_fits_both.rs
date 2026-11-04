//! `fits(a, j) && fits(b, j)` as one test (velt_opt `fits_pair`), on real code from both release
//! backends: `tests/golden/lang/simd_fits_pair.vlt` compares the result with the arithmetic
//! written out on lengths and indexes at the edges. The goldens run it with the backend a release
//! build picks; this test runs it with each, and checks the conditions did become one test, so
//! the backends' `fits_both` is what those runs compare.

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

fn golden(root: &Path) -> PathBuf {
    root.join("tests/golden/lang/simd_fits_pair.vlt")
}

#[test]
fn release_builds_test_the_pair_once() {
    let opts = BuildOptions {
        input: golden(&root()),
        release: true,
        emit: Emit::Vir,
        ..Default::default()
    };
    let mut sess = Session::new();
    let vir = match driver::compile(&mut sess, &opts) {
        Ok(program) => program.to_string(),
        Err(e) => panic!("{e:?}\n{}", sess.render_diagnostics()),
    };
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

fn runs_as_the_golden_says(backend: &str) {
    let root = root();
    runtime_support::build_native_runtime(&root);
    let work = work_dir::work_dir(&root, "golden-work-fits-both").join(backend);
    std::fs::create_dir_all(&work).expect("work dir");
    let f = golden(&root);
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
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(got, want, "with {backend}");
}

#[test]
fn llvm_agrees_at_the_edges() {
    if !velt_codegen_llvm::available() {
        eprintln!("note: clang not available; skipping the LLVM run");
        return;
    }
    runs_as_the_golden_says("llvm");
}

#[test]
fn cranelift_agrees_at_the_edges() {
    runs_as_the_golden_says("cranelift");
}
