//! Recursive arrow constants (#750): `const fact = (n: number): number => n <= 1 ? 1 : n *
//! fact(n - 1);` names itself in its own initializer. As a closure value its environment would
//! hold the closure itself (a reference cycle), so such a constant is checked as the function
//! `function fact(n: number): number { return …; }` instead: at module level a module function,
//! in a body a nested function, which `crate::nested_closures` makes a recursive closure group
//! when it uses the enclosing function's locals (members reach themselves through
//! `Intrinsic::ClosureSibling`, no cycle). The parameter types come from the arrow, or, when it
//! writes none, from the constant's function type annotation
//! (`const fib: (n: number) => number = (n) => …`). An arrow using `this` keeps the enclosing
//! `this`, which a function would not: it stays a closure.

use velt_syntax::ast;

/// The function the arrow constant `v` declares when the arrow is not generic, names the
/// constant in its body, and has (or gets from `v`'s annotation) typed parameters.
pub(super) fn recursive_arrow(v: &ast::VarDecl) -> Option<ast::FnDecl> {
    let ast::PatternKind::Ident(name) = &v.pattern.kind else {
        return None;
    };
    if v.kind != ast::VarKind::Const {
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
    if !type_params.is_empty() {
        return None;
    }
    // The annotation's parameter and result types, for an arrow that writes none.
    let written = match v.ty.as_ref().map(|t| &t.kind) {
        None => None,
        Some(ast::TypeExprKind::Function {
            params: tys,
            ret: r,
            throws: th,
            rest: false,
        }) if tys.len() == params.len() => Some((tys, r, th)),
        Some(_) => return None,
    };
    let params = params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let ty = match (&p.ty, written) {
                (Some(t), None) => t.clone(),
                (None, Some((tys, _, _))) if p.default.is_none() && !p.optional => tys[i].clone(),
                _ => return None,
            };
            if p.rest {
                return None;
            }
            Some(ast::Param {
                span: p.name.span.to(ty.span),
                name: p.name.clone(),
                pattern: p.pattern.clone(),
                ty,
                default: p.default.clone(),
                optional: p.optional,
                rest: false,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let (ret, throws) = match written {
        Some((_, r, th)) if ret.is_none() && throws.is_none() => {
            (Some((**r).clone()), th.as_deref().cloned())
        }
        Some(_) => return None,
        None => (ret.clone(), throws.clone()),
    };
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
    let f = ast::FnDecl {
        sig: ast::FnSig {
            name: name.clone(),
            generics: vec![],
            params,
            ret,
            throws,
            is_async: *is_async,
            is_generator: false,
            this_param: None,
            span: v.span,
        },
        body,
        overloads: vec![],
    };
    let scan = crate::nested_closures::scan::scan_fn(&f);
    (scan.refs.contains(&name.name) && !scan.this).then_some(f)
}

#[cfg(test)]
mod tests {
    use velt_common::FileId;

    use super::*;

    fn decl(src: &str) -> ast::VarDecl {
        let (m, d) = velt_syntax::parse_file(FileId(0), src);
        assert!(d.is_empty(), "{d:?}");
        match m.items.into_iter().next().map(|i| i.kind) {
            Some(ast::ItemKind::Var(v)) => *v,
            _ => panic!("not a constant"),
        }
    }

    #[test]
    fn lifts_arrows_naming_themselves() {
        let f = recursive_arrow(&decl(
            "const fact = (n: number): number => n <= 1 ? 1 : n * fact(n - 1);",
        ))
        .expect("lifted");
        assert_eq!(f.sig.name.name, "fact");
        assert!(f.sig.ret.is_some());
        // The parameter types from the annotation.
        let src = "const fib: (n: number) => number = (n) => n < 2 ? n : fib(n - 1) + fib(n - 2);";
        let f = recursive_arrow(&decl(src)).expect("lifted");
        assert!(f.sig.ret.is_some() && f.sig.params.len() == 1);
    }

    #[test]
    fn leaves_other_arrows_alone() {
        assert!(recursive_arrow(&decl("const f = (n: number): number => n + 1;")).is_none());
        assert!(recursive_arrow(&decl("const f = (n): number => f(n);")).is_none());
        assert!(recursive_arrow(&decl("let f = (n: number): number => f(n);")).is_none());
        assert!(
            recursive_arrow(&decl("const f = (n: number): number => this.k + f(n);")).is_none()
        );
        let src = "const f: (n: number, m: number) => number = (n) => f(n, n);";
        assert!(recursive_arrow(&decl(src)).is_none());
    }
}
