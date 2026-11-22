//! The cost of a function's variable scopes when its code is cut into many stretches, as a
//! function of many loops is: counted (tree nodes looked at), not timed.

use velt_vir::vir::{DebugTyId, LocalDebug, SrcLoc, Ty};

use super::super::{scope_ranges, FunctionLines, ScopeCode, Variable};

/// `n` loops in a row, each laid out body first (line `2k + 11`), then its header (line
/// `2k + 10`, earlier: a new stretch), with a counter declared on the header's line whose
/// block ends on the body's.
fn loops(n: u32) -> FunctionLines {
    let loc = |line, col| SrcLoc { file: 0, line, col };
    let mut rows = vec![(0, Some(loc(1, 1)))];
    for k in 0..n {
        rows.push((4 + 20 * k, Some(loc(2 * k + 11, 5))));
        rows.push((14 + 20 * k, Some(loc(2 * k + 10, 5))));
    }
    FunctionLines {
        id: cranelift_module::FuncId::from_u32(0),
        symbol: "_V4main".into(),
        name: "main".into(),
        external: false,
        size: 4 + 20 * n,
        decl: Some(loc(1, 1)),
        rows,
        body: 4,
        vars: (0..n)
            .map(|k| Variable {
                name: format!("i{k}"),
                debug: LocalDebug {
                    decl: loc(2 * k + 10, 10),
                    end: Some(loc(2 * k + 11, 40)),
                    ty: DebugTyId(0),
                    by_ref: false,
                    param: false,
                },
                ty: Ty::I64,
                fp_offset: -8 * (i64::from(k) + 1),
            })
            .collect(),
    }
}

/// The tree nodes the scopes of `f`'s variables looked at, checking each scope is one range.
fn looked(f: &FunctionLines) -> u64 {
    let code = ScopeCode::new(f);
    for v in &f.vars {
        let ranges = scope_ranges(&code, &v.debug).expect("a scope of its own");
        assert_eq!(ranges.len(), 1, "{}: {ranges:?}", v.name);
    }
    code.index.looked.get()
}

#[test]
fn scopes_in_a_function_of_many_loops_cost_linear_work() {
    let (small, large) = (looked(&loops(500)), looked(&loops(4000)));
    // A look at every stretch per variable costs the square of the number of loops; a query
    // of the index costs about log2 of the stretches, plus those it finds.
    assert!(
        large <= small * 12,
        "8 times the loops cost {large} against {small}: more than linear"
    );
}
