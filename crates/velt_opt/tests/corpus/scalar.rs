//! Scalar programs: integer/float/bool operators and casts at every width (both on runtime
//! inputs and on constants the optimizer folds), and switches.

use super::*;
use velt_vir::vir::Ty::*;

pub fn cases() -> Vec<Case> {
    vec![
        ops_on_constants(),
        ops_on_params(),
        switches(),
        floats(),
        bool_logic(),
    ]
}

/// Print a battery of operations on `a`, `b` (I64 locals) at various widths.
fn emit_ops(env: &Env, fb: &mut FuncBuilder, mut b: BlockId, a: Local, bb: Local) -> BlockId {
    let narrow = |fb: &mut FuncBuilder, blk: BlockId, l: Local, ty: Ty| {
        let t = fb.local(ty);
        fb.assign(blk, t, Rvalue::Cast(copy_local(l), ty));
        copy_local(t)
    };
    let int_ops = [
        (I8, BinOp::Add),
        (I8, BinOp::Mul),
        (U16, BinOp::Sub),
        (I32, BinOp::Div),
        (I32, BinOp::Rem),
        (U8, BinOp::Div),
        (U8, BinOp::Shr),
        (I8, BinOp::Shr),
        (I32, BinOp::UShr),
        (I32, BinOp::Shl),
        (U64, BinOp::Mul),
        (I64, BinOp::Rem),
        (U32, BinOp::Lt),
        (I32, BinOp::Lt),
        (I16, BinOp::Ge),
        (U8, BinOp::BitXor),
        (I16, BinOp::BitAnd),
        (U32, BinOp::BitOr),
    ];
    for (ty, op) in int_ops {
        let (x, y) = (narrow(fb, b, a, ty), narrow(fb, b, bb, ty));
        let is_div = matches!(op, BinOp::Div | BinOp::Rem);
        if is_div {
            // Lowering guards division by zero; mirror that so the program never traps.
            let (z, ok, cont, skip) = (fb.local(Bool), fb.block(), fb.block(), fb.block());
            fb.assign(b, z, bin(BinOp::Eq, y.clone(), int(0, ty)));
            fb.branch(b, z, skip, ok);
            let rt = result_ty(op, ty);
            let after = env.show(fb, ok, bin(op, x, y), rt);
            fb.goto(after, cont);
            fb.goto(skip, cont);
            b = cont;
        } else {
            b = env.show(fb, b, bin(op, x, y), result_ty(op, ty));
        }
    }
    let x16 = narrow(fb, b, a, I16);
    b = env.show(fb, b, Rvalue::Unary(UnOp::Neg, x16.clone()), I16);
    b = env.show(fb, b, Rvalue::Unary(UnOp::BitNot, x16), I16);
    let f = narrow(fb, b, a, F64);
    let scaled = fb.local(F64);
    fb.assign(b, scaled, bin(BinOp::Mul, f.clone(), float(1e10, F64)));
    b = env.show(fb, b, Rvalue::Cast(copy_local(scaled), I32), I32);
    b = env.show(fb, b, Rvalue::Cast(f, U8), U8);
    let g = narrow(fb, b, bb, F32);
    let h = fb.local(F32);
    fb.assign(b, h, bin(BinOp::Add, g, float(16_777_216.0, F32)));
    b = env.show(fb, b, Rvalue::Cast(copy_local(h), I64), I64);
    let u = narrow(fb, b, a, U32);
    b = env.show(fb, b, Rvalue::Cast(u, I64), I64);
    b
}

fn result_ty(op: BinOp, ty: Ty) -> Ty {
    match op {
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Bool,
        _ => ty,
    }
}

const PAIRS: [(i64, i64); 7] = [
    (100, 100),
    (-1, 3),
    (i32::MIN as i64, -1),
    (-7, 3),
    (0x80, 35),
    ((1 << 40) + 5, 7),
    (12345, 0),
];

/// `velt_main` computing everything on constants: folded entirely by the optimizer, computed
/// at run time by the unoptimized program.
fn ops_on_constants() -> Case {
    let mut env = Env::new();
    let (mut fb, mut b) = main_fn();
    for (x, y) in PAIRS {
        let (a, bb) = (fb.local(I64), fb.local(I64));
        fb.assign(b, a, Rvalue::Use(int(x as i128, I64)));
        fb.assign(b, bb, Rvalue::Use(int(y as i128, I64)));
        b = emit_ops(&env, &mut fb, b, a, bb);
    }
    fb.ret(b, int(0, I32));
    env.pb.add(fb.finish());
    env.case("ops_on_constants", "velt_main", vec![vec![]])
}

fn ops_on_params() -> Case {
    let mut env = Env::new();
    let mut fb = FuncBuilder::export("ops", &[I64, I64], Unit);
    let (a, bb) = (fb.param(0), fb.param(1));
    let b = fb.block();
    let b = emit_ops(&env, &mut fb, b, a, bb);
    fb.ret(b, unit());
    env.pb.add(fb.finish());
    let inputs = PAIRS.iter().map(|&(x, y)| vec![arg(x), arg(y)]).collect();
    env.case("ops_on_params", "ops", inputs)
}

/// `classify(x)`: an I32 switch then an I8 switch on the truncated value; `velt_main` calls
/// it with constants (inlined, then both switches fold).
fn switches() -> Case {
    let mut env = Env::new();
    let mut fb = FuncBuilder::internal("classify", &[I32], I32);
    let x = fb.param(0);
    let (r, x8, r2) = (fb.local(I32), fb.local(I8), fb.local(I32));
    let b0 = fb.block();
    let arms: Vec<BlockId> = (0..5).map(|_| fb.block()).collect();
    let join = fb.block();
    fb.term(
        b0,
        Terminator::Switch {
            value: copy_local(x),
            cases: vec![(-1, arms[0]), (0, arms[1]), (5, arms[2]), (1000, arms[3])],
            default: arms[4],
        },
    );
    for (i, &arm) in arms.iter().enumerate() {
        fb.assign(arm, r, Rvalue::Use(int(10 * (i as i128 + 1), I32)));
        fb.goto(arm, join);
    }
    fb.assign(join, x8, Rvalue::Cast(copy_local(x), I8));
    let (k1, k2, kd, end) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.term(
        join,
        Terminator::Switch {
            value: copy_local(x8),
            cases: vec![(-56, k1), (7, k2)],
            default: kd,
        },
    );
    for (blk, add) in [(k1, 1), (k2, 2), (kd, 0)] {
        fb.assign(blk, r2, bin(BinOp::Add, copy_local(r), int(add, I32)));
        fb.goto(blk, end);
    }
    fb.ret(end, copy_local(r2));
    let classify = env.pb.add(fb.finish());

    let (mut fb, mut b) = main_fn();
    for v in [-1, 0, 5, 1000, 200, 7, 263, 99] {
        let t = fb.local(I32);
        b = fb.call(b, Callee::Func(classify), vec![int(v, I32)], Some(t));
        b = env.print(&mut fb, b, copy_local(t), I32);
    }
    fb.ret(b, int(0, I32));
    env.pb.add(fb.finish());
    env.case("switches", "velt_main", vec![vec![]])
}

/// `floats(n)`: f64 harmonic sum, f32 accumulation, NaN comparison, float→int casts.
fn floats() -> Case {
    let mut env = Env::new();
    let mut fb = FuncBuilder::export("floats", &[I64], I64);
    let n = fb.param(0);
    let (s, t, i, c, fi, q, ti) = (
        fb.local(F64),
        fb.local(F32),
        fb.local(I64),
        fb.local(Bool),
        fb.local(F64),
        fb.local(F64),
        fb.local(F32),
    );
    let (b0, head, body, exit) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(b0, s, Rvalue::Use(float(0.0, F64)));
    fb.assign(b0, t, Rvalue::Use(float(0.0, F32)));
    fb.assign(b0, i, Rvalue::Use(int(0, I64)));
    fb.goto(b0, head);
    fb.assign(head, c, bin(BinOp::Lt, copy_local(i), copy_local(n)));
    fb.branch(head, c, body, exit);
    fb.assign(body, fi, Rvalue::Cast(copy_local(i), F64));
    fb.assign(body, fi, bin(BinOp::Add, copy_local(fi), float(1.0, F64)));
    fb.assign(body, q, bin(BinOp::Div, float(1.0, F64), copy_local(fi)));
    fb.assign(body, s, bin(BinOp::Add, copy_local(s), copy_local(q)));
    fb.assign(body, ti, Rvalue::Cast(copy_local(i), F32));
    fb.assign(body, ti, bin(BinOp::Mul, copy_local(ti), float(0.5, F32)));
    fb.assign(body, t, bin(BinOp::Add, copy_local(t), copy_local(ti)));
    fb.assign(body, i, bin(BinOp::Add, copy_local(i), int(1, I64)));
    fb.goto(body, head);
    let (r, rt, nan, ne, nei) = (
        fb.local(I64),
        fb.local(I64),
        fb.local(F64),
        fb.local(Bool),
        fb.local(I64),
    );
    fb.assign(exit, s, bin(BinOp::Mul, copy_local(s), float(1000.0, F64)));
    fb.assign(exit, r, Rvalue::Cast(copy_local(s), I64));
    fb.assign(exit, rt, Rvalue::Cast(copy_local(t), I64));
    fb.assign(exit, r, bin(BinOp::Add, copy_local(r), copy_local(rt)));
    fb.assign(exit, nan, bin(BinOp::Div, float(0.0, F64), float(0.0, F64)));
    fb.assign(exit, ne, bin(BinOp::Ne, copy_local(nan), copy_local(nan)));
    fb.assign(exit, nei, Rvalue::Cast(copy_local(ne), I64));
    fb.assign(exit, r, bin(BinOp::Add, copy_local(r), copy_local(nei)));
    let next = fb.call(
        exit,
        Callee::Extern(env.write_f64),
        vec![copy_local(s)],
        None,
    );
    fb.ret(next, copy_local(r));
    env.pb.add(fb.finish());
    let inputs = [0, 1, 10, 1000].iter().map(|&v| vec![arg(v)]).collect();
    env.case("floats", "floats", inputs)
}

/// `bool_logic(a, b)`: Not/BitAnd/BitOr/BitXor on bools, Bool→int casts, switch on a widened Bool.
fn bool_logic() -> Case {
    let mut env = Env::new();
    let mut fb = FuncBuilder::export("bool_logic", &[I64, I64], I32);
    let (a, bb) = (fb.param(0), fb.param(1));
    let cs: Vec<Local> = (0..6).map(|_| fb.local(Bool)).collect();
    let (r, t) = (fb.local(I32), fb.local(I32));
    let b = fb.block();
    fb.assign(b, cs[0], bin(BinOp::Lt, copy_local(a), copy_local(bb)));
    fb.assign(b, cs[1], Rvalue::Unary(UnOp::Not, copy_local(cs[0])));
    fb.assign(b, cs[2], bin(BinOp::Eq, copy_local(a), int(3, I64)));
    fb.assign(
        b,
        cs[3],
        bin(BinOp::BitAnd, copy_local(cs[0]), copy_local(cs[2])),
    );
    fb.assign(
        b,
        cs[4],
        bin(BinOp::BitOr, copy_local(cs[1]), copy_local(cs[3])),
    );
    fb.assign(
        b,
        cs[5],
        bin(BinOp::BitXor, copy_local(cs[4]), copy_local(cs[2])),
    );
    fb.assign(b, r, Rvalue::Use(int(0, I32)));
    for (k, weight) in [(5, 4), (3, 2), (0, 1)] {
        fb.assign(b, t, Rvalue::Cast(copy_local(cs[k]), I32));
        fb.assign(b, t, bin(BinOp::Mul, copy_local(t), int(weight, I32)));
        fb.assign(b, r, bin(BinOp::Add, copy_local(r), copy_local(t)));
    }
    let (yes, no) = (fb.block(), fb.block());
    // VIR switches on integers only: widen the Bool first.
    let w = fb.local(I32);
    fb.assign(b, w, Rvalue::Cast(copy_local(cs[1]), I32));
    fb.term(
        b,
        Terminator::Switch {
            value: copy_local(w),
            cases: vec![(1, yes)],
            default: no,
        },
    );
    fb.assign(yes, r, bin(BinOp::Add, copy_local(r), int(100, I32)));
    fb.ret(yes, copy_local(r));
    fb.ret(no, copy_local(r));
    let id = env.pb.add(fb.finish());
    let (mut fb, mut b) = main_fn();
    for (x, y) in [(1, 2), (3, 5), (3, 1), (7, 7)] {
        let t = fb.local(I32);
        b = fb.call(b, Callee::Func(id), vec![int(x, I64), int(y, I64)], Some(t));
        b = env.print(&mut fb, b, copy_local(t), I32);
    }
    fb.ret(b, int(0, I32));
    env.pb.add(fb.finish());
    let inputs = [(1, 2), (3, 5), (3, 1), (-4, -9)]
        .iter()
        .map(|&(x, y)| vec![arg(x), arg(y)])
        .collect();
    env.case("bool_logic", "bool_logic", inputs)
        .with("velt_main", vec![vec![]])
}
