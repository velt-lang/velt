//! A symbol constant's record id (`Intrinsic::SymbolStatic`) is the same in every program
//! (#2943): it comes from the constant's name, or the key of `Symbol.for`, not from numbers one
//! check gives out.

mod common;

use common::programs::ok_src;
use velt_sema::hir::{Def, Program};

/// The `SymbolStatic` id the module constant `name` is initialized with.
fn symbol_id(p: &Program, name: &str) -> i128 {
    use velt_sema::hir::{ExprKind, Lit};
    let g = (p.defs.iter())
        .find_map(|d| match d {
            Def::Global(g) if g.name == name => Some(g),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no constant {name}"));
    match &g.init.kind {
        ExprKind::Call { args, .. } => match &args[0].kind {
            ExprKind::Lit(Lit::Int(id)) => *id as i128,
            other => panic!("not an id: {other:?}"),
        },
        other => panic!("not a symbol's record: {other:?}"),
    }
}

#[test]
fn a_symbol_constant_has_the_same_id_in_every_program() {
    // The second program declares more before the constant, so the constant's `DefId` differs;
    // the id it is compiled with doesn't.
    let use_it = "function main() { console.log(KEY.toString(), REG.toString()); }";
    let few = ok_src(&format!(
        "const KEY = Symbol(\"k\");\nconst REG = Symbol.for(\"r\");\n{use_it}"
    ));
    let more = ok_src(&format!(
        "class A {{}}\nclass B {{}}\nconst OTHER = Symbol.for(\"o\");\nconst KEY = Symbol(\"k\");\nconst REG = Symbol.for(\"r\");\n{use_it}\nfunction f() {{ console.log(OTHER.toString(), new A(), new B()); }}"
    ));
    assert_eq!(symbol_id(&few, "KEY"), symbol_id(&more, "KEY"));
    assert_eq!(symbol_id(&few, "REG"), symbol_id(&more, "REG"));
    assert_ne!(symbol_id(&more, "KEY"), symbol_id(&more, "REG"));
}
