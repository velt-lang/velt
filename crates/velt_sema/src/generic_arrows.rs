//! Generic arrow functions (`<T,>(x: T): T => x`, the TSX spelling): a module-level
//! `const f = <T,>(x: T): R => body;` with typed parameters and a return type is checked as the
//! generic function `function f<T>(x: T): R { return body; }` (same spans, same `export`). The
//! syntax is rewritten before collection, so every later pass sees an ordinary generic function.
//! Generic arrows anywhere else are reported by `body::expr::closure` (a closure value has one
//! type; Velt has no generic function values).

use velt_syntax::ast;

use crate::SourceModule;

/// The modules with their liftable generic arrow constants rewritten into functions, or `None`
/// when no module has one (the common case: nothing is copied).
pub(crate) fn lift(modules: &[SourceModule]) -> Option<Vec<SourceModule>> {
    let any = modules
        .iter()
        .any(|m| m.ast.items.iter().any(|i| as_function(i).is_some()));
    if !any {
        return None;
    }
    let lifted = modules
        .iter()
        .map(|m| SourceModule {
            path: m.path.clone(),
            file: m.file,
            ast: ast::Module {
                items: m
                    .ast
                    .items
                    .iter()
                    .map(|i| as_function(i).unwrap_or_else(|| i.clone()))
                    .collect(),
                span: m.ast.span,
                jsx_import_source: m.ast.jsx_import_source.clone(),
            },
            imports: m.imports.clone(),
            jsx_runtime: m.jsx_runtime.clone(),
        })
        .collect();
    Some(lifted)
}

/// `const f = <T,>(x: T): R => body;` as `function f<T>(x: T): R { return body; }`.
fn as_function(item: &ast::Item) -> Option<ast::Item> {
    let ast::ItemKind::Var(v) = &item.kind else {
        return None;
    };
    let ast::PatternKind::Ident(name) = &v.pattern.kind else {
        return None;
    };
    if v.kind != ast::VarKind::Const || v.ty.is_some() {
        return None;
    }
    let ast::ExprKind::Arrow {
        type_params,
        params,
        ret: Some(ret),
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
        ret: Some(ret.clone()),
        throws: throws.clone(),
        is_async: *is_async,
        span: v.span,
    };
    Some(ast::Item {
        kind: ast::ItemKind::Function(ast::FnDecl { sig, body }),
        exported: item.exported,
        span: item.span,
    })
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
            file: FileId(0),
            ast,
            imports: vec![],
            jsx_runtime: None,
        };
        lift(&[m]).map(|mut ms| ms.remove(0).ast.items.remove(0).kind)
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
    fn leaves_other_constants_alone() {
        assert!(lifted("const id = <T,>(x: T) => x;").is_none());
        assert!(lifted("const id = <T,>(x): T => x;").is_none());
        assert!(lifted("const f = (x: i64): i64 => x;").is_none());
        assert!(lifted("const n = 1;").is_none());
    }
}
