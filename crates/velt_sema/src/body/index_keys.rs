//! An object pattern taking apart an array or a tuple (`const { 0: first, 2: third, length } =
//! xs`): as in JavaScript, a numeric key reads that element and `length` the length (a tuple's
//! arity). Each key is read in order from the hidden temporary holding the value, an element
//! through the array pattern with holes up to it (`const [, , third] = #d`), so it gets the element's type (a
//! tuple's own, an array's with its default) exactly as an array pattern would:
//!
//! ```text
//! const { 2: c = d, length: n } = xs;   →   const #d = xs; const [, , c = d] = #d; const n = #d.length;
//! ```

use velt_syntax::ast::{self, Expr, ExprKind as E, Pattern, PatternKind as P, VarKind};

use super::pattern_defaults::{mk, pat};
use super::FnCx;
use crate::hir;

/// The largest index a numeric key reads through holes (`{ 4096: v }`): past it, one diagnostic.
const MAX_INDEX: usize = 4096;

/// The index a numeric key `{ 2: v }` (or `{ "2": v }`, `{ [2]: v }`) names: its name is the
/// canonical decimal form of an integer.
fn key_index(key: &ast::Ident) -> Option<usize> {
    let k: usize = key.name.parse().ok()?;
    (k.to_string() == key.name).then_some(k)
}

/// Does an object pattern in `p` name a property by a numeric key, or `length` (`p` is then
/// taken apart through hidden temporaries, `decl_with_defaults`, so an array or a tuple can be
/// read)?
pub(super) fn has_index_key(p: &Pattern) -> bool {
    match &p.kind {
        P::Object { fields, .. } => fields
            .iter()
            .any(|(k, sub)| key_index(k).is_some() || k.name == "length" || has_index_key(sub)),
        P::Array { elems, .. } => elems.iter().any(has_index_key),
        P::Default { pattern, .. } => has_index_key(pattern),
        P::Ident(_) | P::Wildcard => false,
    }
}

impl FnCx<'_, '_> {
    /// `kind { 0: a, length: n } = tmp` where the hidden temporary `tmp` holds an array or a
    /// tuple (module docs); `false` (nothing done) when it holds anything else.
    pub(super) fn index_key_decls(
        &mut self,
        kind: VarKind,
        fields: &[(ast::Ident, Pattern)],
        rest: Option<&ast::Ident>,
        tmp: &Expr,
        out: &mut Vec<hir::Stmt>,
    ) -> bool {
        let Some(t) = self.temp_ty(tmp) else {
            return false;
        };
        let arity = match self.cx.ty.kind(t) {
            hir::TyKind::Tuple(ts) => Some(ts.len()),
            hir::TyKind::Array(_) => None,
            _ => return false,
        };
        for (key, sub) in fields {
            if let Some(k) = key_index(key) {
                if k > MAX_INDEX {
                    let msg = format!("a numeric key above {MAX_INDEX} in an object pattern over an array is not supported yet; read the element with `xs[{k}]`");
                    self.unsupported_binding(kind, sub, &msg, key.span, out);
                    continue;
                }
                let mut elems = vec![pat(P::Wildcard, key.span); k];
                elems.push(sub.clone());
                let p = pat(P::Array { elems, rest: None }, sub.span);
                self.decl_with_defaults(kind, &p, tmp.clone(), out);
                continue;
            }
            // `length` is never `undefined`: a default never applies (and is not evaluated).
            let sub = match &sub.kind {
                P::Default { pattern, .. } if key.name == "length" => pattern,
                _ => sub,
            };
            let read = match arity {
                // A tuple's `length` is its arity.
                Some(n) if key.name == "length" => E::Lit(ast::Lit::Int {
                    value: n as u128,
                    suffix: None,
                }),
                _ => E::Member {
                    object: Box::new(tmp.clone()),
                    prop: key.clone(),
                    optional: false,
                },
            };
            self.decl_with_defaults(kind, sub, mk(read, key.span), out);
        }
        if let Some(r) = rest {
            let msg = "`...rest` in an object pattern over an array or a tuple is not supported yet; use an array pattern (`const [a, ...rest] = xs`)";
            self.unsupported_rest(kind, r, msg, out);
        }
        true
    }

    /// The bindings of `sub`, reported at `span` and declared as errors so their uses say
    /// nothing more.
    fn unsupported_binding(
        &mut self,
        kind: VarKind,
        sub: &Pattern,
        msg: &str,
        span: velt_common::Span,
        out: &mut Vec<hir::Stmt>,
    ) {
        self.cx.err(msg, span);
        let reported = mk(
            E::Ident(super::pattern_defaults::ident(crate::body::REPORTED, span)),
            span,
        );
        let sub = match &sub.kind {
            P::Default { pattern, .. } => pattern,
            _ => sub,
        };
        self.decl_with_defaults(kind, sub, reported, out);
    }
}
