//! Parameter attributes (vir.rs invariant 9) set from pass modes; a modified (`BorrowMut`)
//! or borrowed class param gets the attributes of its object, like `this`.

use velt_sema::hir::{AdtKind, Def, PassMode, UseMode as U};

use super::builder::*;
use super::builder_m2::*;
use super::lower_ok;
use crate::vir::ParamAttrs;

/// `class Res { n: i64 }` with a constructor and a method; a free function taking `xs: i64[]`
/// (modified), `ys: i64[]` (read) and `r: Res` (modified) and returning a string. It prints
/// arrays of numbers through std's hooks (#2197): Velt source loaded with the real std.
const PROGRAM: &str = "class Res {\n  n: i64;\n  constructor(n: i64) {\n    this.n = n;\n  }\n  \
    bump() {\n    console.log(this.n);\n  }\n}\n\n\
    function describe(xs: i64[], ys: i64[], r: Res): string {\n  xs.push(7);\n  console.log(ys);\n  \
    r.n = 5;\n  return \"done\";\n}\n\nfunction peek(r: Res) {\n  console.log(r.n);\n}\n\n\
    function main() {\n  const a: i64[] = [];\n  const b: i64[] = [1];\n  const r = new Res(1);\n  \
    console.log(describe(a, b, r));\n  r.bump();\n  peek(r);\n  console.log(a);\n}\n";

#[test]
fn attributes_follow_pass_modes() {
    let v = super::lower_src(PROGRAM);
    let attrs = |sym: &str| {
        let f = v.funcs.iter().find(|f| f.symbol.starts_with(sym));
        f.unwrap_or_else(|| panic!("no `{sym}…` in\n{v}"))
            .param_attrs
            .clone()
    };
    let mutable = ParamAttrs {
        noalias: true,
        readonly: false,
        frozen: false,
        nonnull: true,
        dereferenceable: 24,
    };
    let shared = ParamAttrs {
        noalias: false,
        readonly: true,
        frozen: true,
        ..mutable
    };
    let out = ParamAttrs {
        dereferenceable: 24,
        ..mutable
    };
    // A class value is the object pointer: the attributes describe the object (8 bytes: `n`),
    // borrowed mutably / shared — for `this` and every other class-typed param (FINDINGS 8.1).
    let object = |noalias| ParamAttrs {
        noalias,
        readonly: !noalias,
        frozen: !noalias,
        nonnull: true,
        dereferenceable: 8,
    };
    // `xs`, `ys`, `r` (modified), the string out-pointer.
    assert_eq!(
        attrs("_V8describe"),
        vec![mutable, shared, object(true), out]
    );
    assert_eq!(attrs("_V3ResM11constructor")[0], object(true));
    assert_eq!(attrs("_V3ResM4bump"), vec![object(false)]);
    assert_eq!(attrs("_V4peek"), vec![object(false)]);
    assert!(v
        .to_string()
        .contains("[noalias nonnull dereferenceable(24)] // xs"));
}

#[test]
fn modified_class_param_is_seen_by_the_caller() {
    let out = super::run_src(PROGRAM);
    assert_eq!(out.stdout, "[ 1 ]\ndone\n5\n5\n[ 7 ]\n");
}

/// A borrowed param holding one of the prelude's `Mutex`, `RwLock` or `Atomic` in its own memory
/// is never `readonly`: the runtime writes a lock's word, and an `Atomic`'s methods write its
/// word, through a borrow. A plain struct of the same shape stays `readonly`. One holding an
/// `Atomic` is still `frozen` (only the atomic's word changes); one holding a `Mutex` is not
/// (the value it guards changes in place).
#[test]
fn borrowed_locks_and_atomics_are_not_readonly() {
    let mut pb = PB::new();
    let t = pb.t;
    let mut tys = vec![];
    for name in [
        "std/prelude/sync::Mutex",
        "std/prelude/sync::Atomic",
        "Plain",
    ] {
        let d = pb.declare();
        let ty = pb.adt_ty(d, vec![]);
        let mut ad = adt(
            name,
            AdtKind::Struct,
            vec![("w", t.i64, None), ("v", t.i64, None)],
        );
        ad.opaque = name.starts_with("std/");
        pb.set_def(d, Def::Adt(ad));
        tys.push((d, ty));
    }
    let mut peeks = vec![];
    for (i, &(_, ty)) in tys.iter().enumerate() {
        let mut f = FB::new(&format!("peek{i}"), t.unit);
        let x = f.param("x", ty, PassMode::Borrow);
        f.immutable(x);
        let body = vec![se(print(vec![field(f.bw(x), 1, U::Copy, t.i64)], t))];
        peeks.push(pb.add_fn(f.build(body)));
    }
    let mut f = FB::new("main", t.unit);
    let mut body = vec![];
    for (&(d, ty), &peek) in tys.iter().zip(&peeks) {
        let l = f.local("x", ty);
        body.push(let_(l, adt_lit(d, vec![int(0, t.i64), int(1, t.i64)], ty)));
        body.push(se(call(peek, vec![f.bw(l)], t.unit)));
    }
    pb.add_main(f.build(body));
    let v = lower_ok(&pb.finish());
    let attr = |sym: &str| {
        let f = v.funcs.iter().find(|f| f.symbol.contains(sym)).expect(sym);
        f.param_attrs.first().copied().unwrap_or_default()
    };
    let readonly = |sym: &str| attr(sym).readonly;
    assert!(
        !readonly("peek0"),
        "a Mutex param is written by the runtime"
    );
    assert!(
        !readonly("peek1"),
        "an Atomic param is written by its methods"
    );
    assert!(readonly("peek2"), "a plain struct param is only read");
    assert!(!attr("peek0").frozen, "a Mutex's value changes in place");
    assert!(attr("peek1").frozen, "only an Atomic's word changes");
    assert!(attr("peek2").frozen, "a plain struct param does not change");
}
