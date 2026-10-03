//! Generic arrow functions (`<T>(x: T): T => x`, or `<T,>` as `.tsx` spells it): a
//! `const f = <T>(x: T): R => body;` with typed parameters is checked as the generic function
//! `function f<T>(x: T): R { return body; }` (same spans; at module level with the same
//! `export`, in a body as a nested function, see [`local`]). Without a return type, the
//! function infers it from its body like any other (`collect::ret_infer`). The syntax is rewritten
//! before collection, so every later pass sees an ordinary generic function. Generic arrows
//! anywhere else are reported by `body::expr::closure` (a closure value has one type; Velt has
//! no generic function values).

mod local;

use std::collections::HashSet;

use velt_common::Span;
use velt_syntax::ast;

use crate::SourceModule;

/// The modules with their generic arrow constants rewritten into functions.
pub(crate) struct Lifted {
    pub modules: Vec<SourceModule>,
    /// Name spans of the functions made from local (non-module-level) generic arrows.
    pub local_fns: HashSet<Span>,
    /// Name spans of every function made from a generic arrow, module-level ones included.
    pub all_fns: HashSet<Span>,
}

/// The modules with their liftable generic arrow constants rewritten into functions, or `None`
/// when no module has one (the common case: nothing is copied).
pub(crate) fn lift(modules: &[SourceModule]) -> Option<Lifted> {
    let any = modules.iter().any(|m| {
        m.ast
            .items
            .iter()
            .any(|i| as_function(i).is_some() || local::item_has_local(i))
    });
    if !any {
        return None;
    }
    let mut locals = local::Rewriter::default();
    let mut module_fns = vec![];
    let mut lift_item = |i: &ast::Item| {
        let mut i = match as_function(i) {
            Some(f) => {
                if let ast::ItemKind::Function(f) = &f.kind {
                    module_fns.push(f.sig.name.span);
                }
                f
            }
            None => i.clone(),
        };
        locals.item(&mut i);
        i
    };
    let modules = modules
        .iter()
        .map(|m| SourceModule {
            path: m.path.clone(),
            is_std: m.is_std,
            file: m.file,
            ast: ast::Module {
                items: m.ast.items.iter().map(&mut lift_item).collect(),
                span: m.ast.span,
                jsx_import_source: m.ast.jsx_import_source.clone(),
            },
            imports: m.imports.clone(),
            jsx_runtime: m.jsx_runtime.clone(),
        })
        .collect();
    let local_fns: HashSet<Span> = locals.lifted.into_iter().collect();
    let all_fns = local_fns.iter().copied().chain(module_fns).collect();
    Some(Lifted {
        modules,
        local_fns,
        all_fns,
    })
}

/// A module-level `const f = <T>(x: T): R => body;` as `function f<T>(x: T): R { return body; }`
/// (`: R` only when the arrow has it).
fn as_function(item: &ast::Item) -> Option<ast::Item> {
    let ast::ItemKind::Var(v) = &item.kind else {
        return None;
    };
    Some(ast::Item {
        kind: ast::ItemKind::Function(arrow_function(v)?),
        exported: item.exported,
        span: item.span,
    })
}

/// The function a generic arrow constant `v` declares, if it is one with typed parameters.
fn arrow_function(v: &ast::VarDecl) -> Option<ast::FnDecl> {
    let ast::PatternKind::Ident(name) = &v.pattern.kind else {
        return None;
    };
    if v.kind != ast::VarKind::Const || v.ty.is_some() {
        return None;
    }
    let ast::ExprKind::Arrow {
        type_params,
        params,
        ret,
        throws,
        body,
        is_async,
    } = &v.init.as_ref()?.kind
    else {
        return None;
    };
    if type_params.is_empty() {
        return None;
    }
    let params = params
        .iter()
        .map(|p| {
            let ty = p.ty.clone()?;
            Some(ast::Param {
                span: p.name.span.to(ty.span),
                name: p.name.clone(),
                ty,
                default: None,
                optional: false,
                rest: false,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let body = match body {
        ast::ArrowBody::Block(b) => b.clone(),
        ast::ArrowBody::Expr(e) => ast::Block {
            stmts: vec![ast::Stmt {
                kind: ast::StmtKind::Return(Some((**e).clone())),
                span: e.span,
            }],
            span: e.span,
        },
    };
    let sig = ast::FnSig {
        name: name.clone(),
        generics: type_params.clone(),
        params,
        ret: ret.clone(),
        throws: throws.clone(),
        is_async: *is_async,
        span: v.span,
    };
    Some(ast::FnDecl { sig, body })
}

#[cfg(test)]
mod tests {
    use velt_common::FileId;

    use super::*;

    fn lifted(src: &str) -> Option<ast::ItemKind> {
        let (ast, d) = velt_syntax::parse_file(FileId(0), src);
        assert!(d.is_empty(), "{d:?}");
        let m = SourceModule {
            path: "main".into(),
            is_std: false,
            file: FileId(0),
            ast,
            imports: vec![],
            jsx_runtime: None,
        };
        lift(&[m]).map(|mut l| l.modules.remove(0).ast.items.remove(0).kind)
    }

    #[test]
    fn lifts_typed_module_level_generic_arrows() {
        let Some(ast::ItemKind::Function(f)) = lifted("export const id = <T,>(x: T): T => x;")
        else {
            panic!("not lifted");
        };
        assert_eq!(f.sig.name.name, "id");
        assert_eq!(f.sig.generics.len(), 1);
        assert!(matches!(
            f.body.stmts[0].kind,
            ast::StmtKind::Return(Some(_))
        ));
    }

    #[test]
    fn lifts_generic_arrows_without_a_return_type() {
        let Some(ast::ItemKind::Function(f)) = lifted("const id = <T,>(x: T) => x;") else {
            panic!("not lifted");
        };
        assert!(f.sig.ret.is_none());
    }

    #[test]
    fn leaves_other_constants_alone() {
        assert!(lifted("const id = <T,>(x): T => x;").is_none());
        assert!(lifted("const f = (x: i64): i64 => x;").is_none());
        assert!(lifted("const n = 1;").is_none());
    }

    #[test]
    fn lifts_local_generic_arrows_in_blocks_and_arrow_bodies() {
        let src = "function f() { if (true) { const id = <T,>(x: T): T => x; } \
                   const g = () => { const one = <T,>(xs: T[]): T => xs[0]; }; let k = 1; }";
        let Some(ast::ItemKind::Function(f)) = lifted(src) else {
            panic!("not a function");
        };
        let ast::StmtKind::If { then, .. } = &f.body.stmts[0].kind else {
            panic!("not an if");
        };
        assert!(matches!(then.stmts[0].kind, ast::StmtKind::Item(_)));
        let ast::StmtKind::Var(g) = &f.body.stmts[1].kind else {
            panic!("not a var");
        };
        let Some(ast::ExprKind::Arrow {
            body: ast::ArrowBody::Block(b),
            ..
        }) = g.init.as_ref().map(|e| &e.kind)
        else {
            panic!("not an arrow");
        };
        assert!(matches!(b.stmts[0].kind, ast::StmtKind::Item(_)));
        assert!(matches!(f.body.stmts[2].kind, ast::StmtKind::Var(_)));
    }

    #[test]
    fn leaves_bodies_without_generic_arrows_alone() {
        assert!(lifted("function f() { let id = <T,>(x: T): T => x; }").is_none());
        assert!(lifted("function f() { const id = <T,>(x) => x; }").is_none());
        assert!(lifted("function f() { const g = (x: i64): i64 => x; }").is_none());
    }
}
