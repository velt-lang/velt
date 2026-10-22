//! Reference counts a borrow does not need: a typed array element write on a `const` receiver
//! (no `<receiver>` temporary, sema `accessor_rmw.rs`), and a borrow through a counted object
//! passed to a quiet callee (no stabilizing share, velt_vir `lower/quiet.rs`), while a callee
//! that may replace what the borrow goes through still gets one.

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
