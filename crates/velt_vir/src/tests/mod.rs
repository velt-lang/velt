//! Lowering tests on hand-built HIR (builder.rs), shaped the way sema produces it. Programs are
//! lowered, verified, and executed by a VIR interpreter with an emulated runtime (interp/), which
//! checks outputs against the goldens and fails on leaks, double frees and backend traps.

mod builder;
mod builder_m2;
mod builder_m3;
mod builder_prelude;
mod control;
mod drops;
mod for_of_consume;
mod goldens;
mod hybrid;
mod interp;
mod keys;
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
mod m4_http;
mod m4_json;
mod native_init;
mod numeric;
mod param_attrs;
mod programs_http;
mod programs_m3;
mod programs_m4;
mod srclocs;
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

/// Expected stdout of an M2 golden program.
fn m2_golden(name: &str) -> String {
    golden_out("m2", name)
}

/// Expected stdout of an M3/M4 golden program.
fn m3_golden(name: &str) -> String {
    golden_out("m3", name)
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
