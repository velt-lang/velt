//! The M1 golden programs (`tests/golden/m1/*.vlt`), hand-lowered to HIR the way sema will
//! produce them, run through lower → verify → interpreter and compared with the `.out` files.

use velt_sema::hir::{BinOp as B, Intrinsic, LogicOp, PassMode, Program, UnOp};

use super::builder::*;
use super::run;

fn golden(name: &str) -> String {
    let path = format!(
        "{}/../../tests/golden/m1/{name}.out",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .replace("\r\n", "\n")
}

pub(super) fn hello() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let f = FB::new("main", t.unit);
    pb.add_main(f.build(vec![se(print(vec![s("Hello, Velt!", t)], t))]));
    pb.finish()
}

pub(super) fn arith() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("main", t.unit);
    let a = f.local("a", t.i64);
    let b = f.local("b", t.i64);
    let x = f.local("x", t.f64);
    let y = f.local("y", t.f64);
    let small = f.local("small", t.i32);
    let u = f.local("u", t.u8);
    let i = |v| int(v, t.i64);
    let body = vec![
        let_(a, i(7)),
        let_(b, i(3)),
        se(print(
            vec![
                bin(B::Add, f.cp(a), f.cp(b)),
                bin(B::Sub, f.cp(a), f.cp(b)),
                bin(B::Mul, f.cp(a), f.cp(b)),
                // `/` on inferred integers is float division (sema casts both sides).
                bin(B::Div, cast(f.cp(a), t.f64), cast(f.cp(b), t.f64)),
                bin(B::Rem, f.cp(a), f.cp(b)),
            ],
            t,
        )),
        se(print(
            vec![
                bin(B::Div, cast(neg(f.cp(a)), t.f64), cast(f.cp(b), t.f64)),
                bin(B::Rem, neg(f.cp(a)), f.cp(b)),
            ],
            t,
        )),
        se(print(
            vec![bin(
                B::Sub,
                bin(B::Add, i(2), bin(B::Mul, i(3), i(4))),
                bin(B::Div, bin(B::Sub, i(10), i(4)), i(2)),
            )],
            t,
        )),
        let_(x, flt(1.5, t.f64)),
        let_(y, flt(2.25, t.f64)),
        se(print(
            vec![
                bin(B::Add, f.cp(x), f.cp(y)),
                bin(B::Mul, f.cp(x), f.cp(y)),
                bin(B::Div, f.cp(y), f.cp(x)),
            ],
            t,
        )),
        se(print(
            vec![
                flt(10.0, t.f64),
                bin(B::Add, flt(0.1, t.f64), flt(0.2, t.f64)),
                flt(1e21, t.f64),
                bin(B::Div, cast(i(5), t.f64), flt(2.0, t.f64)),
            ],
            t,
        )),
        se(print(
            vec![
                cmp(B::Gt, f.cp(a), f.cp(b), t),
                cmp(B::Eq, f.cp(a), f.cp(b), t),
                cmp(B::NotEq, f.cp(a), f.cp(b), t),
                logic(
                    LogicOp::Or,
                    logic(
                        LogicOp::And,
                        not(cmp(B::LtEq, f.cp(a), f.cp(b), t)),
                        boolean(true, t),
                        t,
                    ),
                    boolean(false, t),
                    t,
                ),
            ],
            t,
        )),
        let_(small, int(100000, t.i32)),
        se(print(
            vec![
                bin(B::Mul, f.cp(small), int(3, t.i32)),
                bin(B::Mul, cast(f.cp(small), t.i64), i(100000)),
            ],
            t,
        )),
        let_(u, int(250, t.u8)),
        se(print(
            vec![bin(B::Add, f.cp(u), int(5, t.u8)), cast(i(300), t.u8)],
            t,
        )),
        se(print(
            vec![
                bin(B::BitAnd, i(7), i(3)),
                bin(B::BitOr, i(7), i(8)),
                bin(B::BitXor, i(7), i(2)),
                bin(B::Shl, i(1), i(10)),
                bin(B::Shr, neg(i(16)), i(2)),
                un(UnOp::BitNot, i(0)),
            ],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

pub(super) fn control() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("main", t.unit);
    let i = f.local("i", t.i64);
    let sum = f.local("sum", t.i64);
    let j = f.local("j", t.i64);
    let n = f.local("n", t.i64);
    let kind = f.local("kind", t.str);
    let k = f.local("k", t.i64);
    let a = f.local("a", t.i64);
    let b = f.local("b", t.i64);
    let c = |v| int(v, t.i64);
    let body = vec![
        let_(i, c(0)),
        let_(sum, c(0)),
        while_(
            None,
            cmp(B::Lt, f.cp(i), c(10), t),
            vec![
                se(assign(f.cp(i), bin(B::Add, f.cp(i), c(1)), t)),
                if_(
                    cmp(B::Eq, bin(B::Rem, f.cp(i), c(2)), c(0), t),
                    vec![cont(None)],
                    None,
                ),
                se(cassign(B::Add, f.cp(sum), f.cp(i), t)),
            ],
            None,
        ),
        se(print(vec![s("sum odd", t), f.cp(sum)], t)),
        sblock(vec![
            let_(j, c(0)),
            while_(
                None,
                cmp(B::Lt, f.cp(j), c(5), t),
                vec![
                    if_(cmp(B::Eq, f.cp(j), c(3), t), vec![brk(None)], None),
                    se(print(vec![s("j", t), f.cp(j)], t)),
                ],
                Some(cassign(B::Add, f.cp(j), c(1), t)),
            ),
        ]),
        let_(n, c(15)),
        if_(
            cmp(B::Eq, bin(B::Rem, f.cp(n), c(15)), c(0), t),
            vec![se(print(vec![s("FizzBuzz", t)], t))],
            Some(vec![if_(
                cmp(B::Eq, bin(B::Rem, f.cp(n), c(3)), c(0), t),
                vec![se(print(vec![s("Fizz", t)], t))],
                Some(vec![se(print(vec![f.cp(n)], t))]),
            )]),
        ),
        let_(
            kind,
            ifx(cmp(B::Gt, f.cp(n), c(10), t), s("big", t), s("small", t)),
        ),
        se(print(vec![f.bw(kind)], t)),
        let_(k, c(3)),
        while_(
            None,
            boolean(true, t),
            vec![
                se(cassign(B::Sub, f.cp(k), c(1), t)),
                if_(not(cmp(B::Gt, f.cp(k), c(0), t)), vec![brk(None)], None),
            ],
            None,
        ),
        se(print(vec![f.cp(k)], t)),
        sblock(vec![
            let_(a, c(0)),
            while_(
                Some("outer"),
                cmp(B::Lt, f.cp(a), c(3), t),
                vec![sblock(vec![
                    let_(b, c(0)),
                    while_(
                        None,
                        cmp(B::Lt, f.cp(b), c(3), t),
                        vec![
                            if_(
                                cmp(B::Eq, f.cp(b), c(2), t),
                                vec![cont(Some("outer"))],
                                None,
                            ),
                            if_(cmp(B::Eq, f.cp(a), c(2), t), vec![brk(Some("outer"))], None),
                            se(print(vec![f.cp(a), f.cp(b)], t)),
                        ],
                        Some(cassign(B::Add, f.cp(b), c(1), t)),
                    ),
                ])],
                Some(cassign(B::Add, f.cp(a), c(1), t)),
            ),
        ]),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

pub(super) fn functions() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let c = |v| int(v, t.i64);
    let fib = pb.declare();
    let gcd = pb.declare();
    let is_even = pb.declare();
    let is_odd = pb.declare();
    let square = pb.declare();

    let mut f = FB::new("fib", t.i64);
    let n = f.param("n", t.i64, PassMode::Copy);
    let body = vec![
        if_(cmp(B::Lt, f.cp(n), c(2), t), vec![ret(Some(f.cp(n)))], None),
        ret(Some(bin(
            B::Add,
            call(fib, vec![bin(B::Sub, f.cp(n), c(1))], t.i64),
            call(fib, vec![bin(B::Sub, f.cp(n), c(2))], t.i64),
        ))),
    ];
    pb.define(fib, f.build(body));

    let mut f = FB::new("gcd", t.i64);
    let a = f.param("a", t.i64, PassMode::Copy);
    let b = f.param("b", t.i64, PassMode::Copy);
    let tt = f.local("t", t.i64);
    let body = vec![
        while_(
            None,
            cmp(B::NotEq, f.cp(b), c(0), t),
            vec![
                let_(tt, f.cp(b)),
                se(assign(f.cp(b), bin(B::Rem, f.cp(a), f.cp(b)), t)),
                se(assign(f.cp(a), f.cp(tt), t)),
            ],
            None,
        ),
        ret(Some(f.cp(a))),
    ];
    pb.define(gcd, f.build(body));

    for (id, name, other, base) in [
        (is_even, "isEven", is_odd, true),
        (is_odd, "isOdd", is_even, false),
    ] {
        let mut f = FB::new(name, t.bool);
        let n = f.param("n", t.i64, PassMode::Copy);
        let body = vec![ret(Some(ifx(
            cmp(B::Eq, f.cp(n), c(0), t),
            boolean(base, t),
            call(other, vec![bin(B::Sub, f.cp(n), c(1))], t.bool),
        )))];
        pb.define(id, f.build(body));
    }

    let mut f = FB::new("square", t.f64);
    let x = f.param("x", t.f64, PassMode::Copy);
    let body = vec![ret(Some(bin(B::Mul, f.cp(x), f.cp(x))))];
    pb.define(square, f.build(body));

    let f = FB::new("main", t.i32);
    let body = vec![
        se(print(vec![call(fib, vec![c(30)], t.i64)], t)),
        se(print(vec![call(gcd, vec![c(1071), c(462)], t.i64)], t)),
        se(print(
            vec![
                call(is_even, vec![c(10)], t.bool),
                call(is_odd, vec![c(7)], t.bool),
            ],
            t,
        )),
        se(print(vec![call(square, vec![flt(1.5, t.f64)], t.f64)], t)),
        ret(Some(int(3, t.i32))),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

pub(super) fn strings() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let greet = pb.declare();
    let mut f = FB::new("greet", t.str);
    let name = f.param("name", t.str, PassMode::Borrow);
    // `Hello, ${name}!`
    let body = vec![ret(Some(concat(
        concat(s("Hello, ", t), f.bw(name), t),
        s("!", t),
        t,
    )))];
    pb.define(greet, f.build(body));

    let mut f = FB::new("main", t.unit);
    let who = f.local("who", t.str);
    let n = f.local("n", t.i64);
    let pi = f.local("pi", t.f64);
    let sv = f.local("s", t.str);
    let i = f.local("i", t.i64);
    let line = f.local("line", t.str);
    let c = |v| int(v, t.i64);
    // `n=${n} pi=${pi} ok=${n > 40} sum=${n + 1}`
    let tmpl = {
        let mut e = concat(s("n=", t), to_s(f.cp(n), t), t);
        e = concat(e, s(" pi=", t), t);
        e = concat(e, to_s(f.cp(pi), t), t);
        e = concat(e, s(" ok=", t), t);
        e = concat(e, to_s(cmp(B::Gt, f.cp(n), c(40), t), t), t);
        e = concat(e, s(" sum=", t), t);
        concat(e, to_s(bin(B::Add, f.cp(n), c(1)), t), t)
    };
    let body = vec![
        let_(who, s("Velt", t)),
        se(print(vec![call(greet, vec![f.bw(who)], t.str)], t)),
        se(print(vec![call(greet, vec![s("world", t)], t.str)], t)),
        let_(n, c(42)),
        let_(pi, flt(3.5, t.f64)),
        se(print(vec![tmpl], t)),
        let_(sv, s("a", t)),
        se(assign(f.bm(sv), concat(f.bw(sv), s("b", t), t), t)),
        se(cassign(B::Add, f.bm(sv), s("c", t), t)),
        se(print(
            vec![f.bw(sv), intr(Intrinsic::StrLen, vec![f.bw(sv)], t.usize)],
            t,
        )),
        se(print(
            vec![
                cmp(B::Eq, s("abc", t), f.bw(sv), t),
                cmp(B::NotEq, s("abd", t), f.bw(sv), t),
                cmp(B::Lt, s("abc", t), s("abd", t), t),
            ],
            t,
        )),
        se(print(vec![s("multi\nline", t)], t)),
        se(print(vec![s("esc: \"q\" \\ \t|", t)], t)),
        sblock(vec![
            let_(i, c(0)),
            while_(
                None,
                cmp(B::Lt, f.cp(i), c(3), t),
                vec![
                    // `row ${i}: ${greet(who)}`
                    let_(
                        line,
                        concat(
                            concat(concat(s("row ", t), to_s(f.cp(i), t), t), s(": ", t), t),
                            call(greet, vec![f.bw(who)], t.str),
                            t,
                        ),
                    ),
                    se(print(vec![f.bw(line)], t)),
                ],
                Some(cassign(B::Add, f.cp(i), c(1), t)),
            ),
        ]),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

fn check(p: Program, name: &str, code: i32) {
    let out = run(&p);
    assert_eq!(out.stdout, golden(name), "stdout of {name}");
    assert_eq!(out.stderr, "", "stderr of {name}");
    assert_eq!(out.code, code, "exit code of {name}");
}

#[test]
fn golden_hello() {
    check(hello(), "hello", 0);
}

#[test]
fn golden_arith() {
    check(arith(), "arith", 0);
}

#[test]
fn golden_control() {
    check(control(), "control", 0);
}

#[test]
fn golden_functions() {
    check(functions(), "functions", 3);
}

#[test]
fn golden_strings() {
    check(strings(), "strings", 0);
}

/// Snapshot of the full VIR dump for `hello` (documents the Display format and the ABI shape).
#[test]
fn snapshot_hello() {
    let v = super::lower_ok(&hello());
    let expected = r#"agg#0 string size=24 align=8 { u64@0, u64@8, u64@16 }
static#0 align=1 "Hello, Velt!"
extern#0 velt_rt_write_str(u32, ptr) -> unit
extern#1 velt_rt_write_byte(u32, u8) -> unit

fn#0 internal _V4main() -> unit {
  let _0: u64
  let _1: agg#0
  let _2: ptr
  bb0:
    _0 = cast static#0 as u64
    _1 = agg#0 { _0, 51539607564_u64, 0_u64 }
    _2 = &_1
    call extern#0 velt_rt_write_str(1_u32, _2) -> bb1
  bb1:
    call extern#1 velt_rt_write_byte(1_u32, 10_u8) -> bb2
  bb2:
    return ()
}

fn#1 export velt_main() -> i32 {
  bb0:
    call fn#0 _V4main() -> bb1
  bb1:
    return 0_i32
}
"#;
    assert_eq!(v.to_string(), expected);
}

/// Snapshot of a Str-returning function: trailing out-pointer, borrowed param via deref, and a
/// three-part concatenation built in one string builder (no intermediate strings).
#[test]
fn snapshot_greet() {
    let v = super::lower_ok(&strings());
    let dump = v.to_string();
    let start = dump.find("fn#2 internal").unwrap();
    let end = start + dump[start..].find("\n}\n").unwrap() + 3;
    let expected = r#"fn#2 internal _V5greet(ptr, ptr) -> unit {
  param _0: ptr [nonnull dereferenceable(24)] // name
  param _1: ptr [noalias nonnull dereferenceable(24)] // ret.out
  let _2: agg#0
  let _3: ptr
  bb0:
    _3 = &_2
    call extern#3 velt_rt_strbuf_new(24_u64, _3) -> bb1
  bb1:
    call extern#4 velt_rt_strbuf_push_bytes(_3, static#15, 30064771079_u64) -> bb2
  bb2:
    call extern#13 velt_rt_strbuf_push_str(_3, _0) -> bb3
  bb3:
    call extern#14 velt_rt_strbuf_push_byte(_3, 33_u8) -> bb4
  bb4:
    (*_1 as agg#0) = _2
    return ()
}
"#;
    assert_eq!(&dump[start..end], expected);
}
