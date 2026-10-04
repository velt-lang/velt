use velt_common::FileId;
use velt_syntax::ast;

use super::{assigned_by_closures, assigned_in_stmt, Assigned};

/// The names `f` finds in the body of `function main() { <body> }`, sorted.
fn names(body: &str, f: fn(&[ast::Stmt]) -> Vec<String>) -> Vec<String> {
    let src = format!("function main() {{ {body} }}");
    let (module, diags) = velt_syntax::parse_file(FileId(0), &src);
    assert!(diags.is_empty(), "{diags:?}");
    let ast::ItemKind::Function(main) = &module.items[0].kind else {
        panic!("not a function");
    };
    let mut out = f(&main.body.stmts);
    out.sort();
    out
}

fn by_closures(body: &str) -> Vec<String> {
    names(body, |stmts| {
        let out = assigned_by_closures(stmts, []);
        out.keys().map(|n| n.to_string()).collect()
    })
}

fn anywhere(body: &str) -> Vec<String> {
    names(body, |stmts| {
        let mut out = Assigned::new();
        stmts.iter().for_each(|s| assigned_in_stmt(s, &mut out));
        out.keys().map(|n| n.to_string()).collect()
    })
}

#[test]
fn closures_assigning_enclosing_variables() {
    assert_eq!(by_closures("let x = 1; x = 2;"), Vec::<String>::new());
    assert_eq!(by_closures("let x = 1; const f = () => { x = 2; };"), ["x"]);
    assert_eq!(by_closures("let x = 1; const f = () => x++;"), ["x"]);
    assert_eq!(
        by_closures("let x = 1; run(() => { if (c) { x += 1; } });"),
        ["x"]
    );
    // Nested closures, generator function expressions, parameter defaults, pattern defaults.
    assert_eq!(
        by_closures("let x = 1; const f = () => () => { x = 2; };"),
        ["x"]
    );
    assert_eq!(
        by_closures("let x = 1; const g = function* () { x = 2; };"),
        ["x"]
    );
    assert_eq!(
        by_closures("let x = 1; const f = (a = (x = 2)) => a;"),
        ["x"]
    );
    assert_eq!(
        by_closures("let x = 1; const f = () => { const [a = (x = 2)] = [1]; };"),
        ["x"]
    );
}

#[test]
fn a_closures_own_variables_are_not_enclosing_ones() {
    assert_eq!(
        by_closures("const f = (n) => { n = n + 1; };"),
        Vec::<String>::new()
    );
    assert_eq!(
        by_closures("const f = () => { let i = 0; i++; };"),
        Vec::<String>::new()
    );
    // Assigned before the closure declares its own `i`: that may be the enclosing `i`.
    assert_eq!(by_closures("const f = () => { i = 1; let i = 0; };"), ["i"]);
    // Declared in a nested block only: other assignments may name the enclosing variable.
    assert_eq!(
        by_closures("const f = () => { if (c) { let i = 0; } i = 1; };"),
        ["i"]
    );
    // A nested closure assigning the outer closure's local is not an enclosing assignment.
    assert_eq!(
        by_closures("const f = () => { let i = 0; const g = () => { i = 1; }; };"),
        Vec::<String>::new()
    );
}

#[test]
fn assignments_anywhere_include_closures() {
    assert_eq!(
        anywhere("x = 1; const f = () => { y = 2; let z = 0; z = 1; };"),
        ["x", "y"]
    );
}
