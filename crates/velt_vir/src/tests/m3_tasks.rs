//! `tests/golden/m3/tasks.vlt` hand-lowered to HIR: spawned compiled calls sharing an atomic
//! counter, `Promise.all` over join handles, `shared(new Mutex(...))` with `with`, and spawned
//! async closure literals that own a cloned `shared` value. Plus `with` on a scalar value.

use velt_sema::hir::{BinOp as B, Capture, Intrinsic as I, PassMode};

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

/// `const xs = ["a"]; const f = async () => { console.log(xs.length); }; const s = shared(f);`
/// with `f` a local async closure (`FnDef::shares_captures`, which sema never sets for a closure
/// that reaches `shared`): its calls would share the counted `xs` from several threads, so the
/// many-threads check panics (glue/transfer_env.rs). The same closure copying its captures per
/// call is fine.
#[test]
fn shared_local_async_closure_panics() {
    for shares in [true, false] {
        let mut pb = PB::new();
        let t = pb.t;
        let sa = pb.arr(t.str);
        let pv = pb.promise(t.unit);
        let fty = pb.fn_ty(vec![], pv);
        let sf = pb.shared(fty);
        let mut f = FB::new("main", pv);
        let xs = f.local("xs", sa);
        let g = f.local("g", fty);
        let s_ = f.local("s", sf);
        let clo = {
            let mut c = FB::new("main::{closure#0}", pv);
            let xin = c.param("xs", sa, PassMode::Owned);
            c.captures.push(Capture {
                outer: xs,
                inner: xin,
                mode: PassMode::Owned,
                share: true,
            });
            let body = vec![se(print(
                vec![intr(I::ArrayLen, vec![c.bw(xin)], t.usize)],
                t,
            ))];
            let mut d = c.build_async(body);
            d.shares_captures = shares;
            pb.add_fn(d)
        };
        let body = vec![
            let_(xs, array(vec![s("a", t)], sa)),
            let_(g, closure(clo, fty)),
            let_(s_, intr(I::SharedNew, vec![f.mv(g)], sf)),
            se(print(vec![intr(I::ArrayLen, vec![f.bw(xs)], t.usize)], t)),
        ];
        pb.add_main(f.build_async(body));
        let out = run(&pb.finish());
        if shares {
            assert_eq!(out.code, 101, "{}", out.stderr);
            assert!(
                out.stderr
                    .contains("an async closure that changes or shares what it captured"),
                "{}",
                out.stderr
            );
        } else {
            assert_eq!(
                (out.stdout.as_str(), out.code),
                ("1\n", 0),
                "{}",
                out.stderr
            );
        }
    }
}
