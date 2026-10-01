//! Integer/float operator lowering: checked division, `MIN / -1`, `**` and `as` casts.

use velt_sema::hir::BinOp as B;

use super::builder::*;
use super::{lower_ok, run};
use crate::vir::{self, Callee, Terminator};

fn contains_panic_call(f: &vir::Function, p: &vir::Program) -> bool {
    f.blocks.iter().any(|b| match &b.term {
        Terminator::Call {
            callee: Callee::Extern(e),
            ..
        } => p.externs[e.0 as usize].symbol == "velt_rt_panic",
        _ => false,
    })
}

#[test]
fn division_by_zero_panics() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("main", t.unit);
    let z = f.local("z", t.i64);
    let body = vec![
        let_(z, int(0, t.i64)),
        se(print(vec![s("before", t)], t)),
        se(print(vec![bin(B::Div, int(10, t.i64), f.cp(z))], t)),
        se(print(vec![s("after", t)], t)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "before\n");
    assert_eq!(out.stderr, "panic: division by zero\n");
    assert_eq!(out.code, 101);
}

#[test]
fn remainder_by_zero_unsigned_panics() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("main", t.unit);
    let z = f.local("z", t.u8);
    let body = vec![
        let_(z, int(0, t.u8)),
        se(print(vec![bin(B::Rem, int(10, t.u8), f.cp(z))], t)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(out.stderr, "panic: division by zero\n");
    assert_eq!(out.code, 101);
}

#[test]
fn signed_min_div_minus_one_wraps() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("main", t.unit);
    let m = f.local("m", t.i64);
    let d = f.local("d", t.i64);
    let body = vec![
        let_(m, neg(int(1u128 << 63, t.i64))),
        let_(d, neg(int(1, t.i64))),
        se(print(
            vec![
                f.cp(m),
                bin(B::Div, f.cp(m), f.cp(d)),
                bin(B::Rem, f.cp(m), f.cp(d)),
            ],
            t,
        )),
        se(print(
            vec![
                bin(B::Div, int(7, t.i64), f.cp(d)),
                bin(B::Rem, neg(int(7, t.i64)), int(2, t.i64)),
            ],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(
        out.stdout,
        "-9223372036854775808 -9223372036854775808 0\n-7 -1\n"
    );
}

#[test]
fn division_by_nonzero_constant_has_no_check() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("main", t.unit);
    let a = f.local("a", t.i64);
    let body = vec![
        let_(a, int(9, t.i64)),
        se(print(vec![bin(B::Div, f.cp(a), int(2, t.i64))], t)),
    ];
    pb.add_main(f.build(body));
    let v = lower_ok(&pb.finish());
    let main = v.funcs.iter().find(|f| f.symbol == "_V4main").unwrap();
    assert!(!contains_panic_call(main, &v), "{v}");
    assert_eq!(main.blocks.len(), 3, "{v}");
}

#[test]
fn pow_and_casts() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("main", t.unit);
    let x = f.local("x", t.f64);
    let body = vec![
        let_(x, flt(2.0, t.f64)),
        se(print(
            vec![
                bin(B::Pow, int(2, t.i64), int(10, t.i64)),
                bin(B::Pow, f.cp(x), flt(0.5, t.f64)),
                bin(B::Pow, int(3, t.u8), int(5, t.u8)),
            ],
            t,
        )),
        se(print(
            vec![
                cast(flt(1e10, t.f64), t.i32),
                cast(neg(flt(3.9, t.f64)), t.i64),
                cast(neg(int(1, t.i64)), t.u8),
                cast(bin(B::Div, f.cp(x), flt(0.0, t.f64)), t.i64),
                cast(f.cp(x), t.u8),
            ],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(
        out.stdout,
        "1024 1.4142135623730951 243\n2147483647 -3 255 9223372036854775807 2\n"
    );
}
