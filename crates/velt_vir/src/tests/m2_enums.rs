//! Enums and matches: `tests/golden/m2/enums_switch.vlt` (payload enums, C-like enums with
//! explicit discriminants, matches with or-patterns / guards / by-reference bindings), loaded
//! from source with the real std since it prints numbers through std's hooks (#2197), and
//! hand-lowered HIR for the rest.

use velt_sema::hir::{Def, UseMode as U};

use super::builder::*;
use super::builder_m2::*;
use super::{m2_golden, run};

/// The golden program prints numbers through std's hooks (#2197): loaded from source with the
/// real std (std_src.rs).
#[test]
fn golden_enums_switch() {
    let out = super::run_src(&super::golden_src("m2", "enums_switch"));
    assert_eq!(out.stdout, m2_golden("enums_switch"));
}

/// An owned scrutinee: the arm moves the string payload out, the rest is dropped per arm.
#[test]
fn match_moves_payload_out_of_owned_scrutinee() {
    let mut pb = PB::new();
    let t = pb.t;
    let tok_d = pb.add_def(Def::Enum(enum_def(
        "Tok",
        vec![("Pair", vec![t.str, t.str], 0), ("None", vec![], 1)],
    )));
    let tok = pb.adt_ty(tok_d, vec![]);
    let mut f = FB::new("main", t.unit);
    let x = f.local("x", tok);
    let a = f.local("a", t.str);
    let out = f.local("out", t.str);
    let body = vec![
        let_(
            x,
            variant(
                tok_d,
                0,
                vec![
                    concat(s("a", t), s("1", t), t),
                    concat(s("b", t), s("2", t), t),
                ],
                tok,
            ),
        ),
        let_(
            out,
            match_(
                f.mv(x),
                vec![
                    (
                        pvariant(tok_d, 0, vec![pbind(a, U::Move, t.str), pwild(t.str)], tok),
                        None,
                        f.mv(a),
                    ),
                    (pwild(tok), None, s("none", t)),
                ],
                t.str,
            ),
        ),
        se(print(vec![f.bw(out)], t)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "a1\n");
}
