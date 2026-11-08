//! Lowering tests on hand-built HIR (builder.rs), shaped the way sema produces it. Programs are
//! lowered, verified, and executed by a VIR interpreter with an emulated runtime (interp/), which
//! checks outputs against the goldens and fails on leaks, double frees and backend traps.

mod builder;
mod builder_m2;
mod builder_m3;
mod builder_prelude;
mod c_wasi;
mod const_arrays;
mod control;
mod cpu_checks;
mod drop_chain;
mod drops;
mod fingerprint;
mod for_of_consume;
mod frame_envs;
mod goldens;
mod hook_demand;
mod hybrid;
mod interp;
mod keys;
mod live_superset;
mod lock_walks;
mod m2_arrays;
mod m2_classes;
mod m2_closures;
mod m2_edges;
mod m2_enums;
mod m2_errors;
mod m2_generics;
mod m2_values;
mod m3_async;
mod m3_edges;
mod m3_io;
mod m3_state;
mod m3_tasks;
mod native_init;
mod numeric;
mod param_attrs;
mod programs_m3;
mod shape_dicts;
mod shape_guard;
mod srclocs;
mod std_src;
mod verifier;
mod vir_ext;

use velt_sema::hir;

use crate::vir;

/// Lower and verify; panics with the verifier errors and the VIR dump on failure.
fn lower_ok(p: &hir::Program) -> vir::Program {
    let v = crate::lower(p);
    if let Err(errs) = crate::verify(&v) {
        panic!("verify failed:\n{}\n\n{v}", errs.join("\n"));
    }
    v
}

/// Lower, verify and interpret. Asserts no leaks on normal exit.
fn run(p: &hir::Program) -> interp::Outcome {
    let v = lower_ok(p);
    let out = interp::run(&v);
    assert_eq!(out.live_allocs, 0, "leaked heap allocations\n{v}");
    out
}

/// `src` (Velt, loaded with the real std: std_src.rs) lowered and verified.
fn lower_src(src: &str) -> vir::Program {
    lower_ok(&std_src::check_src(src))
}

/// The function of `v` that the root module's top-level function `name` lowered to (its symbol
/// is `_V<len><name>`).
fn user_fn<'a>(v: &'a vir::Program, name: &str) -> &'a vir::Function {
    let sym = format!("_V{}{name}", name.len());
    v.funcs
        .iter()
        .find(|f| f.symbol == sym)
        .unwrap_or_else(|| panic!("no function `{sym}` in:\n{v}"))
}

/// `src` (Velt, loaded with the real std: std_src.rs) lowered, verified and interpreted. Asserts
/// no leaks on normal exit.
fn run_src(src: &str) -> interp::Outcome {
    let v = lower_src(src);
    let out = interp::run(&v);
    assert_eq!(out.live_allocs, 0, "leaked heap allocations\n{v}");
    out
}

/// Expected stdout of an M2 golden program.
fn m2_golden(name: &str) -> String {
    golden_out("m2", name)
}

/// Expected stdout of an M3/M4 golden program.
fn m3_golden(name: &str) -> String {
    golden_out("m3", name)
}

/// The source of golden program `tests/golden/<dir>/<name>.velt`, for [`run_src`].
fn golden_src(dir: &str, name: &str) -> String {
    let path = format!(
        "{}/../../tests/golden/{dir}/{name}.velt",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn golden_out(dir: &str, name: &str) -> String {
    let path = format!(
        "{}/../../tests/golden/{dir}/{name}.out",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .replace("\r\n", "\n")
}
