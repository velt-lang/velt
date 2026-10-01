//! Memory programs: aggregates passed by pointer (getters/setters, out-pointers), `MemCopy`,
//! address-taken scalars, enum variant views (`Proj::Cast`), statics, function pointers.

use super::*;
use velt_vir::vir::Ty::*;

pub fn cases() -> Vec<Case> {
    vec![
        getters(),
        address_taken_scalar(),
        out_pointer(),
        enum_views(),
        static_bytes(),
        indirect_calls(),
    ]
}

fn fn_ptr(id: FuncId) -> Operand {
    Operand::Const(Const::Func(id), Ptr)
}

/// Point { x: I64, y: I64 } on main's stack, read/written through getter/setter helpers.
fn getters() -> Case {
    let mut env = Env::new();
    let point = env.pb.agg("Point", 16, 8, &[(I64, 0), (I64, 8)]);
    let mut getters = vec![];
    for field in 0..2 {
        let mut fb = FuncBuilder::internal(&format!("get{field}"), &[Ptr], I64);
        let p = fb.param(0);
        let r = fb.local(I64);
        let b = fb.block();
        let place = Place {
            local: p,
            proj: vec![Proj::Deref(Ty::Agg(point)), Proj::Field(field)],
        };
        fb.assign(b, r, Rvalue::Use(copy_place(place)));
        fb.ret(b, copy_local(r));
        getters.push(env.pb.add(fb.finish()));
    }
    let mut fb = FuncBuilder::internal("set_x", &[Ptr, I64], Unit);
    let (p, v) = (fb.param(0), fb.param(1));
    let b = fb.block();
    let place = Place {
        local: p,
        proj: vec![Proj::Deref(Ty::Agg(point)), Proj::Field(0)],
    };
    fb.push(b, Stmt::Assign(place, Rvalue::Use(copy_local(v))));
    fb.ret(b, unit());
    let set_x = env.pb.add(fb.finish());

    let mut fb = FuncBuilder::export("points", &[I64], I64);
    let a = fb.param(0);
    let (pt, p, x, y, s) = (
        fb.local(Ty::Agg(point)),
        fb.local(Ptr),
        fb.local(I64),
        fb.local(I64),
        fb.local(I64),
    );
    let b = fb.block();
    fb.assign(
        b,
        pt,
        Rvalue::Aggregate(point, vec![copy_local(a), int(4, I64)]),
    );
    fb.assign(b, p, Rvalue::AddrOf(Place::local(pt)));
    let b = fb.call(b, Callee::Func(getters[0]), vec![copy_local(p)], Some(x));
    let b = fb.call(b, Callee::Func(getters[1]), vec![copy_local(p)], Some(y));
    fb.assign(b, s, bin(BinOp::Add, copy_local(x), copy_local(y)));
    let b = fb.call(
        b,
        Callee::Func(set_x),
        vec![copy_local(p), copy_local(s)],
        None,
    );
    fb.assign(
        b,
        s,
        bin(BinOp::Mul, copy_place(field(pt, 0)), int(10, I64)),
    );
    fb.assign(
        b,
        s,
        bin(BinOp::Add, copy_local(s), copy_place(field(pt, 1))),
    );
    fb.ret(b, copy_local(s));
    env.pb.add(fb.finish());
    env.case("getters", "points", vec![vec![arg(3)], vec![arg(-8)]])
}

/// `x = 1; p = &x; inc(p); print x; x = x + 5; print x; y = 2 (never escapes)`.
fn address_taken_scalar() -> Case {
    let mut env = Env::new();
    let mut fb = FuncBuilder::internal("inc", &[Ptr], Unit);
    let p = fb.param(0);
    let b = fb.block();
    fb.push(
        b,
        Stmt::Assign(
            deref(p, I64),
            bin(BinOp::Add, copy_place(deref(p, I64)), int(1, I64)),
        ),
    );
    fb.ret(b, unit());
    let inc = env.pb.add(fb.finish());
    let (mut fb, b) = main_fn();
    let (x, p, y) = (fb.local(I64), fb.local(Ptr), fb.local(I64));
    fb.assign(b, x, Rvalue::Use(int(1, I64)));
    fb.assign(b, y, Rvalue::Use(int(2, I64)));
    fb.assign(b, p, Rvalue::AddrOf(Place::local(x)));
    let b = fb.call(b, Callee::Func(inc), vec![copy_local(p)], None);
    let b = fb.call(b, Callee::Func(inc), vec![copy_local(p)], None);
    let b = env.print(&mut fb, b, copy_local(x), I64);
    fb.assign(b, x, bin(BinOp::Add, copy_local(x), copy_local(y)));
    let b = env.print(&mut fb, b, copy_place(deref(p, I64)), I64);
    fb.ret(b, int(0, I32));
    env.pb.add(fb.finish());
    env.case("address_taken_scalar", "velt_main", vec![vec![]])
}

/// `make(a, out)` writes an aggregate through an out-pointer; the caller `MemCopy`s it.
fn out_pointer() -> Case {
    let mut env = Env::new();
    let pair = env.pb.agg("Pair", 16, 8, &[(I32, 0), (I64, 8)]);
    let mut fb = FuncBuilder::internal("make", &[I64, Ptr], Unit);
    let (a, out) = (fb.param(0), fb.param(1));
    let (t, m) = (fb.local(I32), fb.local(I64));
    let b = fb.block();
    fb.assign(b, t, Rvalue::Cast(copy_local(a), I32));
    fb.assign(b, m, bin(BinOp::Mul, copy_local(a), copy_local(a)));
    let place = Place {
        local: out,
        proj: vec![Proj::Deref(Ty::Agg(pair))],
    };
    fb.push(
        b,
        Stmt::Assign(
            place,
            Rvalue::Aggregate(pair, vec![copy_local(t), copy_local(m)]),
        ),
    );
    fb.ret(b, unit());
    let make = env.pb.add(fb.finish());
    let mut fb = FuncBuilder::export("pairs", &[I64], I64);
    let a = fb.param(0);
    let (p1, p2, q1, q2, s, w) = (
        fb.local(Ty::Agg(pair)),
        fb.local(Ty::Agg(pair)),
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(I64),
        fb.local(I64),
    );
    let b = fb.block();
    fb.assign(b, q1, Rvalue::AddrOf(Place::local(p1)));
    fb.assign(b, q2, Rvalue::AddrOf(Place::local(p2)));
    let b = fb.call(
        b,
        Callee::Func(make),
        vec![copy_local(a), copy_local(q1)],
        None,
    );
    fb.push(
        b,
        Stmt::MemCopy {
            dst: copy_local(q2),
            src: copy_local(q1),
            size: 16,
        },
    );
    fb.assign(b, w, Rvalue::Cast(copy_place(field(p2, 0)), I64));
    fb.assign(
        b,
        s,
        bin(BinOp::Add, copy_local(w), copy_place(field(p2, 1))),
    );
    fb.ret(b, copy_local(s));
    env.pb.add(fb.finish());
    env.case(
        "out_pointer",
        "pairs",
        vec![vec![arg(6)], vec![arg(1 << 33)]],
    )
}

/// Shape = { tag: U8, pad, I64 payload } with views Circle { tag, r: I64 } and
/// Rect { tag, w: I32, h: I32 }; `area(kind, v)` builds one and matches on the tag.
fn enum_views() -> Case {
    let mut env = Env::new();
    let shape = env.pb.agg("Shape", 16, 8, &[(U8, 0), (I64, 8)]);
    let circle = env.pb.agg("Shape::Circle", 16, 8, &[(U8, 0), (I64, 8)]);
    let rect = env
        .pb
        .agg("Shape::Rect", 16, 8, &[(U8, 0), (I32, 8), (I32, 12)]);
    let mut fb = FuncBuilder::export("area", &[I64, I64], I64);
    let (kind, v) = (fb.param(0), fb.param(1));
    let (sh, c, v32, r) = (
        fb.local(Ty::Agg(shape)),
        fb.local(Bool),
        fb.local(I32),
        fb.local(I64),
    );
    let view = |agg, n| Place {
        local: sh,
        proj: vec![Proj::Cast(agg), Proj::Field(n)],
    };
    let (b0, mk_c, mk_r, matched) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(b0, c, bin(BinOp::Eq, copy_local(kind), int(0, I64)));
    fb.branch(b0, c, mk_c, mk_r);
    fb.push(mk_c, Stmt::Assign(view(circle, 0), Rvalue::Use(int(0, U8))));
    fb.push(
        mk_c,
        Stmt::Assign(view(circle, 1), Rvalue::Use(copy_local(v))),
    );
    fb.goto(mk_c, matched);
    fb.assign(mk_r, v32, Rvalue::Cast(copy_local(v), I32));
    fb.push(mk_r, Stmt::Assign(view(rect, 0), Rvalue::Use(int(1, U8))));
    fb.push(
        mk_r,
        Stmt::Assign(view(rect, 1), Rvalue::Use(copy_local(v32))),
    );
    fb.push(
        mk_r,
        Stmt::Assign(view(rect, 2), bin(BinOp::Add, copy_local(v32), int(2, I32))),
    );
    fb.goto(mk_r, matched);
    let (arm_c, arm_r, other) = (fb.block(), fb.block(), fb.block());
    fb.term(
        matched,
        Terminator::Switch {
            value: copy_place(field(sh, 0)),
            cases: vec![(0, arm_c), (1, arm_r)],
            default: other,
        },
    );
    fb.assign(
        arm_c,
        r,
        bin(
            BinOp::Mul,
            copy_place(view(circle, 1)),
            copy_place(view(circle, 1)),
        ),
    );
    fb.assign(arm_c, r, bin(BinOp::Mul, copy_local(r), int(3, I64)));
    fb.ret(arm_c, copy_local(r));
    fb.assign(
        arm_r,
        v32,
        bin(
            BinOp::Mul,
            copy_place(view(rect, 1)),
            copy_place(view(rect, 2)),
        ),
    );
    fb.assign(arm_r, r, Rvalue::Cast(copy_local(v32), I64));
    fb.ret(arm_r, copy_local(r));
    fb.term(other, Terminator::Unreachable);
    env.pb.add(fb.finish());
    let inputs = [(0, 5), (1, 5), (1, -3), (0, 1 << 20)]
        .iter()
        .map(|&(k, v)| vec![arg(k), arg(v)])
        .collect();
    env.case("enum_views", "area", inputs)
}

/// Sum the first `n` bytes of a static through `PtrAdd` + `Deref`.
fn static_bytes() -> Case {
    let mut env = Env::new();
    let data = env.pb.stat(b"hello, velt!", 1);
    let mut fb = FuncBuilder::export("bytes", &[I64], I64);
    let n = fb.param(0);
    let (i, s, p, byte, w, c) = (
        fb.local(I64),
        fb.local(I64),
        fb.local(Ptr),
        fb.local(U8),
        fb.local(I64),
        fb.local(Bool),
    );
    let (b0, head, body, exit) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(b0, i, Rvalue::Use(int(0, I64)));
    fb.assign(b0, s, Rvalue::Use(int(0, I64)));
    fb.goto(b0, head);
    fb.assign(head, c, bin(BinOp::Lt, copy_local(i), copy_local(n)));
    fb.branch(head, c, body, exit);
    fb.assign(
        body,
        p,
        bin(
            BinOp::PtrAdd,
            Operand::Const(Const::Static(data), Ptr),
            copy_local(i),
        ),
    );
    fb.assign(body, byte, Rvalue::Use(copy_place(deref(p, U8))));
    fb.assign(body, w, Rvalue::Cast(copy_local(byte), I64));
    fb.assign(body, s, bin(BinOp::Add, copy_local(s), copy_local(w)));
    fb.assign(body, i, bin(BinOp::Add, copy_local(i), int(1, I64)));
    fb.goto(body, head);
    fb.ret(exit, copy_local(s));
    env.pb.add(fb.finish());
    env.case(
        "static_bytes",
        "bytes",
        vec![vec![arg(0)], vec![arg(5)], vec![arg(12)]],
    )
}

/// Calls through function pointers: a constant target (devirtualized, then inlined), a
/// target chosen at run time from a table in an aggregate, and an extern pointer.
fn indirect_calls() -> Case {
    let mut env = Env::new();
    let table = env.pb.agg("Table", 16, 8, &[(Ptr, 0), (Ptr, 8)]);
    let mut fns = vec![];
    for (name, k) in [("double", 2), ("triple", 3)] {
        let mut fb = FuncBuilder::internal(name, &[I64], I64);
        let r = fb.local(I64);
        let b = fb.block();
        fb.assign(b, r, bin(BinOp::Mul, copy_local(fb.param(0)), int(k, I64)));
        fb.ret(b, copy_local(r));
        fns.push(env.pb.add(fb.finish()));
    }
    let sig = |target| Callee::Ptr {
        target,
        params: vec![I64],
        ret: I64,
    };
    let mut fb = FuncBuilder::export("dispatch", &[I64, I64], I64);
    let (which, v) = (fb.param(0), fb.param(1));
    let (t, fp, c, r1, r2, s) = (
        fb.local(Ty::Agg(table)),
        fb.local(Ptr),
        fb.local(Bool),
        fb.local(I64),
        fb.local(I64),
        fb.local(I64),
    );
    let (b0, pick0, pick1, call) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(
        b0,
        t,
        Rvalue::Aggregate(table, vec![fn_ptr(fns[0]), fn_ptr(fns[1])]),
    );
    fb.assign(b0, c, bin(BinOp::Eq, copy_local(which), int(0, I64)));
    fb.branch(b0, c, pick0, pick1);
    fb.assign(pick0, fp, Rvalue::Use(copy_place(field(t, 0))));
    fb.goto(pick0, call);
    fb.assign(pick1, fp, Rvalue::Use(copy_place(field(t, 1))));
    fb.goto(pick1, call);
    let b = fb.call(call, sig(copy_local(fp)), vec![copy_local(v)], Some(r1));
    let direct = fb.local(Ptr);
    fb.assign(b, direct, Rvalue::Use(fn_ptr(fns[1])));
    let b = fb.call(b, sig(copy_local(direct)), vec![copy_local(r1)], Some(r2));
    let ext = Callee::Ptr {
        target: Operand::Const(Const::Extern(env.write_i64), Ptr),
        params: vec![I64],
        ret: Unit,
    };
    let b = fb.call(b, ext, vec![copy_local(r2)], None);
    fb.assign(b, s, bin(BinOp::Add, copy_local(r1), copy_local(r2)));
    fb.ret(b, copy_local(s));
    env.pb.add(fb.finish());
    let inputs = [(0, 5), (1, 5), (7, -2)]
        .iter()
        .map(|&(w, v)| vec![arg(w), arg(v)])
        .collect();
    env.case("indirect_calls", "dispatch", inputs)
}
