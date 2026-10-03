//! Memory programs: aggregates, projections, pointers, memcpy, statics and rt calls.

use super::*;
use crate::tests::*;
use velt_vir::vir::Ty::*;

// ───────────── aggregates, pointers, memcpy ─────────────

pub(crate) fn aggregates() -> TestProgram {
    let (mut pb, rt) = ProgramBuilder::new();
    // Point { x: I64 @0, y: I32 @8, flag: Bool @12 }
    let point = pb.agg(AggLayout {
        name: "Point".into(),
        size: 16,
        align: 8,
        fields: vec![(I64, 0), (I32, 8), (Bool, 12)],
    });
    // Outer { p: Point @0, z: F64 @16, b: U8 @24 }
    let outer = pb.agg(AggLayout {
        name: "Outer".into(),
        size: 32,
        align: 8,
        fields: vec![(Agg(point), 0), (F64, 16), (U8, 24)],
    });
    // Enum views: Opt { tag: U8 } and Opt::Some { tag: U8 @0, v: I64 @8 }
    let opt = pb.agg(AggLayout {
        name: "Opt".into(),
        size: 16,
        align: 8,
        fields: vec![(U8, 0)],
    });
    let opt_some = pb.agg(AggLayout {
        name: "Opt::Some".into(),
        size: 16,
        align: 8,
        fields: vec![(U8, 0), (I64, 8)],
    });
    // Big { a: I64 @0, b: I64 @192 } (size 200 → memcpy path)
    let big = pb.agg(AggLayout {
        name: "Big".into(),
        size: 200,
        align: 8,
        fields: vec![(I64, 0), (I64, 192)],
    });

    // fn make_point(out: Ptr, v: I64) { *out = Point { x: v, y: (v * 2) as i32, flag: true } }
    let make_point = {
        let mut fb = FuncBuilder::internal("make_point", &[Ptr, I64], Unit);
        let t = fb.local(I64);
        let y = fb.local(I32);
        let b = fb.block();
        fb.assign(b, t, bin(BinOp::Mul, copy_local(Local(1)), int(2, I64)));
        fb.assign(b, y, Rvalue::Cast(copy_local(t), I32));
        fb.push(
            b,
            Stmt::Assign(
                place(Local(0), vec![Proj::Deref(Agg(point))]),
                Rvalue::Aggregate(
                    point,
                    vec![
                        copy_local(Local(1)),
                        copy_local(y),
                        Operand::Const(Const::Bool(true), Bool),
                    ],
                ),
            ),
        );
        fb.term(b, Terminator::Return(Operand::Const(Const::Unit, Unit)));
        pb.add(fb.finish())
    };
    // fn sum_point(p: Ptr) -> I64 { return p->x + (p->y as i64) }
    let sum_point = {
        let mut fb = FuncBuilder::internal("sum_point", &[Ptr], I64);
        let y = fb.local(I64);
        let r = fb.local(I64);
        let b = fb.block();
        fb.assign(
            b,
            y,
            Rvalue::Cast(
                copy_place(place(
                    Local(0),
                    vec![Proj::Deref(Agg(point)), Proj::Field(1)],
                )),
                I64,
            ),
        );
        fb.assign(
            b,
            r,
            bin(
                BinOp::Add,
                copy_place(place(
                    Local(0),
                    vec![Proj::Deref(Agg(point)), Proj::Field(0)],
                )),
                copy_local(y),
            ),
        );
        fb.term(b, Terminator::Return(copy_local(r)));
        pb.add(fb.finish())
    };

    let (mut fb, b0) = main_fb();
    let a = fb.local(Agg(point));
    let pa = fb.local(Ptr);
    let o = fb.local(Agg(outer));
    let x = fb.local(I64);
    let px = fb.local(Ptr);
    let e = fb.local(Agg(opt));
    let g1 = fb.local(Agg(big));
    let g2 = fb.local(Agg(big));
    let pg1 = fb.local(Ptr);
    let pg2 = fb.local(Ptr);
    let s = fb.local(I64);
    let mut exp = String::new();
    let mut out = Out {
        fb: &mut fb,
        rt: &rt,
        cur: b0,
    };
    let cur = out.cur;
    // a = Point { 10, 7, false }
    out.fb.assign(
        cur,
        a,
        Rvalue::Aggregate(
            point,
            vec![
                int(10, I64),
                int(7, I32),
                Operand::Const(Const::Bool(false), Bool),
            ],
        ),
    );
    // pa = &a
    out.fb.assign(cur, pa, Rvalue::AddrOf(Place::local(a)));
    // o.p = *pa   (aggregate copy through a Deref'd pointer)
    out.fb.push(
        cur,
        Stmt::Assign(
            place(o, vec![Proj::Field(0)]),
            Rvalue::Use(copy_place(place(pa, vec![Proj::Deref(Agg(point))]))),
        ),
    );
    out.fb.push(
        cur,
        Stmt::Assign(place(o, vec![Proj::Field(1)]), Rvalue::Use(float(2.5, F64))),
    );
    out.fb.push(
        cur,
        Stmt::Assign(place(o, vec![Proj::Field(2)]), Rvalue::Use(int(255, U8))),
    );
    // (*pa).y = 99
    out.fb.push(
        cur,
        Stmt::Assign(
            place(pa, vec![Proj::Deref(Agg(point)), Proj::Field(1)]),
            Rvalue::Use(int(99, I32)),
        ),
    );
    out.line(
        copy_place(place(o, vec![Proj::Field(0), Proj::Field(1)])),
        I32,
    );
    exp.push_str("7\n");
    out.line(copy_place(place(a, vec![Proj::Field(1)])), I32);
    exp.push_str("99\n");
    out.line(
        copy_place(place(o, vec![Proj::Field(0), Proj::Field(0)])),
        I64,
    );
    exp.push_str("10\n");
    out.line(copy_place(place(o, vec![Proj::Field(1)])), F64);
    exp.push_str("2.5\n");
    out.line(copy_place(place(o, vec![Proj::Field(2)])), U8);
    exp.push_str("255\n");
    out.line(
        copy_place(place(o, vec![Proj::Field(0), Proj::Field(2)])),
        Bool,
    );
    exp.push_str("false\n");
    // make_point(&o.p, 21); print o.p.y, o.p.flag, sum_point(&o.p)
    let cur = out.cur;
    let pp = out.fb.local(Ptr);
    out.fb
        .assign(cur, pp, Rvalue::AddrOf(place(o, vec![Proj::Field(0)])));
    out.cur = out.fb.call(
        cur,
        Callee::Func(make_point),
        vec![copy_local(pp), int(21, I64)],
        None,
    );
    out.line(
        copy_place(place(o, vec![Proj::Field(0), Proj::Field(1)])),
        I32,
    );
    exp.push_str("42\n");
    out.line(
        copy_place(place(o, vec![Proj::Field(0), Proj::Field(2)])),
        Bool,
    );
    exp.push_str("true\n");
    out.cur = out.fb.call(
        out.cur,
        Callee::Func(sum_point),
        vec![copy_local(pp)],
        Some(Place::local(s)),
    );
    out.line(copy_local(s), I64);
    exp.push_str("63\n");
    // a = o.p (whole aggregate copy between locals); print a.x
    let cur = out.cur;
    out.fb.assign(
        cur,
        a,
        Rvalue::Use(copy_place(place(o, vec![Proj::Field(0)]))),
    );
    out.line(copy_place(place(a, vec![Proj::Field(0)])), I64);
    exp.push_str("21\n");
    // Address-taken scalar: x = 5; px = &x; *px = 42; print x
    let cur = out.cur;
    out.fb.assign(cur, x, Rvalue::Use(int(5, I64)));
    out.fb.assign(cur, px, Rvalue::AddrOf(Place::local(x)));
    out.fb.push(
        cur,
        Stmt::Assign(place(px, vec![Proj::Deref(I64)]), Rvalue::Use(int(42, I64))),
    );
    out.line(copy_local(x), I64);
    exp.push_str("42\n");
    // Enum view: e as Opt::Some = { 1, -77 }; print e.tag, (e as Some).v
    let cur = out.cur;
    out.fb.push(
        cur,
        Stmt::Assign(
            place(e, vec![Proj::Cast(opt_some)]),
            Rvalue::Aggregate(opt_some, vec![int(1, U8), int(-77, I64)]),
        ),
    );
    out.line(copy_place(place(e, vec![Proj::Field(0)])), U8);
    exp.push_str("1\n");
    out.line(
        copy_place(place(e, vec![Proj::Cast(opt_some), Proj::Field(1)])),
        I64,
    );
    exp.push_str("-77\n");
    // Big aggregates: g1 = { 1, 2 }; g2 = g1 (memcpy); MemCopy(&g1 <- &g2) after g2.b = 9
    let cur = out.cur;
    out.fb.push(
        cur,
        Stmt::Assign(place(g1, vec![Proj::Field(0)]), Rvalue::Use(int(1, I64))),
    );
    out.fb.push(
        cur,
        Stmt::Assign(place(g1, vec![Proj::Field(1)]), Rvalue::Use(int(2, I64))),
    );
    out.fb.assign(cur, g2, Rvalue::Use(copy_local(g1)));
    out.fb.push(
        cur,
        Stmt::Assign(place(g1, vec![Proj::Field(1)]), Rvalue::Use(int(3, I64))),
    );
    out.line(copy_place(place(g2, vec![Proj::Field(1)])), I64);
    exp.push_str("2\n");
    let cur = out.cur;
    out.fb.push(
        cur,
        Stmt::Assign(place(g2, vec![Proj::Field(1)]), Rvalue::Use(int(9, I64))),
    );
    out.fb.assign(cur, pg1, Rvalue::AddrOf(Place::local(g1)));
    out.fb.assign(cur, pg2, Rvalue::AddrOf(Place::local(g2)));
    out.fb.push(
        cur,
        Stmt::MemCopy {
            dst: copy_local(pg1),
            src: copy_local(pg2),
            size: 200,
        },
    );
    out.line(copy_place(place(g1, vec![Proj::Field(1)])), I64);
    exp.push_str("9\n");
    // Small MemCopy (12 bytes, unaligned-size path): copy x,y of g2 into g1 region via PtrAdd
    let cur = out.cur;
    out.fb.push(
        cur,
        Stmt::Assign(place(g2, vec![Proj::Field(0)]), Rvalue::Use(int(-5, I64))),
    );
    out.fb.push(
        cur,
        Stmt::MemCopy {
            dst: copy_local(pg1),
            src: copy_local(pg2),
            size: 12,
        },
    );
    out.line(copy_place(place(g1, vec![Proj::Field(0)])), I64);
    exp.push_str("-5\n");
    let cur = out.cur;
    pb.add(finish_main(fb, cur, 0));
    TestProgram {
        name: "aggregates",
        program: pb.p,
        stdout: exp,
        exit: 0,
    }
}

// ───────────── strings / statics / rt calls ─────────────

pub(crate) fn strings() -> TestProgram {
    let (mut pb, rt) = ProgramBuilder::new();
    let hello = pb.stat(b"hello, world");
    let empty = pb.stat(b"");
    let (mut fb, b0) = main_fb();
    let s = fb.local(Agg(STR_AGG));
    let ps = fb.local(Ptr);
    let t = fb.local(Agg(STR_AGG));
    let pt = fb.local(Ptr);
    let c = fb.local(I32);
    let mut out = Out {
        fb: &mut fb,
        rt: &rt,
        cur: b0,
    };
    // String word 0 is a `U64` (the static's address cast), like lowering's `str_lit`.
    let (hello_w0, empty_w0) = (out.fb.local(U64), out.fb.local(U64));
    let addr = |id: StaticId| Rvalue::Cast(Operand::Const(Const::Static(id), Ptr), U64);
    out.fb.assign(b0, hello_w0, addr(hello));
    out.fb.assign(b0, empty_w0, addr(empty));
    // ASCII literals: `w1` is `units << 32 | len` with units == len.
    let lit = |w0: Local, len: i128| {
        let w1 = int((len << 32) | len, U64);
        Rvalue::Aggregate(STR_AGG, vec![copy_local(w0), w1, int(0, U64)])
    };
    out.fb.assign(b0, s, lit(hello_w0, 12));
    out.fb.assign(b0, ps, Rvalue::AddrOf(Place::local(s)));
    out.fb.assign(b0, t, lit(empty_w0, 0));
    out.fb.assign(b0, pt, Rvalue::AddrOf(Place::local(t)));
    out.cur = out.fb.call(
        out.cur,
        Callee::Extern(rt.write_str),
        vec![int(1, U32), copy_local(ps)],
        None,
    );
    out.byte(b' ');
    out.cur = out.fb.call(
        out.cur,
        Callee::Extern(rt.write_str),
        vec![int(1, U32), copy_local(pt)],
        None,
    );
    out.bool(Operand::Const(Const::Bool(true), Bool));
    out.byte(b' ');
    // The byte length: the low half of word 1.
    let len = out.fb.local(U64);
    let w1 = copy_place(place(s, vec![Proj::Field(1)]));
    let cur = out.cur;
    out.fb
        .assign(cur, len, bin(BinOp::BitAnd, w1, int(0xffff_ffff, U64)));
    out.u64(copy_local(len));
    out.nl();
    out.cur = out.fb.call(
        out.cur,
        Callee::Extern(rt.str_cmp),
        vec![copy_local(ps), copy_local(pt)],
        Some(Place::local(c)),
    );
    out.line(copy_local(c), I32);
    out.f64(float(1.5, F64));
    out.nl();
    out.cur = out.fb.call(out.cur, Callee::Extern(rt.flush), vec![], None);
    // Unit-typed call destination is allowed and ignored.
    let u = out.fb.local(Unit);
    out.cur = out.fb.call(
        out.cur,
        Callee::Extern(rt.flush),
        vec![],
        Some(Place::local(u)),
    );
    // A never-taken path to a noreturn extern followed by Unreachable.
    let cur = out.cur;
    let cond = out.fb.local(Bool);
    let panic_bb = out.fb.block();
    let ok_bb = out.fb.block();
    out.fb.assign(
        cur,
        cond,
        bin(
            BinOp::Eq,
            copy_place(place(s, vec![Proj::Field(1)])),
            int(0, U64),
        ),
    );
    out.fb.term(
        cur,
        Terminator::Branch {
            cond: copy_local(cond),
            then: panic_bb,
            els: ok_bb,
        },
    );
    let after_panic = out.fb.call(
        panic_bb,
        Callee::Extern(rt.panic),
        vec![copy_local(ps)],
        None,
    );
    out.fb.term(after_panic, Terminator::Unreachable);
    pb.add(finish_main(fb, ok_bb, 42));
    TestProgram {
        name: "strings",
        program: pb.p,
        stdout: "hello, world true 12\n1\n1.5\n".into(),
        exit: 42,
    }
}
