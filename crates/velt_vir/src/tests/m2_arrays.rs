//! Arrays and structs: the array parts of `tests/golden/m2/arrays_maps.velt` (Map is a prelude
//! class), `tests/golden/m2/structs.velt`, destructuring, bounds checks and array intrinsics. The
//! programs that index an array are Velt source loaded with the real std (std_src.rs): the
//! out-of-bounds report prints the index through std's hooks (#2197).

use velt_sema::hir::{AdtKind, BinOp as B, Def, Intrinsic as I, PassMode, Program, UseMode as U};

use super::builder::*;
use super::builder_m2::*;
use super::{run, run_src};

/// The array part of `tests/golden/m2/arrays_maps.velt` (pushes, a sum over `for … of`, indexing,
/// assignment through an index, `pop`, array and object destructuring, nested arrays). Indexing
/// builds the out-of-bounds report, which prints the index through std's hooks (#2197): loaded
/// with the real std (std_src.rs).
pub(super) const ARRAYS: &str = "function sum(xs: i64[]): i64 {\n  let s: i64 = 0;\n  \
    for (const x of xs) {\n    s += x;\n  }\n  return s;\n}\n\nfunction main() {\n  \
    const xs: i64[] = [];\n  for (let i: i64 = 0; i < 5; i++) {\n    xs.push(i * i);\n  }\n  \
    console.log(xs.length, sum(xs), xs[4]);\n  xs[0] = 100;\n  const last = xs.pop();\n  \
    console.log(xs[0], last ?? -1, xs.length);\n  const [first, second] = [10, 20];\n  \
    const { x, y } = { x: 1.5, y: 2.5 };\n  console.log(first + second, x + y);\n  \
    const nested: i64[][] = [[1, 2], [3]];\n  console.log(nested[0][1] + nested[1][0], nested.length);\n}\n";

#[test]
fn golden_arrays_part() {
    let out = run_src(ARRAYS);
    assert_eq!(out.stdout, "5 30 16\n100 16 4\n30 4\n5 2\n");
}

pub(super) fn structs() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let pd = pb.declare();
    let point = pb.adt_ty(pd, vec![]);
    let mut def = adt(
        "Point",
        AdtKind::Struct,
        vec![("x", t.f64, None), ("y", t.f64, None)],
    );
    def.is_copy = true;
    pb.set_def(pd, Def::Adt(def));
    let fx = |f: &FB, this, i| field(f.bw(this), i, U::Copy, t.f64);
    let len_m = {
        let mut f = FB::method("Point.len", point, t.f64);
        let this = f.param("this", point, PassMode::Borrow);
        let sq = bin(
            B::Add,
            bin(B::Mul, fx(&f, this, 0), fx(&f, this, 0)),
            bin(B::Mul, fx(&f, this, 1), fx(&f, this, 1)),
        );
        pb.add_fn(f.build(vec![ret(Some(intr(I::Sqrt, vec![sq], t.f64)))]))
    };
    let scale = {
        let mut f = FB::method("Point.scale", point, t.unit);
        let this = f.param("this", point, PassMode::BorrowMut);
        let k = f.param("k", t.f64, PassMode::Copy);
        let body = (0..2)
            .map(|i| {
                se(cassign(
                    B::Mul,
                    field(f.bm(this), i, U::BorrowMut, t.f64),
                    f.cp(k),
                    t,
                ))
            })
            .collect();
        pb.add_fn(f.build(body))
    };
    let add = {
        let mut f = FB::new("add", point);
        let a = f.param("a", point, PassMode::Copy);
        let b = f.param("b", point, PassMode::Copy);
        let lit = adt_lit(
            pd,
            vec![
                bin(B::Add, fx(&f, a, 0), fx(&f, b, 0)),
                bin(B::Add, fx(&f, a, 1), fx(&f, b, 1)),
            ],
            point,
        );
        pb.add_fn(f.build(vec![ret(Some(lit))]))
    };
    let od = pb.declare();
    let anon = pb.adt_ty(od, vec![]);
    pb.set_def(
        od,
        Def::Adt(adt(
            "{name,n}",
            AdtKind::Anon,
            vec![("name", t.str, None), ("n", t.i64, None)],
        )),
    );
    let mut f = FB::new("main", t.unit);
    let p = f.local("p", point);
    let q = f.local("q", point);
    let r = f.local("r", point);
    let o = f.local("o", anon);
    let body = vec![
        let_(
            p,
            adt_lit(pd, vec![flt(3.0, t.f64), flt(4.0, t.f64)], point),
        ),
        let_(q, f.cp(p)),
        se(print(
            vec![call(len_m, vec![f.bw(p)], t.f64), fx(&f, q, 0)],
            t,
        )),
        se(call(scale, vec![f.bm(p), flt(2.0, t.f64)], t.unit)),
        se(print(vec![fx(&f, p, 0), fx(&f, p, 1), fx(&f, q, 0)], t)),
        let_(r, call(add, vec![f.cp(p), f.cp(q)], point)),
        se(print(
            vec![concat(
                concat(to_s(fx(&f, r, 0), t), s(",", t), t),
                to_s(fx(&f, r, 1), t),
                t,
            )],
            t,
        )),
        let_(o, adt_lit(od, vec![s("anon", t), int(1, t.i64)], anon)),
        se(print(
            vec![
                field(f.bw(o), 0, U::Borrow, t.str),
                field(f.bw(o), 1, U::Copy, t.i64),
            ],
            t,
        )),
        se(print(
            vec![
                intr(I::Floor, vec![flt(2.7, t.f64)], t.f64),
                intr(I::Round, vec![flt(-2.5, t.f64)], t.f64),
                intr(I::FAbs, vec![neg(flt(4.5, t.f64))], t.f64),
                intr(I::Ceil, vec![flt(0.5, t.f64)], t.f64),
            ],
            t,
        )),
    ];
    pb.add_main(f.build(body));
    pb.finish()
}

#[test]
fn golden_structs_part() {
    let out = run(&structs());
    assert_eq!(out.stdout, "5 3\n6 8 3\n9,12\nanon 1\n2 -2 4.5 1\n");
}

#[test]
fn index_out_of_bounds_panics() {
    let src = "function main() {\n  const xs = [\"a\", \"b\"];\n  const i: i64 = 5;\n  \
               console.log(xs[i]);\n}\n";
    let out = run_src(src);
    assert_eq!(
        out.stderr,
        "panic: index out of bounds: the len is 2 but the index is 5\n"
    );
    assert_eq!(out.code, 101);
}

/// swap / shift / truncate / clone on arrays of strings (the clone is a copy of its own), and a
/// string key built at run time found in a `Map` (its hash is the literal's).
#[test]
fn array_intrinsics() {
    let src = "function main() {\n  const xs: string[] = [];\n  xs.push(\"a\" + \"1\");\n  \
               xs.push(\"b\" + \"2\");\n  xs.push(\"c\" + \"3\");\n  xs.swap(0, 2);\n  \
               const ys = xs.clone();\n  console.log(xs, ys);\n  const removed = xs.shift();\n  \
               ys.truncate(1);\n  console.log(removed, xs, ys);\n  \
               const m = new Map<string, i64>();\n  m.set(\"k\", 1);\n  const empty = \"\";\n  \
               console.log(m.has(empty + \"k\"));\n}\n";
    let out = run_src(src);
    assert_eq!(
        out.stdout,
        "[ 'c3', 'b2', 'a1' ] [ 'c3', 'b2', 'a1' ]\nc3 [ 'b2', 'a1' ] [ 'c3' ]\ntrue\n"
    );
}
