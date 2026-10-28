//! Unit tests for borrowed counted pointers on hand-built VIR shaped like
//! `const data = this.data; …; return data.length` with lowering's increment and release.

use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{AggId, BasicBlock, ExternId, FuncId};

/// What runs between the increment and the read of `data.length` (or, for the `After…`
/// variants, between that read and the release).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Between {
    Nothing,
    /// A runtime function that may free anything, before the read.
    UnknownExtern,
    /// The same after the last read.
    UnknownExternAfter,
    /// A Velt function that frees (it calls `velt_rt_free`), before the read.
    FreeingCall,
    /// A Velt function that only reads, handed `data`.
    QuietCallWithData,
    /// The freeing function handed `data` after the last read: it may free it, then read it.
    FreeingCallWithDataAfter,
    /// `data` stored into the object.
    Escapes,
    /// A return before the release: the reference went somewhere else.
    ReturnsWithout,
    /// A second increment of `data`'s count.
    SharedAgain,
    /// An allocation.
    Alloc,
    /// A call that never returns (the bounds check's report).
    NoReturn,
    /// A Velt function that frees and never returns, on a path of its own, not handed `data`:
    /// nothing uses `data` after it.
    NoReturnFreeing,
    /// The same handed `data`: it runs with `data` and may free it first.
    NoReturnWithData,
    /// A call through a function pointer (a closure), before the read.
    ClosureCall,
    /// A string runtime function (an error message built): frees no counted object.
    StringExtern,
}

struct Kit {
    pb: ProgramBuilder,
    obj: AggId,
    arr: AggId,
    free: ExternId,
    other: ExternId,
    alloc: ExternId,
    panic: ExternId,
    concat: ExternId,
    quiet: FuncId,
    freeing: FuncId,
    stuck: FuncId,
}

fn kit() -> Kit {
    let mut pb = ProgramBuilder::new();
    let arr = pb.agg("T[]", 24, 8, &[(Ty::Ptr, 0), (Ty::U64, 8), (Ty::U64, 16)]);
    let obj = pb.agg("Db object", 8, 8, &[(Ty::Ptr, 0)]);
    let free = pb.ext(
        "velt_rt_free",
        &[Ty::Ptr, Ty::U64, Ty::U64],
        Ty::Unit,
        false,
    );
    let other = pb.ext("velt_rt_str_append", &[Ty::Ptr, Ty::Ptr], Ty::Unit, false);
    let alloc = pb.ext("velt_rt_alloc", &[Ty::U64, Ty::U64], Ty::Ptr, false);
    let panic = pb.ext("velt_rt_panic", &[Ty::Ptr], Ty::Unit, true);
    let concat = pb.ext(
        "velt_rt_str_concat",
        &[Ty::Ptr, Ty::Ptr, Ty::Ptr],
        Ty::Unit,
        false,
    );
    // quiet(a) = a.len
    let mut fb = FuncBuilder::internal("quiet", &[Ty::Ptr], Ty::U64);
    let (a, b) = (fb.param(0), fb.block());
    fb.ret(b, copy_place(field_of(a, arr, 1)));
    let quiet = pb.add(fb.finish());
    // freeing(a) { free(a, 24, 8) }
    let mut fb = FuncBuilder::internal("freeing", &[Ty::Ptr], Ty::Unit);
    let (a, b) = (fb.param(0), fb.block());
    let n = fb.call(
        b,
        Callee::Extern(free),
        vec![copy_local(a), int(24, Ty::U64), int(8, Ty::U64)],
        None,
    );
    fb.ret(n, Operand::Const(Const::Unit, Ty::Unit));
    let freeing = pb.add(fb.finish());
    // stuck(a) { freeing(a); for (;;) {} }: no path returns.
    let mut fb = FuncBuilder::internal("stuck", &[Ty::Ptr], Ty::Unit);
    let (a, b) = (fb.param(0), fb.block());
    let n = fb.call(b, Callee::Func(freeing), vec![copy_local(a)], None);
    fb.goto(n, n);
    let stuck = pb.add(fb.finish());
    Kit {
        pb,
        obj,
        arr,
        free,
        other,
        alloc,
        panic,
        concat,
        quiet,
        freeing,
        stuck,
    }
}

/// `(*p as agg).n`
fn field_of(p: Local, agg: AggId, n: u32) -> Place {
    Place {
        local: p,
        proj: vec![Proj::Deref(Ty::Agg(agg)), Proj::Field(n)],
    }
}

/// The count word behind `q`.
fn count(q: Local) -> Place {
    Place {
        local: q,
        proj: vec![Proj::Deref(Ty::U64)],
    }
}

/// `f(this) { v = null; r = this.data; r.count++; v = r; …; x = r.len; …; release v; return x }`
fn program(between: Between) -> (Program, FuncId) {
    let mut k = kit();
    let mut fb = FuncBuilder::export("f", &[Ty::Ptr], Ty::U64);
    let this = fb.param(0);
    let [v, r, q, n, x, v2, c0, q2, cnt, c1, m, tmp] = [
        Ty::Ptr,
        Ty::Ptr,
        Ty::Ptr,
        Ty::U64,
        Ty::U64,
        Ty::Ptr,
        Ty::Bool,
        Ty::Ptr,
        Ty::U64,
        Ty::Bool,
        Ty::U64,
        Ty::U64,
    ]
    .map(|t| fb.local(t));
    let entry = fb.block();
    fb.assign(entry, v, Rvalue::Use(int(0, Ty::Ptr)));
    fb.assign(entry, r, Rvalue::Use(copy_place(field_of(this, k.obj, 0))));
    fb.assign(
        entry,
        q,
        bin(BinOp::PtrAdd, copy_local(r), int(-8, Ty::I64)),
    );
    fb.assign(
        entry,
        n,
        bin(BinOp::Add, copy_place(count(q)), int(1, Ty::U64)),
    );
    fb.push(entry, Stmt::Assign(count(q), Rvalue::Use(copy_local(n))));
    fb.assign(entry, v, Rvalue::Use(copy_local(r)));
    let call = |fb: &mut FuncBuilder, b, callee, args| fb.call(b, callee, args, None);
    let mut b = entry;
    match between {
        Between::UnknownExtern => {
            b = call(
                &mut fb,
                b,
                Callee::Extern(k.other),
                vec![copy_local(this), copy_local(this)],
            )
        }
        Between::FreeingCall => {
            b = call(&mut fb, b, Callee::Func(k.freeing), vec![copy_local(this)])
        }
        Between::StringExtern => {
            let args = vec![copy_local(this), copy_local(this), copy_local(this)];
            b = call(&mut fb, b, Callee::Extern(k.concat), args)
        }
        Between::QuietCallWithData => {
            b = fb.call(b, Callee::Func(k.quiet), vec![copy_local(v)], Some(tmp))
        }
        Between::Escapes => fb.push(
            b,
            Stmt::Assign(field_of(this, k.obj, 0), Rvalue::Use(copy_local(v))),
        ),
        Between::SharedAgain => {
            let (q3, n3) = (fb.local(Ty::Ptr), fb.local(Ty::U64));
            fb.assign(b, q3, bin(BinOp::PtrAdd, copy_local(v), int(-8, Ty::I64)));
            fb.assign(
                b,
                n3,
                bin(BinOp::Add, copy_place(count(q3)), int(1, Ty::U64)),
            );
            fb.push(b, Stmt::Assign(count(q3), Rvalue::Use(copy_local(n3))));
        }
        Between::Alloc => {
            let fresh = fb.local(Ty::Ptr);
            b = fb.call(
                b,
                Callee::Extern(k.alloc),
                vec![int(8, Ty::U64), int(8, Ty::U64)],
                Some(fresh),
            )
        }
        Between::NoReturn => {
            let (bad, report, dead, ok) = (fb.local(Ty::Bool), fb.block(), fb.block(), fb.block());
            fb.assign(b, bad, bin(BinOp::Eq, copy_local(this), int(0, Ty::Ptr)));
            fb.branch(b, bad, report, ok);
            let after = call(
                &mut fb,
                report,
                Callee::Extern(k.panic),
                vec![copy_local(this)],
            );
            fb.term(after, Terminator::Unreachable);
            fb.term(dead, Terminator::Unreachable);
            b = ok;
        }
        Between::NoReturnFreeing | Between::NoReturnWithData => {
            let (bad, report, ok) = (fb.local(Ty::Bool), fb.block(), fb.block());
            fb.assign(b, bad, bin(BinOp::Eq, copy_local(this), int(0, Ty::Ptr)));
            fb.branch(b, bad, report, ok);
            let arg = match between {
                Between::NoReturnWithData => v,
                _ => this,
            };
            let after = call(
                &mut fb,
                report,
                Callee::Func(k.stuck),
                vec![copy_local(arg)],
            );
            fb.term(after, Terminator::Unreachable);
            b = ok;
        }
        Between::ClosureCall => {
            let target = Callee::Ptr {
                target: copy_local(this),
                params: vec![Ty::Ptr],
                ret: Ty::Unit,
            };
            b = call(&mut fb, b, target, vec![copy_local(this)])
        }
        _ => {}
    }
    fb.assign(b, x, Rvalue::Use(copy_place(field_of(r, k.arr, 1))));
    match between {
        Between::UnknownExternAfter => {
            b = call(
                &mut fb,
                b,
                Callee::Extern(k.other),
                vec![copy_local(this), copy_local(this)],
            )
        }
        Between::FreeingCallWithDataAfter => {
            b = call(&mut fb, b, Callee::Func(k.freeing), vec![copy_local(v)])
        }
        Between::ReturnsWithout => {
            let (c, early, on) = (fb.local(Ty::Bool), fb.block(), fb.block());
            fb.assign(b, c, bin(BinOp::Eq, copy_local(x), int(0, Ty::U64)));
            fb.branch(b, c, early, on);
            fb.ret(early, copy_local(x));
            b = on;
        }
        _ => {}
    }
    // The release: `if (v != null) { if (count == 1) free… else count-- }`.
    let [test, last, dec, exit] = [fb.block(), fb.block(), fb.block(), fb.block()];
    fb.assign(b, v2, Rvalue::Use(copy_local(v)));
    fb.assign(b, c0, bin(BinOp::Ne, copy_local(v), int(0, Ty::Ptr)));
    fb.branch(b, c0, test, exit);
    fb.assign(
        test,
        q2,
        bin(BinOp::PtrAdd, copy_local(v2), int(-8, Ty::I64)),
    );
    fb.assign(test, cnt, Rvalue::Use(copy_place(count(q2))));
    fb.assign(test, c1, bin(BinOp::Eq, copy_local(cnt), int(1, Ty::U64)));
    fb.branch(test, c1, last, dec);
    let freed = fb.call(
        last,
        Callee::Extern(k.free),
        vec![copy_local(q2), int(32, Ty::U64), int(8, Ty::U64)],
        None,
    );
    fb.goto(freed, exit);
    fb.assign(dec, m, bin(BinOp::Sub, copy_local(cnt), int(1, Ty::U64)));
    fb.push(dec, Stmt::Assign(count(q2), Rvalue::Use(copy_local(m))));
    fb.goto(dec, exit);
    fb.ret(exit, copy_local(x));
    let f = k.pb.add(fb.finish());
    (k.pb.finish(), f)
}

/// Whether `f`'s count changes were removed (no store to a count word, no free).
fn borrowed(between: Between) -> bool {
    let (mut p, f) = program(between);
    let changed = run(&mut p)[f.0 as usize];
    assert_valid(&p);
    let func = &p.funcs[f.0 as usize];
    let counts = (func.blocks.iter().flat_map(|b: &BasicBlock| &b.stmts))
        .filter(|s| matches!(s, Stmt::Assign(d, _) if count_word(d).is_some()))
        .count();
    let frees = func.blocks.iter().filter(|b| {
        matches!(&b.term, Terminator::Call { args, .. } if args.len() == 3 && matches!(args[1], Operand::Const(Const::Int(32), _)))
    });
    let removed = counts == 0 && frees.count() == 0;
    assert_eq!(changed, removed, "{between:?}");
    removed
}

#[test]
fn a_copy_only_read_borrows() {
    for b in [
        Between::Nothing,
        Between::UnknownExternAfter,
        Between::QuietCallWithData,
        Between::Alloc,
        Between::NoReturn,
        Between::NoReturnFreeing,
        Between::StringExtern,
    ] {
        assert!(borrowed(b), "{b:?}");
    }
}

#[test]
fn a_copy_something_may_free_or_take_keeps_its_count() {
    for b in [
        Between::UnknownExtern,
        Between::FreeingCall,
        Between::FreeingCallWithDataAfter,
        Between::Escapes,
        Between::ReturnsWithout,
        Between::SharedAgain,
        Between::NoReturnWithData,
        Between::ClosureCall,
    ] {
        assert!(!borrowed(b), "{b:?}");
    }
}

/// `_Gdrop_Db(slot)`: the release of the pointer in `*slot` (glue.rs).
fn release_glue(k: &mut Kit) -> FuncId {
    let mut fb = FuncBuilder::internal("_Gdrop_Db", &[Ty::Ptr], Ty::Unit);
    let slot = fb.param(0);
    let [x, c, q, n, t, m] =
        [Ty::Ptr, Ty::Bool, Ty::Ptr, Ty::U64, Ty::Bool, Ty::U64].map(|t| fb.local(t));
    let [b0, a, last, dec, r] = [fb.block(), fb.block(), fb.block(), fb.block(), fb.block()];
    fb.assign(b0, x, Rvalue::Use(copy_place(deref(slot, Ty::Ptr))));
    fb.assign(b0, c, bin(BinOp::Ne, copy_local(x), int(0, Ty::Ptr)));
    fb.branch(b0, c, a, r);
    fb.assign(a, q, bin(BinOp::PtrAdd, copy_local(x), int(-8, Ty::I64)));
    fb.assign(a, n, Rvalue::Use(copy_place(count(q))));
    fb.assign(a, t, bin(BinOp::Eq, copy_local(n), int(1, Ty::U64)));
    fb.branch(a, t, last, dec);
    let args = vec![copy_local(q), int(32, Ty::U64), int(8, Ty::U64)];
    let freed = fb.call(last, Callee::Extern(k.free), args, None);
    fb.goto(freed, r);
    fb.assign(dec, m, bin(BinOp::Sub, copy_local(n), int(1, Ty::U64)));
    fb.push(dec, Stmt::Assign(count(q), Rvalue::Use(copy_local(m))));
    fb.goto(dec, r);
    fb.ret(r, Operand::Const(Const::Unit, Ty::Unit));
    k.pb.add(fb.finish())
}

#[test]
fn a_release_through_drop_glue_borrows_too() {
    // `v = this.data; v.count++; x = v.len; _Gdrop_Db(&v); return x`
    for escapes in [false, true] {
        let mut k = kit();
        let glue = release_glue(&mut k);
        let mut fb = FuncBuilder::export("f", &[Ty::Ptr], Ty::U64);
        let this = fb.param(0);
        let [v, r, q, n, x, a] =
            [Ty::Ptr, Ty::Ptr, Ty::Ptr, Ty::U64, Ty::U64, Ty::Ptr].map(|t| fb.local(t));
        let entry = fb.block();
        fb.assign(entry, v, Rvalue::Use(int(0, Ty::Ptr)));
        fb.assign(entry, r, Rvalue::Use(copy_place(field_of(this, k.obj, 0))));
        fb.assign(
            entry,
            q,
            bin(BinOp::PtrAdd, copy_local(r), int(-8, Ty::I64)),
        );
        fb.assign(
            entry,
            n,
            bin(BinOp::Add, copy_place(count(q)), int(1, Ty::U64)),
        );
        fb.push(entry, Stmt::Assign(count(q), Rvalue::Use(copy_local(n))));
        fb.assign(entry, v, Rvalue::Use(copy_local(r)));
        fb.assign(entry, x, Rvalue::Use(copy_place(field_of(v, k.arr, 1))));
        fb.assign(entry, a, Rvalue::AddrOf(Place::local(v)));
        let mut b = fb.call(entry, Callee::Func(glue), vec![copy_local(a)], None);
        if escapes {
            // The address also goes to a function that may keep it.
            b = fb.call(
                b,
                Callee::Extern(k.other),
                vec![copy_local(a), copy_local(a)],
                None,
            );
        }
        fb.ret(b, copy_local(x));
        let f = k.pb.add(fb.finish());
        let mut p = k.pb.finish();
        let changed = run(&mut p)[f.0 as usize];
        assert_valid(&p);
        let calls_glue = p.funcs[f.0 as usize].blocks.iter().any(
            |b| matches!(&b.term, Terminator::Call { callee: Callee::Func(g), .. } if *g == glue),
        );
        assert_eq!(
            (changed, calls_glue),
            (!escapes, escapes),
            "escapes: {escapes}"
        );
    }
}
