//! Keys checked against a fixed set (`crate::global_this`'s rewrite of `(globalThis as Host)[k]`):
//! the rewrite calls `globalThis.key<"a" | "b", Host>(k)` (or `globalThis.defineKey` for
//! `Object.defineProperty`), a name no source can spell, which is `k` when its type is one of
//! the keys and TypeScript's TS7053 otherwise. The rest of a site with a wrong key is not
//! reported again (`crate::global_this::drop_site_errors`): the key's error is the site's one error.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::global_this::{DEFINE_KEY_CHECK, KEY_CHECK};
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
        let ast::ExprKind::Ident(id) = &callee.kind else {
            return None;
        };
        let define = match id.name.as_str() {
            KEY_CHECK => false,
            DEFINE_KEY_CHECK => true,
            _ => return None,
        };
        let ([keys, host], [k]) = (type_args, args) else {
            return None;
        };
        let want = self.resolve(keys);
        let h = self.expr(k, None, Want::Borrow);
        if self.cx.ty.is_bottom(h.ty) {
            return Some(h);
        }
        match self.try_coerce(h, want) {
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
                let d = Diagnostic::error(
                    format!("the key of `globalThis as {shown}` must be one of its keys `{}`, found `{found}` (TypeScript's TS7053)", listed(keys)),
                    k.span,
                );
                let d = match define {
                    true => d.with_note("TypeScript accepts any key in `Object.defineProperty`; Velt, which keeps each property of `globalThis` as a variable, needs one of the listed keys"),
                    false => d.with_note(format!("give the key the type of the keys (`{}`)", listed(keys))),
                };
                self.cx.error(d);
                self.cx.bad_key_sites.push((span, k.span));
                Some(self.error_expr(span))
            }
        }
    }
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
