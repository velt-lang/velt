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
    /// A math runtime function (`f64.fromBits`).
    MathExtern,
    /// A string from a byte range of `data` itself.
    BytesRangeExtern,
    /// A pointer into the object (`&(*v).len`) read through.
    InteriorPointer,
    /// That pointer stored into the object holding the source.
    InteriorPointerEscapes,
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
    bits: ExternId,
    range: ExternId,
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
    let bits = pb.ext("velt_rt_math_bits_f64", &[Ty::U64], Ty::F64, false);
    let range_params = [Ty::Ptr, Ty::I64, Ty::I64, Ty::Ptr];
    let range = pb.ext("velt_rt_bytes_range_to_str", &range_params, Ty::Unit, false);
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
        bits,
        range,
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
        Between::MathExtern => {
            let f = fb.local(Ty::F64);
            b = fb.call(b, Callee::Extern(k.bits), vec![int(7, Ty::U64)], Some(f))
        }
        Between::BytesRangeExtern => {
            let args = vec![
                copy_local(v),
                int(0, Ty::I64),
                int(1, Ty::I64),
                copy_local(this),
            ];
            b = call(&mut fb, b, Callee::Extern(k.range), args)
        }
        Between::InteriorPointer | Between::InteriorPointerEscapes => {
            let (ip, y) = (fb.local(Ty::Ptr), fb.local(Ty::U64));
            fb.assign(b, ip, Rvalue::AddrOf(field_of(v, k.arr, 1)));
            fb.assign(b, y, Rvalue::Use(copy_place(deref(ip, Ty::U64))));
            if between == Between::InteriorPointerEscapes {
                let to = field_of(this, k.obj, 0);
                fb.push(b, Stmt::Assign(to, Rvalue::Use(copy_local(ip))));
            }
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
        Between::MathExtern,
        Between::BytesRangeExtern,
        Between::InteriorPointer,
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
        Between::InteriorPointerEscapes,
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

/// `r = src; q = r - 8; *q = *q + 1`: a counted copy of `src` shared; returns `r` and `q`.
fn share(fb: &mut FuncBuilder, b: BlockId, src: Place) -> (Local, Local) {
    let [r, q, n] = [Ty::Ptr, Ty::Ptr, Ty::U64].map(|t| fb.local(t));
    fb.assign(b, r, Rvalue::Use(copy_place(src)));
    fb.assign(b, q, bin(BinOp::PtrAdd, copy_local(r), int(-8, Ty::I64)));
    let add = bin(BinOp::Add, copy_place(count(q)), int(1, Ty::U64));
    fb.assign(b, n, add);
    fb.push(b, Stmt::Assign(count(q), Rvalue::Use(copy_local(n))));
    (r, q)
}

/// `if (v != null) { if (count == 1) free… else count-- }` from `b`: the block after it and the
/// release's count pointer.
fn release(fb: &mut FuncBuilder, b: BlockId, v: Local, free: ExternId) -> (BlockId, Local) {
    let [c0, q, cnt, c1, m] = [Ty::Bool, Ty::Ptr, Ty::U64, Ty::Bool, Ty::U64].map(|t| fb.local(t));
    let [test, last, dec, exit] = [fb.block(), fb.block(), fb.block(), fb.block()];
    fb.assign(b, c0, bin(BinOp::Ne, copy_local(v), int(0, Ty::Ptr)));
    fb.branch(b, c0, test, exit);
    fb.assign(test, q, bin(BinOp::PtrAdd, copy_local(v), int(-8, Ty::I64)));
    fb.assign(test, cnt, Rvalue::Use(copy_place(count(q))));
    fb.assign(test, c1, bin(BinOp::Eq, copy_local(cnt), int(1, Ty::U64)));
    fb.branch(test, c1, last, dec);
    let args = vec![copy_local(q), int(32, Ty::U64), int(8, Ty::U64)];
    let freed = fb.call(last, Callee::Extern(free), args, None);
    fb.goto(freed, exit);
    fb.assign(dec, m, bin(BinOp::Sub, copy_local(cnt), int(1, Ty::U64)));
    fb.push(dec, Stmt::Assign(count(q), Rvalue::Use(copy_local(m))));
    fb.goto(dec, exit);
    (exit, q)
}

/// `x = v.len` in `b`.
fn read_len(fb: &mut FuncBuilder, k: &Kit, b: BlockId, v: Local) -> Local {
    let x = fb.local(Ty::U64);
    fb.assign(b, x, Rvalue::Use(copy_place(field_of(v, k.arr, 1))));
    x
}

/// Runs the pass and returns, per family (the count pointers of its increment and releases),
/// whether `f` borrowed it: no store through those count pointers is left.
fn decide(mut p: Program, f: FuncId, families: &[[Local; 2]]) -> Vec<bool> {
    let changed = run(&mut p)[f.0 as usize];
    assert_valid(&p);
    let func = &p.funcs[f.0 as usize];
    let stores_through = |qs: &[Local; 2]| {
        (func.blocks.iter().flat_map(|b| &b.stmts)).any(
            |s| matches!(s, Stmt::Assign(d, _) if count_word(d).is_some_and(|q| qs.contains(&q))),
        )
    };
    let out: Vec<bool> = families.iter().map(|qs| !stores_through(qs)).collect();
    assert_eq!(changed, out.iter().any(|&b| b), "{out:?}");
    out
}

/// What the nullable-field test runs (see `a_nullable_field_loaded_twice`).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Nullable {
    Nothing,
    /// The freeing function before the plain copy: nothing is held yet.
    FreeBeforeLoad,
    /// The freeing function after the share, before the read.
    FreeAfterShare,
    /// The field loaded into the copy again while it holds the reference.
    ReloadWhileHeld,
}

#[test]
fn a_nullable_field_loaded_twice() {
    // `c = this.data; if (c != null) { r = this.data; r.count++; c = r }; x = c.len; release c`:
    // lowering's copy of a nullable field reads the field twice, the plain copy then the share.
    // The repeated load joins the family (same pointer, or null); a freeing call while the
    // share is held and used, or another load into the copy while it holds the reference
    // (it would be overwritten unreleased), keeps the count.
    for (case, borrows) in [
        (Nullable::Nothing, true),
        (Nullable::FreeBeforeLoad, true),
        (Nullable::FreeAfterShare, false),
        (Nullable::ReloadWhileHeld, false),
    ] {
        let k = kit();
        let mut fb = FuncBuilder::export("f", &[Ty::Ptr], Ty::U64);
        let this = fb.param(0);
        let [c, t, slot] = [Ty::Ptr, Ty::Bool, Ty::Ptr].map(|t| fb.local(t));
        let [mut b, set, join] = [fb.block(), fb.block(), fb.block()];
        // `slot = &this.data`, loaded twice: the repeat must be `*p` of a register written once.
        fb.assign(b, slot, Rvalue::AddrOf(field_of(this, k.obj, 0)));
        let freeing = |fb: &mut FuncBuilder, b| {
            fb.call(b, Callee::Func(k.freeing), vec![copy_local(this)], None)
        };
        if case == Nullable::FreeBeforeLoad {
            b = freeing(&mut fb, b);
        }
        let field = deref(slot, Ty::Ptr);
        fb.assign(b, c, Rvalue::Use(copy_place(field.clone())));
        fb.assign(b, t, bin(BinOp::Ne, copy_local(c), int(0, Ty::Ptr)));
        fb.branch(b, t, set, join);
        let (r, q) = share(&mut fb, set, field.clone());
        fb.assign(set, c, Rvalue::Use(copy_local(r)));
        fb.goto(set, join);
        let mut b = join;
        match case {
            Nullable::FreeAfterShare => b = freeing(&mut fb, b),
            Nullable::ReloadWhileHeld => fb.assign(b, c, Rvalue::Use(copy_place(field))),
            _ => {}
        }
        let x = read_len(&mut fb, &k, b, c);
        let (exit, q2) = release(&mut fb, b, c, k.free);
        fb.ret(exit, copy_local(x));
        let mut pb = k.pb;
        let f = pb.add(fb.finish());
        assert_eq!(decide(pb.finish(), f, &[[q, q2]]), [borrows], "{case:?}");
    }
}

/// Two families in one function (see `two_families_are_decided_together`).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Two {
    /// `a` shared, read and released; then `b` shared, a freeing call, `b` read and released.
    Disjoint,
    /// `a` shared; `b` shared, read and released; `a` read and released.
    Nested,
    /// The same with `b` stored into the object (so `b` keeps its count).
    NestedEscapes,
}

#[test]
fn two_families_are_decided_together() {
    // Disjoint: the freeing call runs only after `a`'s last use, so `a` borrows while `b`,
    // used after it, keeps its count. Nested: `b`'s release runs while `a` is still used; when
    // `b` borrows too its release no longer frees and both borrow, but when `b` must keep its
    // count, its release may free `a`'s object (the same array), so `a` keeps its count too.
    for (case, expect) in [
        (Two::Disjoint, [true, false]),
        (Two::Nested, [true, true]),
        (Two::NestedEscapes, [false, false]),
    ] {
        let k = kit();
        let mut fb = FuncBuilder::export("f", &[Ty::Ptr], Ty::U64);
        let this = fb.param(0);
        let field = field_of(this, k.obj, 0);
        let mut b = fb.block();
        let (a, qa) = share(&mut fb, b, field.clone());
        let mut xa = None;
        let mut qa2 = None;
        if case == Two::Disjoint {
            xa = Some(read_len(&mut fb, &k, b, a));
            let (next, q) = release(&mut fb, b, a, k.free);
            (b, qa2) = (next, Some(q));
        }
        let (r, qb) = share(&mut fb, b, field.clone());
        if case == Two::Disjoint {
            let args = vec![copy_local(this)];
            b = fb.call(b, Callee::Func(k.freeing), args, None);
        }
        if case == Two::NestedEscapes {
            fb.push(b, Stmt::Assign(field, Rvalue::Use(copy_local(r))));
        }
        let xb = read_len(&mut fb, &k, b, r);
        let (mut b, qb2) = release(&mut fb, b, r, k.free);
        if case != Two::Disjoint {
            xa = Some(read_len(&mut fb, &k, b, a));
            let (next, q) = release(&mut fb, b, a, k.free);
            (b, qa2) = (next, Some(q));
        }
        let sum = fb.local(Ty::U64);
        let (xa, qa2) = (xa.unwrap(), qa2.unwrap());
        fb.assign(b, sum, bin(BinOp::Add, copy_local(xa), copy_local(xb)));
        fb.ret(b, copy_local(sum));
        let mut pb = k.pb;
        let f = pb.add(fb.finish());
        let got = decide(pb.finish(), f, &[[qa, qa2], [qb, qb2]]);
        assert_eq!(got, expect, "{case:?}");
    }
}

#[test]
fn a_copy_used_across_iterations_keeps_its_count_over_a_freeing_call() {
    // Hoisted (`const data = this.data; while (go) { x = data.len; call() }`): the call comes
    // after the iteration's last use but before the next iteration's, so the copy is live across
    // it and a call that may free (a freeing Velt function, an unknown runtime function) keeps
    // the count; an allocation does not free and the copy borrows.
    // Reloaded (`while (go) { const data = this.data; x = data.len; call(); release }`): each
    // iteration loads the field afresh, so the copy is dead from its last use to the next load.
    // Borrowing is sound: only the source's reference counted while the copy was used, and the
    // copy's release (which with the count kept would have freed after the call) is gone, so
    // nothing touches the object after the call and the next iteration reads whatever the
    // field then holds. So it borrows even across a freeing call.
    for hoisted in [true, false] {
        for (callee, frees) in [("freeing", true), ("other", true), ("alloc", false)] {
            let k = kit();
            let mut fb = FuncBuilder::export("f", &[Ty::Ptr, Ty::Bool], Ty::U64);
            let (this, go) = (fb.param(0), fb.param(1));
            let field = field_of(this, k.obj, 0);
            let [entry, head, body] = [fb.block(), fb.block(), fb.block()];
            let (exit, latch) = (fb.block(), fb.block());
            let hoist = hoisted.then(|| share(&mut fb, entry, field.clone()));
            fb.goto(entry, head);
            fb.branch(head, go, body, exit);
            let (r, q) = hoist.unwrap_or_else(|| share(&mut fb, body, field.clone()));
            read_len(&mut fb, &k, body, r);
            let (call, args) = match callee {
                "freeing" => (Callee::Func(k.freeing), vec![copy_local(this)]),
                "other" => (Callee::Extern(k.other), vec![copy_local(this); 2]),
                _ => (Callee::Extern(k.alloc), vec![int(8, Ty::U64); 2]),
            };
            let dest = (callee == "alloc").then(|| fb.local(Ty::Ptr));
            let after = fb.call(body, call, args, dest);
            let q2 = if hoisted {
                fb.goto(after, latch);
                let (done, q2) = release(&mut fb, exit, r, k.free);
                fb.ret(done, int(0, Ty::U64));
                q2
            } else {
                let (done, q2) = release(&mut fb, after, r, k.free);
                fb.goto(done, latch);
                fb.ret(exit, int(0, Ty::U64));
                q2
            };
            fb.goto(latch, head);
            let mut pb = k.pb;
            let f = pb.add(fb.finish());
            let borrows = !(hoisted && frees);
            let got = decide(pb.finish(), f, &[[q, q2]]);
            assert_eq!(got, [borrows], "hoisted: {hoisted}, callee: {callee}");
        }
    }
}

#[test]
fn a_function_freeing_only_through_a_cycle_may_free() {
    // `ping(a) { pong(a) }`, `pong(a) { if (a != null) { ping(a); free(a) } }`: `ping` calls
    // no freer directly, only through the cycle; the component's fixed point must still find
    // that it frees, so `f` calling it between the share and the read keeps the count. Without
    // the `free` the cycle frees nothing and `f` borrows.
    for frees in [true, false] {
        let mut k = kit();
        let (ping, pong) = (k.pb.reserve(), k.pb.reserve());
        let unit = || Operand::Const(Const::Unit, Ty::Unit);
        let mut fb = FuncBuilder::internal("ping", &[Ty::Ptr], Ty::Unit);
        let (a, b) = (fb.param(0), fb.block());
        let next = fb.call(b, Callee::Func(pong), vec![copy_local(a)], None);
        fb.ret(next, unit());
        k.pb.set(ping, fb.finish());
        let mut fb = FuncBuilder::internal("pong", &[Ty::Ptr], Ty::Unit);
        let (a, t) = (fb.param(0), fb.local(Ty::Bool));
        let [b, more, done] = [fb.block(), fb.block(), fb.block()];
        fb.assign(b, t, bin(BinOp::Ne, copy_local(a), int(0, Ty::Ptr)));
        fb.branch(b, t, more, done);
        let mut next = fb.call(more, Callee::Func(ping), vec![copy_local(a)], None);
        if frees {
            let args = vec![copy_local(a), int(24, Ty::U64), int(8, Ty::U64)];
            next = fb.call(next, Callee::Extern(k.free), args, None);
        }
        fb.goto(next, done);
        fb.ret(done, unit());
        k.pb.set(pong, fb.finish());
        let mut fb = FuncBuilder::export("f", &[Ty::Ptr], Ty::U64);
        let this = fb.param(0);
        let b = fb.block();
        let (r, q) = share(&mut fb, b, field_of(this, k.obj, 0));
        let b = fb.call(b, Callee::Func(ping), vec![copy_local(this)], None);
        let x = read_len(&mut fb, &k, b, r);
        let (exit, q2) = release(&mut fb, b, r, k.free);
        fb.ret(exit, copy_local(x));
        let f = k.pb.add(fb.finish());
        assert_eq!(
            decide(k.pb.finish(), f, &[[q, q2]]),
            [!frees],
            "frees: {frees}"
        );
    }
}
