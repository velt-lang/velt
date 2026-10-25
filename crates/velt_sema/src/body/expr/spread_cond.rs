//! A conditional spread of object literals, `{ a, ...(c ? { b: x } : {}) }`, the TypeScript
//! idiom for a key present only sometimes. TypeScript types the spread operand as
//! `{ b: number } | {}`, whose spread is `{ b?: number }`. Velt checks the conditional against
//! that object type (`cond_spread_hint`): a field that only some branches write is optional, so
//! each branch converts to it, and the spread copies the field only while present
//! (`spread.rs`), as JavaScript copies only the keys the chosen object has.

use velt_syntax::ast;

use crate::anon::ShapeField;
use crate::body::FnCx;
use crate::hir::TyId;

impl FnCx<'_, '_> {
    /// The object type a conditional spread operand `e` of object literals is checked as (see
    /// the module docs), or `None` when `e` is no such conditional, a branch does not check on
    /// its own, or the branches give one field different types.
    pub(super) fn cond_spread_hint(&mut self, e: &ast::Expr) -> Option<TyId> {
        if !matches!(strip(e).kind, ast::ExprKind::Cond { .. }) {
            return None;
        }
        let out = self.cond_shape(e)?;
        // Every branch writes every field: the literal types it already (nothing to add).
        if out.iter().all(|f| !f.optional) {
            return None;
        }
        Some(self.cx.anon_type_with(&out, self.module))
    }

    /// The fields of an object literal, or of a conditional of them (nested ones too), each
    /// optional when some branch does not write it.
    fn cond_shape(&mut self, e: &ast::Expr) -> Option<Vec<ShapeField>> {
        let e = strip(e);
        let (cond, then, els) = match &e.kind {
            ast::ExprKind::Object(props) if plain_props(props) => {
                let t = self.trial_type(e)?;
                let (d, args) = self.adt_of(t)?;
                let fields = self.cx.adt(d)?.fields.clone();
                let own = fields
                    .iter()
                    .map(|f| ShapeField {
                        name: f.name.clone(),
                        ty: self.cx.subst(f.declared, &args),
                        readonly: false,
                        optional: f.optional,
                    })
                    .collect();
                return Some(own);
            }
            ast::ExprKind::Cond { cond, then, els } => (cond, then, els),
            _ => return None,
        };
        // Each branch under the narrowing of the condition, as `ternary` checks it
        // (`c ? { r: { ...c } } : {}` with `c` an object or `null`).
        let (when_true, when_false) = self.narrowing(cond);
        let mut shapes: Vec<Vec<ShapeField>> = vec![];
        for (b, facts) in [then, els].into_iter().zip([when_true, when_false]) {
            self.push_scope();
            facts.iter().for_each(|f| self.narrow(f));
            let shape = self.cond_shape(b);
            self.pop_scope();
            shapes.push(shape?);
        }
        // Fields in written order: the first branch's, then the second's others.
        let mut out: Vec<ShapeField> = vec![];
        for (k, shape) in shapes.iter().enumerate() {
            for f in shape {
                let other = shapes[1 - k].iter().find(|g| g.name == f.name);
                if let Some(seen) = out.iter().find(|g| g.name == f.name) {
                    if seen.ty != f.ty {
                        return None;
                    }
                    continue;
                }
                out.push(ShapeField {
                    optional: f.optional || other.is_none_or(|g| g.optional),
                    ..f.clone()
                });
            }
        }
        Some(out)
    }
}

fn strip(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(x) => strip(x),
        _ => e,
    }
}

/// Only `key: value` and shorthand properties (no spread, method or computed key, which have
/// their own rules).
fn plain_props(props: &[ast::ObjectProp]) -> bool {
    props.iter().all(|p| match p {
        ast::ObjectProp::KeyValue(k, _) => !crate::symbols::is_computed_key(k),
        ast::ObjectProp::Shorthand(_) => true,
        _ => false,
    })
}
