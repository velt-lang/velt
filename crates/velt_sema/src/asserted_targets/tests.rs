use velt_common::FileId;
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
/// S-expressions; `None` when nothing is rewritten.
fn rewritten(body: &str) -> Option<Vec<String>> {
    let m = module(&format!("function f() {{ {body} }}"), false);
    let mut out = rewrite(&[m])?;
    let ast::ItemKind::Function(f) = out.remove(0).ast.items.remove(0).kind else {
        panic!("not a function");
    };
    let mut lines = vec![];
    for s in &f.body.stmts {
        stmt(s, &mut lines);
    }
    Some(lines)
}

fn stmt(s: &ast::Stmt, out: &mut Vec<String>) {
    match &s.kind {
        S::Expr(e) => out.push(sx(e)),
        S::Var(v) => out.extend(v.init.as_ref().map(sx)),
        S::Block(b) | S::Unsafe(b) => b.stmts.iter().for_each(|s| stmt(s, out)),
        S::For { update, body, .. } => {
            out.extend(update.as_ref().map(sx));
            body.stmts.iter().for_each(|s| stmt(s, out));
        }
        _ => {}
    }
}

fn sx(e: &ast::Expr) -> String {
    match &e.kind {
        E::Ident(i) => i.name.clone(),
        E::This => "this".into(),
        E::Lit(ast::Lit::Int { value, .. }) => value.to_string(),
        E::Lit(ast::Lit::Str(s)) => format!("{s:?}"),
        E::Lit(l) => format!("{l:?}"),
        E::Paren(x) => sx(x),
        E::NonNull(x) => format!("{}!", sx(x)),
        E::Member { object, prop, .. } => format!("{}.{}", sx(object), prop.name),
        E::Index {
            object,
            index,
            more,
            ..
        } => {
            let keys: Vec<String> = std::iter::once(&**index).chain(more).map(sx).collect();
            format!("{}[{}]", sx(object), keys.join(", "))
        }
        E::Call { callee, args, .. } => {
            let args: Vec<String> = args.iter().map(sx).collect();
            format!("{}({})", sx(callee), args.join(", "))
        }
        E::Binary { op, lhs, rhs } => format!("({op:?} {} {})", sx(lhs), sx(rhs)),
        E::Assign { op, target, value } => match op {
            None => format!("(= {} {})", sx(target), sx(value)),
            Some(op) => format!("({op:?}= {} {})", sx(target), sx(value)),
        },
        E::Update { op, prefix, target } => {
            let op = match op {
                ast::UpdateOp::Inc => "++",
                ast::UpdateOp::Dec => "--",
            };
            match prefix {
                true => format!("({op} {})", sx(target)),
                false => format!("({} {op})", sx(target)),
            }
        }
        E::Arrow {
            body: ast::ArrowBody::Expr(x),
            ..
        } => format!("(=> {})", sx(x)),
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

#[test]
fn logical_assignment_keeps_its_operator() {
    assert_eq!(
        rewritten("x! ??= y; a[i]! ||= 1; o.f! &&= g();").unwrap(),
        ["(Nullish= x y)", "(Or= a[i] 1)", "(And= o.f g())"]
    );
}

#[test]
fn compound_assignment_reads_the_target_asserted() {
    assert_eq!(
        rewritten("a[1]! += 10; counts[k]! -= n; this.m[i][j]! *= 2; r[\"a\"]! **= 2;").unwrap(),
        [
            "(= a[1] (Add a[1]! 10))",
            "(= counts[k] (Sub counts[k]! n))",
            "(= this.m[i][j] (Mul this.m[i][j]! 2))",
            "(= r[\"a\"] (Pow r[\"a\"]! 2))",
        ]
    );
}

#[test]
fn updates_in_statements_and_prefixed_read_the_target_asserted() {
    assert_eq!(
        rewritten("a[i]!++; --o.f!; for (;;i!++) {} const y = ++x!;").unwrap(),
        [
            "(= a[i] (Add a[i]! 1))",
            "(= o.f (Sub o.f! 1))",
            "(= i (Add i! 1))",
            "(= x (Add x! 1))",
        ]
    );
}

/// A target with effects is evaluated once: its assertion is only dropped. So is a postfix
/// update whose value is used (the value is the old one).
#[test]
fn targets_with_effects_and_postfix_values_drop_the_assertion() {
    assert_eq!(
        rewritten("a[next()]! += 1; a[i++]!++; f().x! -= 2; const y = x!++;").unwrap(),
        [
            "(Add= a[next()] 1)",
            "(a[(i ++)] ++)",
            "(Sub= f().x 2)",
            "(x ++)",
        ]
    );
}

#[test]
fn nested_functions_and_values_are_rewritten() {
    assert_eq!(
        rewritten("const g = () => a[0]! += 1; h(x! = 1);").unwrap(),
        ["(=> (= a[0] (Add a[0]! 1)))", "h((= x 1))"]
    );
    // In a function declared in a block, a class method and a parameter default.
    let src = "class C { m(p: number = (x! = 1)) { this.n! += 1; } }";
    let out = rewrite(&[module(src, false)]).unwrap();
    let ast::ItemKind::Class(c) = &out[0].ast.items[0].kind else {
        panic!("not a class");
    };
    let m = &c.methods[0].decl;
    assert_eq!(sx(m.sig.params[0].default.as_ref().unwrap()), "(= x 1)");
    let S::Expr(e) = &m.body.stmts[0].kind else {
        panic!("not an expression");
    };
    assert_eq!(sx(e), "(= this.n (Add this.n! 1))");
}

#[test]
fn modules_without_asserted_targets_are_not_copied() {
    assert_eq!(rewritten("x = 1; a[0]! + 1; y = a[0]!; f(x!);"), None);
    // Std is not rewritten.
    let m = module("function f() { x! = 1; }", true);
    assert!(rewrite(&[m]).is_none());
}

/// An operator indexer with several keys (`m[i, j]`), and a target in an `unsafe` block.
#[test]
fn several_keys_and_unsafe_blocks() {
    assert_eq!(
        rewritten("m[i, j]! += 1; m[i, f()]! += 1; unsafe { x! += 1; }").unwrap(),
        [
            "(= m[i, j] (Add m[i, j]! 1))",
            "(Add= m[i, f()] 1)",
            "(= x (Add x! 1))"
        ]
    );
}
