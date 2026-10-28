//! Reference counts a borrow does not need: a typed array element write on a `const` receiver
//! (no `<receiver>` temporary, sema `accessor_rmw.rs`), and a borrow through a counted object
//! passed to a quiet callee (no stabilizing share, velt_vir `lower/quiet.rs`) or to one quiet for
//! borrows (it pushes onto an array and throws through a `never` helper), while a callee that may
//! replace what the borrow goes through still gets one. A call of a `never` function that throws
//! has no Ok path after it (`Function::cold`). In a release build, a local that only reads a
//! field's array keeps no count (velt_opt `borrowed_copies/counted`).

use std::path::Path;

use veltc::cli::Emit;
use veltc::driver::{self, BuildOptions, Session};

/// The debug VIR of `tests/borrow_counts/<name>.vlt` (no inlining: one function per function).
fn vir(name: &str) -> String {
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/borrow_counts")
        .join(format!("{name}.vlt"));
    let opts = BuildOptions {
        input,
        release: false,
        emit: Emit::Vir,
        ..Default::default()
    };
    let mut sess = Session::new();
    match driver::compile(&mut sess, &opts) {
        Ok(program) => program.to_string(),
        Err(e) => panic!("{e:?}\n{}", sess.render_diagnostics()),
    }
}

/// The release VIR of `tests/borrow_counts/<name>.vlt`.
fn release_vir(name: &str) -> String {
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/borrow_counts")
        .join(format!("{name}.vlt"));
    let opts = BuildOptions {
        input,
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

/// The text of the function whose symbol contains `name`.
fn function<'a>(vir: &'a str, name: &str) -> &'a str {
    let start = vir
        .match_indices("\nfn#")
        .map(|(i, _)| i + 1)
        .find(|&i| vir[i..].lines().next().is_some_and(|l| l.contains(name)))
        .unwrap_or_else(|| panic!("no function `{name}` in\n{vir}"));
    let end = vir[start + 1..]
        .find("\nfn#")
        .map_or(vir.len(), |e| start + 1 + e);
    &vir[start..end]
}

#[test]
fn const_typed_array_receiver_needs_no_temporary() {
    let vir = vir("receiver");
    assert!(!vir.contains("// <receiver>"), "{vir}");
}

#[test]
fn quiet_callees_get_borrows_without_counts() {
    let vir = vir("quiet");
    let quiet = function(&vir, "viaQuiet");
    assert!(
        !quiet.contains("_Gdrop_8u8_5b_5d") && !quiet.contains(", 1_u64\n"),
        "{quiet}"
    );
    let callback = function(&vir, "viaCallback");
    assert!(callback.contains("_Gdrop_8u8_5b_5d"), "{callback}");
}

#[test]
fn callees_quiet_for_borrows_get_borrows_without_counts() {
    let vir = vir("borrows");
    let quiet = function(&vir, "_V9viaDecode(");
    assert!(
        !quiet.contains("_Gdrop_8u8_5b_5d") && !quiet.contains(", 1_u64\n"),
        "{quiet}"
    );
    // The `never` helper is lent the borrow: it may run after the helper replaced it.
    let lending = function(&vir, "viaDecodeLending");
    assert!(lending.contains("_Gdrop_8u8_5b_5d"), "{lending}");
}

#[test]
fn a_throwing_never_call_has_no_ok_path() {
    let vir = vir("never");
    let check = function(&vir, "_V5check(");
    // The branch on the result's tag: the error goes on, the Ok side is unreachable.
    assert!(check.contains("unreachable"), "{check}");
}

#[test]
fn a_local_reading_a_field_array_keeps_no_count() {
    let vir = release_vir("field_copy");
    let sum = function(&vir, "ReaderM3sum(");
    // No count word (8 bytes before the array) is touched, and nothing is freed.
    assert!(
        !sum.contains("-8_i64") && !sum.contains("velt_rt_free"),
        "{sum}"
    );
}

#[test]
fn quiet_methods_get_borrowed_receivers() {
    let vir = vir("method");
    let at = function(&vir, "CursorM2at(");
    // `this.db` is lent to `offset` without a share and its release.
    assert!(!at.contains("_Gdrop_") && !at.contains("-8_i64"), "{at}");
}
