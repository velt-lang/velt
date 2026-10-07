//! Frame environments for closures only ever called where they are created
//! (`lower/frame_envs.rs`, #34): which shapes qualify, and that their owned captures are dropped
//! with the closure (the interpreter checks for leaks).

use velt_sema::hir::{BinOp as B, ExprKind, PassMode, Program};

use super::builder::*;
use super::builder_m2::*;
use super::{lower_ok, run};

/// How `main` uses the closure `(x) => x * k` (capturing `k` by copy) it creates.
#[derive(Clone, Copy)]
enum Shape {
    /// `const f = …; f(1); f(2)`.
    Called,
    /// `(…)(1)`.
    Immediate,
    /// `const f = …; const fs = [f]; fs[0](1)`.
    Stored,
    /// `const f = …; const g = () => f(1); g()`.
    Captured,
    /// `let f = …; f = …; f(1)`.
    Reassigned,
}

fn program(shape: Shape) -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let i = t.i64;
    let i2i = pb.fn_ty(vec![i], i);
    let v2i = pb.fn_ty(vec![], i);
    let arr = pb.arr(i2i);
    let mut f = FB::new("main", t.unit);
    let k = f.local("k", i);
    let fl = f.local("f", i2i);
    let other = f.local("other", arr);
    let g = f.local("g", v2i);
    let scale = |pb: &mut PB, n: u32| {
        closure_def(
            pb,
            &format!("main::{{closure#{n}}}"),
            i,
            &[(k, i, PassMode::Copy)],
            &[("x", i)],
            |f, caps, ps| vec![ret(Some(bin(B::Mul, f.cp(ps[0]), f.cp(caps[0]))))],
        )
    };
    let c = scale(&mut pb, 0);
    let mut body = vec![let_(k, int(3, i))];
    let call_f = |f: &FB, v: u128| call_ptr(f.bw(fl), vec![int(v, i)], i);
    match shape {
        Shape::Called => {
            body.push(let_(fl, closure(c, i2i)));
            body.push(se(print(vec![call_f(&f, 1), call_f(&f, 2)], t)));
        }
        Shape::Immediate => {
            body.push(se(print(
                vec![call_ptr(closure(c, i2i), vec![int(1, i)], i)],
                t,
            )));
        }
        Shape::Stored => {
            body.push(let_(fl, closure(c, i2i)));
            body.push(let_(other, array(vec![f.mv(fl)], arr)));
            let elem = ex(
                ExprKind::Index {
                    base: Box::new(f.bw(other)),
                    index: Box::new(int(0, t.usize)),
                    mode: velt_sema::hir::UseMode::Borrow,
                },
                i2i,
            );
            body.push(se(print(vec![call_ptr(elem, vec![int(1, i)], i)], t)));
        }
        Shape::Captured => {
            body.push(let_(fl, closure(c, i2i)));
            let gc = closure_def(
                &mut pb,
                "main::{closure#1}",
                i,
                &[(fl, i2i, PassMode::Borrow)],
                &[],
                |f, caps, _| vec![ret(Some(call_ptr(f.bw(caps[0]), vec![int(1, i)], i)))],
            );
            body.push(let_(g, closure(gc, v2i)));
            body.push(se(print(vec![call_ptr(f.bw(g), vec![], i)], t)));
        }
        Shape::Reassigned => {
            let c2 = scale(&mut pb, 1);
            body.push(let_(fl, closure(c, i2i)));
            body.push(se(assign(f.bm(fl), closure(c2, i2i), t)));
            body.push(se(print(vec![call_f(&f, 1)], t)));
        }
    }
    pb.add_main(f.build(body));
    pb.finish()
}

/// Does `main` allocate (a heap environment)?
fn main_allocates(p: &Program) -> bool {
    let vir = lower_ok(p).to_string();
    let start = vir.find("_V4main(").expect("main in the VIR");
    let rest = &vir[start..];
    rest[..rest.find("\n}\n").unwrap_or(rest.len())].contains("velt_rt_alloc")
}

#[test]
fn only_called_closures_get_frame_envs() {
    for shape in [Shape::Called, Shape::Immediate] {
        let p = program(shape);
        assert!(!main_allocates(&p));
        run(&p);
    }
}

#[test]
fn closures_that_may_outlive_the_call_keep_heap_envs() {
    for shape in [Shape::Stored, Shape::Captured, Shape::Reassigned] {
        let p = program(shape);
        assert!(main_allocates(&p));
        run(&p);
    }
}

/// A frame env that owns a capture (a string moved in) drops it with the closure.
#[test]
fn owned_captures_of_frame_envs_are_dropped() {
    let mut pb = PB::new();
    let t = pb.t;
    let v2s = pb.fn_ty(vec![], t.str);
    let mut f = FB::new("main", t.unit);
    let name = f.local("name", t.str);
    let greet = f.local("greet", v2s);
    let c = closure_def(
        &mut pb,
        "main::{closure#0}",
        t.str,
        &[(name, t.str, PassMode::Owned)],
        &[],
        |f, caps, _| vec![ret(Some(concat(s("hi ", t), f.bw(caps[0]), t)))],
    );
    let body = vec![
        let_(name, concat(s("ve", t), s("lt", t), t)),
        let_(greet, closure(c, v2s)),
        se(print(vec![call_ptr(f.bw(greet), vec![], t.str)], t)),
        se(print(vec![call_ptr(f.bw(greet), vec![], t.str)], t)),
    ];
    pb.add_main(f.build(body));
    let p = pb.finish();
    let vir = lower_ok(&p).to_string();
    assert!(vir.contains("_Genv_drop_frame_"), "{vir}");
    assert_eq!(run(&p).stdout, "hi velt\nhi velt\n");
}
