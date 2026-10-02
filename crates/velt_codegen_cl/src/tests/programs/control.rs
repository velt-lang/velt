//! Control-flow programs: recursion, loops, switches, indirect calls.

use super::*;
use crate::tests::*;
use velt_vir::vir::Ty::*;

// ───────────── fib: recursion + loops ─────────────

pub(crate) fn fib() -> TestProgram {
    let (mut pb, rt) = ProgramBuilder::new();
    let fib_id = pb.reserve();
    {
        let mut fb = FuncBuilder::internal("fib", &[I64], I64);
        let n = fb.param(0);
        let c = fb.local(Bool);
        let a = fb.local(I64);
        let r1 = fb.local(I64);
        let bb = fb.local(I64);
        let r2 = fb.local(I64);
        let s = fb.local(I64);
        let b0 = fb.block();
        let b1 = fb.block();
        let b2 = fb.block();
        fb.assign(b0, c, bin(BinOp::Lt, copy_local(n), int(2, I64)));
        fb.term(
            b0,
            Terminator::Branch {
                cond: copy_local(c),
                then: b1,
                els: b2,
            },
        );
        fb.term(b1, Terminator::Return(copy_local(n)));
        fb.assign(b2, a, bin(BinOp::Sub, copy_local(n), int(1, I64)));
        let b3 = fb.call(
            b2,
            Callee::Func(fib_id),
            vec![copy_local(a)],
            Some(Place::local(r1)),
        );
        fb.assign(b3, bb, bin(BinOp::Sub, copy_local(n), int(2, I64)));
        let b4 = fb.call(
            b3,
            Callee::Func(fib_id),
            vec![copy_local(bb)],
            Some(Place::local(r2)),
        );
        fb.assign(b4, s, bin(BinOp::Add, copy_local(r1), copy_local(r2)));
        fb.term(b4, Terminator::Return(copy_local(s)));
        pb.set(fib_id, fb.finish());
    }
    let iter_id = {
        let mut fb = FuncBuilder::internal("fib_iter", &[I64], I64);
        let n = fb.param(0);
        let (a, b, i, t, c) = (
            fb.local(I64),
            fb.local(I64),
            fb.local(I64),
            fb.local(I64),
            fb.local(Bool),
        );
        let entry = fb.block();
        let head = fb.block();
        let body = fb.block();
        let exit = fb.block();
        fb.assign(entry, a, Rvalue::Use(int(0, I64)));
        fb.assign(entry, b, Rvalue::Use(int(1, I64)));
        fb.assign(entry, i, Rvalue::Use(int(0, I64)));
        fb.term(entry, Terminator::Goto(head));
        fb.assign(head, c, bin(BinOp::Lt, copy_local(i), copy_local(n)));
        fb.term(
            head,
            Terminator::Branch {
                cond: copy_local(c),
                then: body,
                els: exit,
            },
        );
        fb.assign(body, t, bin(BinOp::Add, copy_local(a), copy_local(b)));
        fb.assign(body, a, Rvalue::Use(copy_local(b)));
        fb.assign(body, b, Rvalue::Use(copy_local(t)));
        fb.assign(body, i, bin(BinOp::Add, copy_local(i), int(1, I64)));
        fb.term(body, Terminator::Goto(head));
        fb.term(exit, Terminator::Return(copy_local(a)));
        pb.add(fb.finish())
    };
    let (mut fb, b0) = main_fb();
    let r = fb.local(I64);
    let mut o = Out {
        fb: &mut fb,
        rt: &rt,
        cur: b0,
    };
    o.cur = o.fb.call(
        o.cur,
        Callee::Func(fib_id),
        vec![int(30, I64)],
        Some(Place::local(r)),
    );
    o.line(copy_local(r), I64);
    o.cur = o.fb.call(
        o.cur,
        Callee::Func(iter_id),
        vec![int(30, I64)],
        Some(Place::local(r)),
    );
    o.line(copy_local(r), I64);
    o.cur = o.fb.call(
        o.cur,
        Callee::Func(iter_id),
        vec![int(90, I64)],
        Some(Place::local(r)),
    );
    o.line(copy_local(r), I64);
    let cur = o.cur;
    pb.add(finish_main(fb, cur, 0));
    TestProgram {
        name: "fib",
        program: pb.p,
        stdout: "832040\n832040\n2880067194370816120\n".into(),
        exit: 0,
    }
}

// ───────────── block order: reads before the assignment in VIR order ─────────────

/// Single-assignment locals read in blocks that come before their assignment in VIR order (and
/// in an unreachable block), next to a local assigned twice: the translator first treats them as
/// SSA values, then translates `velt_main` again with them as variables.
pub(crate) fn block_order() -> TestProgram {
    let (mut pb, rt) = ProgramBuilder::new();
    let (mut fb, b0) = main_fb();
    let (x, y, k, c, u) = (
        fb.local(I64),
        fb.local(I64),
        fb.local(I64),
        fb.local(Bool),
        fb.local(I64),
    );
    let b1 = fb.block();
    let b2 = fb.block();
    let b3 = fb.block();
    let dead = fb.block();
    fb.assign(b0, k, Rvalue::Use(int(1, I64)));
    fb.term(b0, Terminator::Goto(b2));
    // b2 dominates b1 and b3 but follows them in VIR order.
    fb.assign(b2, x, bin(BinOp::Add, int(40, I64), copy_local(k)));
    fb.assign(b2, y, bin(BinOp::Add, copy_local(x), int(2, I64)));
    fb.assign(b2, k, Rvalue::Use(int(2, I64)));
    fb.assign(b2, c, bin(BinOp::Gt, copy_local(x), int(0, I64)));
    fb.term(
        b2,
        Terminator::Branch {
            cond: copy_local(c),
            then: b1,
            els: b3,
        },
    );
    fb.term(b3, Terminator::Return(int(1, I32)));
    // Never reached: reads `y` without a dominating assignment.
    fb.assign(dead, u, bin(BinOp::Add, copy_local(y), int(1, I64)));
    fb.term(dead, Terminator::Goto(b1));
    let mut o = Out {
        fb: &mut fb,
        rt: &rt,
        cur: b1,
    };
    o.line(copy_local(x), I64);
    o.line(copy_local(y), I64);
    o.line(copy_local(k), I64);
    let cur = o.cur;
    pb.add(finish_main(fb, cur, 0));
    TestProgram {
        name: "block_order",
        program: pb.p,
        stdout: "41\n43\n2\n".into(),
        exit: 0,
    }
}

/// One straight-line function of `n` calls, each result feeding the next through a fresh
/// temporary: as many blocks and single-assignment locals as statements, the shape of a long
/// generated `main` (compile-time memory used to grow with blocks × locals).
pub(crate) fn long_chain(n: u32) -> TestProgram {
    let (mut pb, rt) = ProgramBuilder::new();
    let inc = {
        let mut fb = FuncBuilder::internal("inc", &[I64], I64);
        let r = fb.local(I64);
        let b = fb.block();
        fb.assign(b, r, bin(BinOp::Add, copy_local(fb.param(0)), int(1, I64)));
        fb.term(b, Terminator::Return(copy_local(r)));
        pb.add(fb.finish())
    };
    let (mut fb, mut cur) = main_fb();
    let mut acc = fb.local(I64);
    fb.assign(cur, acc, Rvalue::Use(int(0, I64)));
    for _ in 0..n {
        let next = fb.local(I64);
        cur = fb.call(
            cur,
            Callee::Func(inc),
            vec![copy_local(acc)],
            Some(Place::local(next)),
        );
        acc = next;
    }
    let mut o = Out {
        fb: &mut fb,
        rt: &rt,
        cur,
    };
    o.line(copy_local(acc), I64);
    let cur = o.cur;
    pb.add(finish_main(fb, cur, 0));
    TestProgram {
        name: "long_chain",
        program: pb.p,
        stdout: format!("{n}\n"),
        exit: 0,
    }
}

// ───────────── switch ─────────────

pub(crate) fn switch() -> TestProgram {
    let (mut pb, rt) = ProgramBuilder::new();
    let classify = |pb: &mut ProgramBuilder, ty: Ty, cases: &[(i128, i128)]| {
        let mut fb = FuncBuilder::internal(&format!("classify{}", pb.p.funcs.len()), &[ty], I32);
        let b0 = fb.block();
        let def = fb.block();
        fb.term(def, Terminator::Return(int(0, I32)));
        let mut sc = vec![];
        for &(k, r) in cases {
            let blk = fb.block();
            fb.term(blk, Terminator::Return(int(r, I32)));
            sc.push((k, blk));
        }
        fb.term(
            b0,
            Terminator::Switch {
                value: copy_local(Local(0)),
                cases: sc,
                default: def,
            },
        );
        pb.add(fb.finish())
    };
    let c32 = classify(
        &mut pb,
        I32,
        &[
            (-1, 100),
            (0, 200),
            (5, 300),
            (6, 301),
            (7, 302),
            (8, 303),
            (1000, 400),
        ],
    );
    let c8 = classify(&mut pb, U8, &[(0, 1), (255, 2), (128, 3)]);
    let c64 = classify(
        &mut pb,
        I64,
        &[(i64::MIN as i128, 7), (u32::MAX as i128 + 1, 8)],
    );
    let (mut fb, b0) = main_fb();
    let mut cs = Cases {
        o: Out {
            fb: &mut fb,
            rt: &rt,
            cur: b0,
        },
        expected: String::new(),
    };
    for (v, e) in [
        (-1, "100"),
        (0, "200"),
        (5, "300"),
        (7, "302"),
        (8, "303"),
        (9, "0"),
        (1000, "400"),
        (-2, "0"),
    ] {
        cs.call(c32, vec![int(v, I32)], I32, e);
    }
    for (v, e) in [(0, "1"), (255, "2"), (128, "3"), (1, "0")] {
        cs.call(c8, vec![int(v, U8)], I32, e);
    }
    for (v, e) in [
        (i64::MIN as i128, "7"),
        (u32::MAX as i128 + 1, "8"),
        (0, "0"),
    ] {
        cs.call(c64, vec![int(v, I64)], I32, e);
    }
    let (cur, expected) = (cs.o.cur, cs.expected);
    pb.add(finish_main(fb, cur, 0));
    TestProgram {
        name: "switch",
        program: pb.p,
        stdout: expected,
        exit: 0,
    }
}

// ───────────── indirect calls ─────────────

pub(crate) fn indirect() -> TestProgram {
    let (mut pb, rt) = ProgramBuilder::new();
    let (add, _) = binary_fn(&mut pb, BinOp::Add, I64, I64);
    let (mul, _) = binary_fn(&mut pb, BinOp::Mul, I64, I64);
    // fn apply(f: Ptr, a: I64, b: I64) -> I64 { return f(a, b) }
    let apply = {
        let mut fb = FuncBuilder::internal("apply", &[Ptr, I64, I64], I64);
        let r = fb.local(I64);
        let b = fb.block();
        let n = fb.call(
            b,
            Callee::Ptr {
                target: copy_local(Local(0)),
                params: vec![I64, I64],
                ret: I64,
            },
            vec![copy_local(Local(1)), copy_local(Local(2))],
            Some(Place::local(r)),
        );
        fb.term(n, Terminator::Return(copy_local(r)));
        pb.add(fb.finish())
    };
    let (mut fb, b0) = main_fb();
    let fp = fb.local(Ptr);
    let r = fb.local(I64);
    let mut out = Out {
        fb: &mut fb,
        rt: &rt,
        cur: b0,
    };
    out.fb
        .assign(b0, fp, Rvalue::Use(Operand::Const(Const::Func(add), Ptr)));
    out.cur = out.fb.call(
        out.cur,
        Callee::Func(apply),
        vec![copy_local(fp), int(6, I64), int(7, I64)],
        Some(Place::local(r)),
    );
    out.line(copy_local(r), I64);
    out.cur = out.fb.call(
        out.cur,
        Callee::Func(apply),
        vec![
            Operand::Const(Const::Func(mul), Ptr),
            int(6, I64),
            int(7, I64),
        ],
        Some(Place::local(r)),
    );
    out.line(copy_local(r), I64);
    // Indirect call of an extern through its address: write_i64(1, -123)
    let cur = out.cur;
    out.fb.assign(
        cur,
        fp,
        Rvalue::Use(Operand::Const(Const::Extern(rt.write_i64), Ptr)),
    );
    out.cur = out.fb.call(
        cur,
        Callee::Ptr {
            target: copy_local(fp),
            params: vec![U32, I64],
            ret: Unit,
        },
        vec![int(1, U32), int(-123, I64)],
        None,
    );
    out.nl();
    let cur = out.cur;
    pb.add(finish_main(fb, cur, 0));
    TestProgram {
        name: "indirect",
        program: pb.p,
        stdout: "13\n42\n-123\n".into(),
        exit: 0,
    }
}
