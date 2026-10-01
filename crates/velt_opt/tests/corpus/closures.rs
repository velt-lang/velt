//! Closure programs: a `{ code, env }` value passed by pointer to a recursive fold that calls
//! through `(*f).0` (constant-field propagation + specialization), and struct temporaries
//! copied between locals (scalar replacement).

use super::*;
use velt_vir::vir::Ty::*;

pub fn cases() -> Vec<Case> {
    vec![closure_fold()]
}

/// `(*p as agg).n`
fn through(p: Local, agg: AggId, n: u32) -> Place {
    Place {
        local: p,
        proj: vec![Proj::Deref(Ty::Agg(agg)), Proj::Field(n)],
    }
}

/// `run(k, a)`: folds `[a, a + 1, 7]` with `acc + x * k` starting from `a + k` (built in a
/// struct that is copied twice), printing every step.
fn closure_fold() -> Case {
    let mut env = Env::new();
    let clo = env.pb.agg("closure", 16, 8, &[(Ptr, 0), (Ptr, 8)]);
    let arr = env.pb.agg("arr3", 24, 8, &[(I64, 0), (I64, 8), (I64, 16)]);
    let pair = env.pb.agg("pair", 16, 8, &[(I64, 0), (I64, 8)]);

    let mut fb = FuncBuilder::internal("mul_env", &[Ptr, I64, I64], I64);
    let (e, acc, x) = (fb.param(0), fb.param(1), fb.param(2));
    let (m, r) = (fb.local(I64), fb.local(I64));
    let b = fb.block();
    fb.assign(
        b,
        m,
        bin(BinOp::Mul, copy_local(x), copy_place(deref(e, I64))),
    );
    fb.assign(b, r, bin(BinOp::Add, copy_local(acc), copy_local(m)));
    fb.ret(b, copy_local(r));
    let mul_env = env.pb.add(fb.finish());

    let fold = env.pb.reserve();
    let mut fb = FuncBuilder::internal("fold", &[Ptr, Ptr, I64, I64], I64);
    let (xs, f, i, acc) = (fb.param(0), fb.param(1), fb.param(2), fb.param(3));
    let (done, off, p, code, cenv, next, i1, r) = (
        fb.local(Bool),
        fb.local(I64),
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(I64),
        fb.local(I64),
        fb.local(I64),
    );
    let (b0, b1, b2) = (fb.block(), fb.block(), fb.block());
    fb.assign(b0, done, bin(BinOp::Ge, copy_local(i), int(3, I64)));
    fb.branch(b0, done, b1, b2);
    fb.ret(b1, copy_local(acc));
    fb.assign(b2, off, bin(BinOp::Mul, copy_local(i), int(8, I64)));
    fb.assign(b2, p, bin(BinOp::PtrAdd, copy_local(xs), copy_local(off)));
    fb.assign(b2, code, Rvalue::Use(copy_place(through(f, clo, 0))));
    fb.assign(b2, cenv, Rvalue::Use(copy_place(through(f, clo, 1))));
    let callee = Callee::Ptr {
        target: copy_local(code),
        params: vec![Ptr, I64, I64],
        ret: I64,
    };
    let args = vec![copy_local(cenv), copy_local(acc), copy_place(deref(p, I64))];
    let b = fb.call(b2, callee, args, Some(next));
    let b = env.print(&mut fb, b, copy_local(next), I64);
    fb.assign(b, i1, bin(BinOp::Add, copy_local(i), int(1, I64)));
    let args = vec![
        copy_local(xs),
        copy_local(f),
        copy_local(i1),
        copy_local(next),
    ];
    let b = fb.call(b, Callee::Func(fold), args, Some(r));
    fb.ret(b, copy_local(r));
    env.pb.set(fold, fb.finish());

    let mut fb = FuncBuilder::export("run", &[I64, I64], I64);
    let (k, a) = (fb.param(0), fb.param(1));
    let (kl, kp, c, cp) = (
        fb.local(I64),
        fb.local(Ptr),
        fb.local(Agg(clo)),
        fb.local(Ptr),
    );
    let (v, vp, a1) = (fb.local(Agg(arr)), fb.local(Ptr), fb.local(I64));
    let (s1, s2, start, r) = (
        fb.local(Agg(pair)),
        fb.local(Agg(pair)),
        fb.local(I64),
        fb.local(I64),
    );
    let b = fb.block();
    fb.assign(b, kl, Rvalue::Use(copy_local(k)));
    fb.assign(b, kp, Rvalue::AddrOf(Place::local(kl)));
    let code = Operand::Const(Const::Func(mul_env), Ptr);
    fb.assign(b, c, Rvalue::Aggregate(clo, vec![code, copy_local(kp)]));
    fb.assign(b, cp, Rvalue::AddrOf(Place::local(c)));
    fb.assign(b, a1, bin(BinOp::Add, copy_local(a), int(1, I64)));
    let elems = vec![copy_local(a), copy_local(a1), int(7, I64)];
    fb.assign(b, v, Rvalue::Aggregate(arr, elems));
    fb.assign(b, vp, Rvalue::AddrOf(Place::local(v)));
    fb.assign(
        b,
        s1,
        Rvalue::Aggregate(pair, vec![copy_local(a), copy_local(k)]),
    );
    fb.assign(b, s2, Rvalue::Use(copy_local(s1)));
    let (f0, f1) = (copy_place(field(s2, 0)), copy_place(field(s2, 1)));
    fb.assign(b, start, bin(BinOp::Add, f0, f1));
    let args = vec![
        copy_local(vp),
        copy_local(cp),
        int(0, I64),
        copy_local(start),
    ];
    let b = fb.call(b, Callee::Func(fold), args, Some(r));
    fb.ret(b, copy_local(r));
    env.pb.add(fb.finish());
    let inputs = [(2, 3), (0, -5), (10, 100)]
        .iter()
        .map(|&(k, a)| vec![arg(k), arg(a)])
        .collect();
    env.case("closure_fold", "run", inputs)
}
