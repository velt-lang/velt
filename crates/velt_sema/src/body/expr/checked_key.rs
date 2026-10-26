//! Keys checked against a fixed set (`crate::global_this`'s rewrite of `(globalThis as Host)[k]`):
//! the rewrite calls `globalThis.key<"a" | "b", Host>(k)` (or `globalThis.defineKey` for
//! `Object.defineProperty`), a name no source can spell, which is `k` when its type is one of
//! the keys and TypeScript's TS7053 otherwise. The rest of a site with a wrong key is not
//! reported again (`crate::global_this::drop_site_errors`): the key's error is the site's one error.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::global_this::{DEFINE_KEY_CHECK, DELETE_KEY_CHECK, KEY_CHECK};
use crate::hir;

impl FnCx<'_, '_> {
    /// `globalThis.key<K, Host>(k)`: `k` as a `K` (`None`: `callee` is not such a check).
    pub(super) fn checked_key_call(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        // Only the rewrite of a program naming `globalThis` writes these calls.
        if self.cx.global_props.sites.is_empty() {
            return None;
        }
        let ast::ExprKind::Ident(id) = &callee.kind else {
            return None;
        };
        if !id.name.starts_with("globalThis.") {
            return None;
        }
        let site = match id.name.as_str() {
            KEY_CHECK => Site::Key,
            DEFINE_KEY_CHECK => Site::Define,
            DELETE_KEY_CHECK => Site::Delete,
            _ => return None,
        };
        let ([keys, host], [k]) = (type_args, args) else {
            return None;
        };
        let want = self.resolve(keys);
        // A written key (`delete (globalThis as T).K`) is its literal type.
        let exp = matches!(k.kind, ast::ExprKind::Lit(_)).then_some(want);
        let h = self.expr(k, exp, Want::Borrow);
        if self.cx.ty.is_bottom(h.ty) {
            return Some(h);
        }
        match self.try_coerce(h, want) {
            Ok(_) if site == Site::Delete => {
                self.cx.error(
                    Diagnostic::error("`delete` of a property of `globalThis` is not supported yet", span)
                        .with_note("Velt keeps each property of `globalThis` as a variable: assign it `undefined` instead"),
                );
                Some(self.error_expr(span))
            }
            Ok(h) => Some(h),
            Err(h) => {
                let found = self.cx.display(h.ty);
                let shown = match &host.kind {
                    ast::TypeExprKind::Named { path, .. } if path.len() == 1 => {
                        path[0].name.clone()
                    }
                    _ => {
                        let t = self.resolve(host);
                        self.cx.display(t)
                    }
                };
                let d = match site {
                    // TypeScript accepts any key there: no TypeScript error to name.
                    Site::Define => Diagnostic::error(
                        format!("the key of `Object.defineProperty(globalThis as {shown}, …)` must be one of its keys `{}` in Velt, found `{found}`", listed(keys)),
                        k.span,
                    )
                    .with_note("TypeScript accepts any key here; Velt keeps each property of `globalThis` as a variable, so it needs one of the listed keys"),
                    _ => Diagnostic::error(
                        format!("the key of `globalThis as {shown}` must be one of its keys `{}`, found `{found}` (TypeScript's TS7053)", listed(keys)),
                        k.span,
                    )
                    .with_note(format!("give the key the type of the keys (`{}`)", listed(keys))),
                };
                self.cx.error(d);
                self.cx.bad_key_sites.push((span, k.span));
                Some(self.error_expr(span))
            }
        }
    }
}

/// What a key check is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Site {
    Key,
    Define,
    Delete,
}

/// The keys of the union `t` of string literals, as written.
fn listed(t: &ast::TypeExpr) -> String {
    let one = |t: &ast::TypeExpr| match &t.kind {
        ast::TypeExprKind::Literal(ast::SignedLit {
            lit: ast::Lit::Str(s),
            ..
        }) => format!("\"{s}\""),
        _ => "?".into(),
    };
    match &t.kind {
        ast::TypeExprKind::Union(ms) => ms.iter().map(one).collect::<Vec<_>>().join(" | "),
        _ => one(t),
    }
}
