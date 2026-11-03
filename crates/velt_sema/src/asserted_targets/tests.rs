use rustc_hash::FxHashSet;
use velt_common::{FileId, Span};
use velt_syntax::ast::{self, ExprKind as E, StmtKind as S};

use super::rewrite;
use crate::SourceModule;

fn module(src: &str, is_std: bool) -> SourceModule {
    let (ast, d) = velt_syntax::parse_file(FileId(0), src);
    assert!(d.is_empty(), "{d:?}");
    SourceModule {
        path: "main".into(),
        is_std,
        is_dependency: false,
        file: FileId(0),
        ast,
        imports: vec![],
        jsx_runtime: None,
    }
}

/// The expression statements of `function f() { <body> }` once rewritten, written as
/// S-expressions, an assignment or update whose read is asserted marked `!`; `None` when nothing
/// is rewritten.
fn rewritten(body: &str) -> Option<Vec<String>> {
    let m = module(&format!("function f() {{ {body} }}"), false);
    let out = rewrite(&[m]);
    let ast::ItemKind::Function(f) = out.modules?.remove(0).ast.items.remove(0).kind else {
        panic!("not a function");
    };
    let mut lines = vec![];
    for s in &f.body.stmts {
        stmt(s, &out.rmw, &mut lines);
    }
    Some(lines)
}

fn stmt(s: &ast::Stmt, rmw: &FxHashSet<Span>, out: &mut Vec<String>) {
    match &s.kind {
        S::Expr(e) => out.push(sx(e, rmw)),
        S::Var(v) => out.extend(v.init.as_ref().map(|e| sx(e, rmw))),
        S::Block(b) | S::Unsafe(b) => b.stmts.iter().for_each(|s| stmt(s, rmw, out)),
        S::For { update, body, .. } => {
            out.extend(update.as_ref().map(|e| sx(e, rmw)));
            body.stmts.iter().for_each(|s| stmt(s, rmw, out));
        }
        _ => {}
    }
}

fn sx(e: &ast::Expr, rmw: &FxHashSet<Span>) -> String {
    let x = |e: &ast::Expr| sx(e, rmw);
    let mark = if rmw.contains(&e.span) { "!" } else { "" };
    match &e.kind {
        E::Ident(i) => i.name.clone(),
        E::This => "this".into(),
        E::Lit(ast::Lit::Int { value, .. }) => value.to_string(),
        E::Lit(ast::Lit::Str(s)) => format!("{s:?}"),
        E::Lit(l) => format!("{l:?}"),
        E::Paren(e) => x(e),
        E::NonNull(e) => format!("{}!", x(e)),
        E::Member { object, prop, .. } => format!("{}.{}", x(object), prop.name),
        E::Index {
            object,
            index,
            more,
            ..
        } => {
            let keys: Vec<String> = std::iter::once(&**index).chain(more).map(x).collect();
            format!("{}[{}]", x(object), keys.join(", "))
        }
        E::Call { callee, args, .. } => {
            let args: Vec<String> = args.iter().map(x).collect();
            format!("{}({})", x(callee), args.join(", "))
        }
        E::Binary { op, lhs, rhs } => format!("({op:?} {} {})", x(lhs), x(rhs)),
        E::Assign { op, target, value } => match op {
            None => format!("(= {} {})", x(target), x(value)),
            Some(op) => format!("({op:?}={mark} {} {})", x(target), x(value)),
        },
        E::Update { op, prefix, target } => {
            let op = match op {
                ast::UpdateOp::Inc => "++",
                ast::UpdateOp::Dec => "--",
            };
            match prefix {
                true => format!("({op}{mark} {})", x(target)),
                false => format!("({} {op}{mark})", x(target)),
            }
        }
        E::Arrow {
            body: ast::ArrowBody::Expr(e),
            ..
        } => format!("(=> {})", x(e)),
        other => format!("<{other:?}>"),
    }
}

#[test]
fn plain_assignment_drops_the_assertion() {
    assert_eq!(
        rewritten("x! = 1; a[2]! = 7; o.f! = v; (x)! = 2; (x!)! = 3;").unwrap(),
        ["(= x 1)", "(= a[2] 7)", "(= o.f v)", "(= x 2)", "(= x 3)"]
    );
}

/// The logical forms test the stored value as it is: their read is not asserted.
#[test]
fn logical_assignment_keeps_its_operator() {
    assert_eq!(
        rewritten("x! ??= y; a[i]! ||= 1; o.f! &&= g();").unwrap(),
        ["(Nullish= x y)", "(Or= a[i] 1)", "(And= o.f g())"]
    );
}

/// The target is kept whole (evaluated once by the checker), and the read marked asserted,
/// whatever the target's object and indices are.
#[test]
fn compound_assignments_are_marked() {
    assert_eq!(
        rewritten("a[1]! += 10; counts[k]! -= n; this.m[i][j]! *= 2; a[next()]! += 1;").unwrap(),
        [
            "(Add=! a[1] 10)",
            "(Sub=! counts[k] n)",
            "(Mul=! this.m[i][j] 2)",
            "(Add=! a[next()] 1)",
        ]
    );
}

#[test]
fn updates_are_marked() {
    assert_eq!(
        rewritten("a[i]!++; --o.f!; for (;;i!++) {} const y = ++x!; const z = x!--;").unwrap(),
        ["(a[i] ++!)", "(--! o.f)", "(i ++!)", "(++! x)", "(x --!)"]
    );
}

/// Only the assignments and updates whose target is asserted are marked.
#[test]
fn unasserted_targets_are_not_marked() {
    assert_eq!(
        rewritten("a[i++]!++; x += 1; a[0]!.n += 1;").unwrap(),
        ["(a[(i ++)] ++!)", "(Add= x 1)", "(Add= a[0]!.n 1)"]
    );
}

#[test]
fn nested_functions_and_values_are_rewritten() {
    assert_eq!(
        rewritten("const g = () => a[0]! += 1; h(x! = 1);").unwrap(),
        ["(=> (Add=! a[0] 1))", "h((= x 1))"]
    );
    // A class method and a parameter default.
    let src = "class C { m(p: number = (x! = 1)) { this.n! += 1; } }";
    let out = rewrite(&[module(src, false)]);
    let ast::ItemKind::Class(c) = &out.modules.as_ref().unwrap()[0].ast.items[0].kind else {
        panic!("not a class");
    };
    let m = &c.methods[0].decl;
    let default = m.sig.params[0].default.as_ref().unwrap();
    assert_eq!(sx(default, &out.rmw), "(= x 1)");
    let S::Expr(e) = &m.body.stmts[0].kind else {
        panic!("not an expression");
    };
    assert_eq!(sx(e, &out.rmw), "(Add=! this.n 1)");
}

#[test]
fn modules_without_asserted_targets_are_not_copied() {
    assert_eq!(rewritten("x = 1; a[0]! + 1; y = a[0]!; f(x!);"), None);
    // Std is not rewritten.
    let m = module("function f() { x! = 1; }", true);
    let out = rewrite(&[m]);
    assert!(out.modules.is_none() && out.rmw.is_empty());
}

/// An operator indexer with several keys (`m[i, j]`), and a target in an `unsafe` block.
#[test]
fn several_keys_and_unsafe_blocks() {
    assert_eq!(
        rewritten("m[i, j]! += 1; unsafe { x! += 1; }").unwrap(),
        ["(Add=! m[i, j] 1)", "(Add=! x 1)"]
    );
}
