//! Fingerprints (crate::fingerprint): the same under renumbered ids, different when the code
//! would be; link mode counts what hot reload leaves out.

use crate::fingerprint::{attrs_hold, Canon, Fingerprint, Mode};
use crate::vir::*;

/// What a test program varies.
#[derive(Clone)]
struct Variant {
    /// Lists functions, aggregates and statics in another order (other ids, same program).
    reordered: bool,
    /// A constant in `f`'s code, as a class id would be.
    constant: i128,
    /// The offset of `Point`'s second field.
    offset: u32,
    /// The function `f` calls.
    callee: &'static str,
    /// The size of `f`'s async state aggregate.
    state_size: u32,
    /// The location `f` records for an uncaught error.
    throw_loc: &'static [u8],
    /// `f`'s parameter attributes.
    noalias: bool,
    /// The bytes of a static at the far end of a cycle of statics `f` uses.
    far: &'static [u8],
    /// Whether `velt_rt_panic`, which `f` calls and names in a static, is declared `noreturn`.
    noreturn: bool,
    /// `f`'s gated target.
    target: Option<crate::simd::Ext>,
}

const BASE: Variant = Variant {
    reordered: false,
    constant: 42,
    offset: 8,
    callee: "_V1g",
    state_size: 24,
    throw_loc: b" at std/a.vlt:1:1",
    noalias: false,
    far: b"far",
    noreturn: true,
    target: None,
};

/// `f(p: Ptr)`: a local of its state aggregate (holding a `Point`), a read of `Point`'s second
/// field through `p`, `velt_rt_set_throw_loc(loc)`, `g(constant)`, the address of a static in a
/// cycle (`a` → `b` → `a`, `b` also pointing at `velt_rt_panic`) and a call to `velt_rt_panic`;
/// plus `g` and `h`. Returns `f`'s fingerprint.
fn f_fingerprint(v: &Variant, mode: Mode) -> Fingerprint {
    let mut aggs = vec![
        AggLayout {
            name: "string".into(),
            size: 24,
            align: 8,
            fields: vec![(Ty::U64, 0), (Ty::U64, 8), (Ty::U64, 16)],
        },
        AggLayout {
            name: "Point".into(),
            size: v.offset + 8,
            align: 8,
            fields: vec![(Ty::I64, 0), (Ty::I64, v.offset)],
        },
        AggLayout {
            name: "f state".into(),
            size: v.state_size,
            align: 8,
            fields: vec![(Ty::Agg(AggId(1)), 0)],
        },
    ];
    let (point, state) = if v.reordered {
        aggs.swap(1, 2);
        aggs[1].fields = vec![(Ty::Agg(AggId(2)), 0)];
        (AggId(2), AggId(1))
    } else {
        (AggId(1), AggId(2))
    };
    // Statics: the throw location, `a` (→ `b`), `b` (→ `a`, the far bytes).
    let (loc, a, b) = if v.reordered {
        (StaticId(2), StaticId(1), StaticId(0))
    } else {
        (StaticId(0), StaticId(1), StaticId(2))
    };
    let stat = |bytes: &[u8], relocs| StaticData {
        bytes: bytes.to_vec(),
        align: 8,
        relocs,
        mutable: None,
    };
    let mut statics = vec![
        stat(v.throw_loc, vec![]),
        stat(&[0; 8], vec![(0, Const::Static(b))]),
        {
            let mut far = vec![0; 8];
            far.extend_from_slice(v.far);
            stat(
                &far,
                vec![(0, Const::Static(a)), (8, Const::Extern(ExternId(1)))],
            )
        },
    ];
    if v.reordered {
        statics.reverse();
    }
    let externs = vec![
        ExternFn {
            symbol: "velt_rt_set_throw_loc".into(),
            params: vec![Ty::Ptr],
            ret: Ty::Unit,
            noreturn: false,
        },
        ExternFn {
            symbol: "velt_rt_panic".into(),
            params: vec![],
            ret: Ty::Unit,
            noreturn: v.noreturn,
        },
    ];
    let leaf = |symbol: &str| {
        let mut g = Function::new(symbol.into(), vec![Ty::I64], Ty::I64, Linkage::Internal);
        g.blocks.push(BasicBlock {
            stmts: vec![],
            term: Terminator::Return(Operand::Copy(Place::local(Local(0)))),
        });
        g
    };
    let (g, h) = (leaf("_V1g"), leaf("_V1h"));
    let ids = if v.reordered { [2, 1, 0] } else { [0, 1, 2] };
    let callee = FuncId(ids[if v.callee == "_V1g" { 0 } else { 1 }]);
    let mut f = Function::new("_V1f".into(), vec![Ty::Ptr], Ty::I64, Linkage::Internal);
    f.locals.push(LocalDecl::new(Ty::Agg(state), None));
    f.locals.push(LocalDecl::new(Ty::I64, None));
    f.locals.push(LocalDecl::new(Ty::Ptr, None));
    f.target = v.target;
    if v.noalias {
        f.param_attrs = vec![ParamAttrs {
            noalias: true,
            ..ParamAttrs::default()
        }];
    }
    let field = Place {
        local: Local(0),
        proj: vec![Proj::Deref(Ty::Agg(point)), Proj::Field(1)],
    };
    f.blocks = vec![
        BasicBlock {
            stmts: vec![
                Stmt::Assign(Place::local(Local(2)), Rvalue::Use(Operand::Copy(field))),
                Stmt::Assign(
                    Place::local(Local(3)),
                    Rvalue::Use(Operand::Const(Const::Static(a), Ty::Ptr)),
                ),
            ],
            term: Terminator::Call {
                callee: Callee::Extern(ExternId(0)),
                args: vec![Operand::Const(Const::Static(loc), Ty::Ptr)],
                dest: None,
                next: BlockId(1),
            },
        },
        BasicBlock {
            stmts: vec![],
            term: Terminator::Call {
                callee: Callee::Func(callee),
                args: vec![Operand::Const(Const::Int(v.constant), Ty::I64)],
                dest: Some(Place::local(Local(2))),
                next: BlockId(2),
            },
        },
        BasicBlock {
            stmts: vec![],
            term: Terminator::Call {
                callee: Callee::Extern(ExternId(1)),
                args: vec![],
                dest: None,
                next: BlockId(3),
            },
        },
        BasicBlock {
            stmts: vec![],
            term: Terminator::Return(Operand::Copy(Place::local(Local(2)))),
        },
    ];
    let mut funcs = vec![g, h, f];
    if v.reordered {
        funcs.reverse();
    }
    let program = Program {
        aggs,
        funcs,
        externs,
        statics,
        ..Program::default()
    };
    let f = program.funcs.iter().find(|f| f.symbol == "_V1f").unwrap();
    Canon::new(&program, mode).function(f)
}

fn same(v: &Variant, mode: Mode) -> bool {
    f_fingerprint(&BASE, mode) == f_fingerprint(v, mode)
}

#[test]
fn renumbered_ids_change_nothing() {
    let v = Variant {
        reordered: true,
        ..BASE
    };
    assert!(same(&v, Mode::HotReload) && same(&v, Mode::Link));
}

#[test]
fn constants_layouts_and_callees_count_in_both_modes() {
    let changes = [
        Variant {
            constant: 43,
            ..BASE
        },
        Variant { offset: 16, ..BASE },
        Variant {
            callee: "_V1h",
            ..BASE
        },
        Variant {
            far: b"FAR",
            ..BASE
        },
    ];
    for (i, v) in changes.iter().enumerate() {
        assert!(!same(v, Mode::HotReload), "change {i}, hot reload");
        assert!(!same(v, Mode::Link), "change {i}, link");
    }
}

/// Hot reload leaves these out on purpose (module docs); code compiled into another object
/// would be wrong for a program where they differ.
#[test]
fn link_mode_counts_what_hot_reload_leaves_out() {
    let changes = [
        Variant {
            state_size: 32,
            ..BASE
        },
        Variant {
            throw_loc: b" at std/a.vlt:2:1",
            ..BASE
        },
        Variant {
            noreturn: false,
            ..BASE
        },
        Variant {
            target: Some(crate::simd::Ext::ShaX86),
            ..BASE
        },
    ];
    for (i, v) in changes.iter().enumerate() {
        assert!(same(v, Mode::HotReload), "change {i}, hot reload");
        assert!(!same(v, Mode::Link), "change {i}, link");
    }
}

/// Parameter attributes are not part of the fingerprint (they depend on the whole program) but
/// must cover what the compiled code assumed.
#[test]
fn param_attributes_must_cover_what_the_code_assumed() {
    let v = Variant {
        noalias: true,
        ..BASE
    };
    assert!(same(&v, Mode::Link));
    let f = |attrs: ParamAttrs| {
        let mut f = Function::new("f".into(), vec![Ty::Ptr], Ty::Unit, Linkage::Internal);
        f.param_attrs = vec![attrs];
        f
    };
    let weak = f(ParamAttrs {
        nonnull: true,
        dereferenceable: 24,
        ..ParamAttrs::default()
    });
    let strong = f(ParamAttrs {
        readonly: true,
        frozen: true,
        nonnull: true,
        dereferenceable: 24,
        ..ParamAttrs::default()
    });
    assert!(
        attrs_hold(&weak, &strong),
        "assumed less than callers uphold"
    );
    assert!(attrs_hold(&strong, &strong));
    assert!(
        !attrs_hold(&strong, &weak),
        "assumed `frozen`, which callers here don't uphold"
    );
    let none = Function::new("f".into(), vec![Ty::Ptr], Ty::Unit, Linkage::Internal);
    assert!(attrs_hold(&none, &weak) && !attrs_hold(&weak, &none));
}

/// Epoch's shape: a static atomic (`mutable`) whose address one function takes, a table another
/// reaches through two relocations, and a function that reaches only read-only statics.
#[test]
fn reaches_mutable_static_through_operands_and_relocations() {
    let stat = |relocs, mutable| StaticData {
        bytes: vec![0; 8],
        align: 8,
        relocs,
        mutable,
    };
    let statics = vec![
        stat(vec![], Some(0xe90c)),
        stat(vec![(0, Const::Static(StaticId(0)))], None),
        stat(vec![(0, Const::Static(StaticId(1)))], None),
        stat(vec![(0, Const::Static(StaticId(3)))], None),
    ];
    let uses = |symbol: &str, s: u32| {
        let mut f = Function::new(symbol.into(), vec![], Ty::Ptr, Linkage::Internal);
        f.blocks.push(BasicBlock {
            stmts: vec![],
            term: Terminator::Return(Operand::Const(Const::Static(StaticId(s)), Ty::Ptr)),
        });
        f
    };
    let program = Program {
        funcs: vec![uses("direct", 0), uses("chain", 2), uses("other", 3)],
        statics,
        ..Program::default()
    };
    let canon = Canon::new(&program, Mode::Link);
    let reaches: Vec<bool> = program
        .funcs
        .iter()
        .map(|f| canon.reaches_mutable_static(f))
        .collect();
    assert_eq!(reaches, [true, true, false]);
}
