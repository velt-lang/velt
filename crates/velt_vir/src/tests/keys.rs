//! Stable symbols (lower/keys.rs): a generic instance and its glue keep their symbols when an
//! unrelated edit shifts the type table's intern order, which `velt dev` relies on to match
//! functions across versions.

use velt_sema::hir::{PassMode, Program};

use super::builder::*;
use super::builder_m2::*;
use super::lower_ok;

/// `function id<T>(x: T): T { return x; }` called as `id<i64[]>([1])`; `shift` first interns
/// unrelated types, as an edit elsewhere in the program would.
fn program(shift: bool) -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    if shift {
        pb.arr(t.f64);
        pb.opt(t.str);
    }
    let p0 = pb.param(0);
    let id = {
        let mut f = FB::new("id", p0);
        f.generics = 1;
        let x = f.param("x", p0, PassMode::Owned);
        let body = vec![ret(Some(f.mv(x)))];
        pb.add_fn(f.build(body))
    };
    let ints = pb.arr(t.i64);
    let mut main = FB::new("main", t.unit);
    let xs = main.local("xs", ints);
    let arg = array(vec![int(1, t.i64)], ints);
    let body = vec![let_(xs, call_g(id, vec![ints], vec![arg], ints))];
    pb.add_main(main.build(body));
    pb.finish()
}

fn symbols(p: &Program) -> Vec<String> {
    let mut syms: Vec<String> = lower_ok(p).funcs.into_iter().map(|f| f.symbol).collect();
    syms.sort();
    syms
}

#[test]
fn instance_and_glue_symbols_ignore_intern_order() {
    let (plain, shifted) = (program(false), program(true));
    assert_ne!(plain.types.len(), shifted.types.len());
    let syms = symbols(&plain);
    assert_eq!(syms, symbols(&shifted));
    assert!(syms.contains(&"_V2id_T9i64_5b_5d".to_string()), "{syms:?}");
    assert!(syms.contains(&"_Gdrop_9i64_5b_5d".to_string()), "{syms:?}");
    assert_eq!(crate::mangle::demangle("_V2id_T9i64_5b_5d"), "id<i64[]>");
}
