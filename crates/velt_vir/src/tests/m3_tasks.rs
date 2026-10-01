//! `tests/golden/m3/tasks.vlt` hand-lowered to HIR: spawned compiled calls sharing an atomic
//! counter, `Promise.all` over join handles, `shared(new Mutex(...))` with `with`, and spawned
//! async closure literals that own a cloned `shared` value. Plus `with` on a scalar value.

use velt_sema::hir::{BinOp as B, Intrinsic as I};

use super::builder::*;
use super::builder_m2::*;
use super::programs_m3::{mutex_def, tasks};
use super::{m3_golden, run};

#[test]
fn tasks_golden() {
    let out = run(&tasks());
    assert_eq!(out.stdout, m3_golden("tasks"));
}

/// `const m = new Mutex<i64>(1); m.with((v) => { v += 41; }); console.log(m.with((v) => v));`
/// — the callback updates the locked scalar in place (it gets a pointer to it).
#[test]
fn mutex_with_updates_scalar_value() {
    let mut pb = PB::new();
    let t = pb.t;
    let mutex = mutex_def(&mut pb);
    let mi = pb.adt_ty(mutex, vec![t.i64]);
    let (inc_ty, get_ty) = (pb.fn_ty(vec![t.i64], t.unit), pb.fn_ty(vec![t.i64], t.i64));
    let inc = closure_def(
        &mut pb,
        "main::{closure#0}",
        t.unit,
        &[],
        &[("v", t.i64)],
        |f, _, ps| vec![se(cassign(B::Add, f.bm(ps[0]), int(41, t.i64), t))],
    );
    let get = closure_def(
        &mut pb,
        "main::{closure#1}",
        t.i64,
        &[],
        &[("v", t.i64)],
        |f, _, ps| vec![ret(Some(f.cp(ps[0])))],
    );
    let mut f = FB::new("main", t.unit);
    let m = f.local("m", mi);
    let body = vec![
        let_(m, intr(I::MutexNew, vec![int(1, t.i64)], mi)),
        se(intr(
            I::MutexWith,
            vec![f.bw(m), closure(inc, inc_ty)],
            t.unit,
        )),
        se(print(
            vec![intr(
                I::MutexWith,
                vec![f.bw(m), closure(get, get_ty)],
                t.i64,
            )],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "42\n");
}
