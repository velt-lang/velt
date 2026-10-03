//! The JSX runtime import (docs/contracts/jsx.md "Choosing the provider"): a module containing
//! JSX implicitly imports `<source>/jsx-runtime`, where the source is the file's
//! `// @jsxImportSource` pragma, else its package's `jsx.importSource` (package.vlt), else
//! `std/jsx`. A module without JSX loads no runtime.

use velt_common::Span;
use velt_syntax::ast::{self, ExprKind as E, StmtKind as S};

/// The import source of modules that name none.
pub const DEFAULT_IMPORT_SOURCE: &str = "velt:jsx";

/// The module a JSX import source provides its factories from.
pub fn runtime_spec(source: &str) -> String {
    format!("{}/jsx-runtime", source.trim_end_matches('/'))
}

/// Span of the first JSX element or fragment in `module`, if it has any.
pub fn first_jsx(module: &ast::Module) -> Option<Span> {
    module.items.iter().find_map(item)
}

fn item(it: &ast::Item) -> Option<Span> {
    match &it.kind {
        ast::ItemKind::Function(f) => block(&f.body),
        ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => type_decl(t),
        ast::ItemKind::Interface(i) => i
            .fields
            .iter()
            .find_map(|f| f.default.as_ref().and_then(expr))
            .or_else(|| {
                i.methods
                    .iter()
                    .find_map(|m| m.body.as_ref().and_then(block))
            }),
        ast::ItemKind::Enum(e) => e
            .variants
            .iter()
            .find_map(|v| v.discriminant.as_ref().and_then(expr)),
        ast::ItemKind::Var(v) => v.init.as_ref().and_then(expr),
        ast::ItemKind::Extend(x) => x.methods.iter().find_map(|m| block(&m.decl.body)),
        ast::ItemKind::Import(_) | ast::ItemKind::TypeAlias(_) | ast::ItemKind::ExternFn(_) => None,
    }
}

fn type_decl(t: &ast::TypeDecl) -> Option<Span> {
    t.fields
        .iter()
        .find_map(|f| f.default.as_ref().and_then(expr))
        .or_else(|| t.constructor.as_ref().and_then(|c| block(&c.body)))
        .or_else(|| t.methods.iter().find_map(|m| block(&m.decl.body)))
}

fn block(b: &ast::Block) -> Option<Span> {
    b.stmts.iter().find_map(stmt)
}

fn stmt(s: &ast::Stmt) -> Option<Span> {
    match &s.kind {
        S::Var(v) => v.init.as_ref().and_then(expr),
        S::Expr(e) | S::Throw(e) => expr(e),
        S::Return(e) => e.as_ref().and_then(expr),
        S::If { cond, then, els } => expr(cond)
            .or_else(|| block(then))
            .or_else(|| els.as_deref().and_then(stmt)),
        S::While { cond, body } | S::DoWhile { body, cond } => expr(cond).or_else(|| block(body)),
        S::For {
            init,
            cond,
            update,
            body,
        } => init
            .as_deref()
            .and_then(stmt)
            .or_else(|| cond.as_ref().and_then(expr))
            .or_else(|| update.as_ref().and_then(expr))
            .or_else(|| block(body)),
        S::ForOf { iter, body, .. } => expr(iter).or_else(|| block(body)),
        S::Block(b) => block(b),
        S::Labeled { body, .. } => stmt(body),
        S::Switch {
            discriminant,
            cases,
        } => expr(discriminant).or_else(|| {
            cases.iter().find_map(|c| {
                c.test
                    .as_ref()
                    .and_then(expr)
                    .or_else(|| c.body.iter().find_map(stmt))
            })
        }),
        S::Try {
            body,
            catch,
            finally,
        } => block(body)
            .or_else(|| catch.as_ref().and_then(|(_, b)| block(b)))
            .or_else(|| finally.as_ref().and_then(block)),
        S::Item(it) => item(it),
        S::Break(_) | S::Continue(_) | S::Empty => None,
    }
}

fn expr(e: &ast::Expr) -> Option<Span> {
    match &e.kind {
        E::Jsx(_) => Some(e.span),
        E::Lit(_) | E::Ident(_) | E::This | E::Super => None,
        E::Template { exprs, .. } | E::Array(exprs) => exprs.iter().find_map(expr),
        E::Unary { expr: x, .. }
        | E::Update { target: x, .. }
        | E::Spread(x)
        | E::Await(x)
        | E::Cast { expr: x, .. }
        | E::InstanceOf { expr: x, .. }
        | E::Paren(x)
        | E::NonNull(x)
        | E::Member { object: x, .. } => expr(x),
        E::Binary { lhs, rhs, .. } => expr(lhs).or_else(|| expr(rhs)),
        E::Assign { target, value, .. } => expr(target).or_else(|| expr(value)),
        E::Cond { cond, then, els } => expr(cond).or_else(|| expr(then)).or_else(|| expr(els)),
        E::Call { callee, args, .. } => expr(callee).or_else(|| args.iter().find_map(expr)),
        E::New { args, .. } => args.iter().find_map(expr),
        E::Yield { arg, .. } => arg.as_deref().and_then(expr),
        E::Index { object, index, .. } => expr(object).or_else(|| expr(index)),
        E::Arrow { body, .. } => match body {
            ast::ArrowBody::Expr(x) => expr(x),
            ast::ArrowBody::Block(b) => block(b),
        },
        E::Function(f) => block(&f.body),
        E::Object(props) | E::StructLit { props, .. } => props.iter().find_map(|p| match p {
            ast::ObjectProp::KeyValue(_, v) | ast::ObjectProp::Spread(v) => expr(v),
            ast::ObjectProp::Shorthand(_) => None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use velt_common::FileId;

    use super::*;

    fn jsx_in(src: &str) -> bool {
        let (m, d) = velt_syntax::parse_file(FileId(0), src);
        assert!(d.is_empty(), "{d:?}");
        first_jsx(&m).is_some()
    }

    #[test]
    fn finds_jsx_anywhere() {
        assert!(jsx_in("function f() { return <p />; }"));
        assert!(jsx_in(
            "class C { m() { if (true) { const x = [() => <></>]; } } }"
        ));
        assert!(jsx_in("function f() { switch (1) { case 1: g(<a />); } }"));
        assert!(!jsx_in("function f(a: i64, b: i64) { return a < b; }"));
        assert!(!jsx_in("const id = <T,>(x: T): T => x;"));
    }

    #[test]
    fn runtime_module_of_a_source() {
        assert_eq!(runtime_spec("std/jsx"), "std/jsx/jsx-runtime");
        assert_eq!(runtime_spec("./ui/"), "./ui/jsx-runtime");
    }
}
