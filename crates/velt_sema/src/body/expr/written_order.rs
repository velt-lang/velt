//! Object literals evaluate their properties in written order, as JS does, although a literal
//! of a struct holds its fields in declaration order (`AdtLit`) and a spread's sources are bound
//! before the literal is made (`spread.rs`). A property is held in a temporary first only when
//! an effect could otherwise be seen out of order: values without effects (`no_effects`) and
//! literals written in declaration order cost nothing.
//!
//! - In a literal of a struct type, `{ y: f(), x: g() }` for `{ x; y }`: when a value with effects
//!   and another one that is not a constant are out of declaration order, each value that is not
//!   a constant is bound in written order (`bind_in_written_order`).
//! - In a literal with spreads, `{ a: f(), ...g() }`, `{ a: f(), ...o }` with `o.a` (overriding
//!   `a`), `{ ...o, y: f(), x: g() }`: a property with effects is evaluated where it is written
//!   (`hold_written`) when something with effects written after it is evaluated earlier, a
//!   spread overrides it, or other properties with effects end up out of order.

use velt_syntax::ast;

use super::spread::Value;
use crate::body::places::set_place_mode;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, BinOp, DefId, ExprKind as H, StmtKind as S, TyId};

/// An explicit property with effects of a spread literal: how many statements were bound
/// before it (`lets.len()` where it is written), its key and its value.
pub(super) type Written<'e> = (usize, &'e ast::Ident, &'e ast::Expr);

/// Where a property's value is evaluated in the literal as built.
enum Place {
    /// Read at entry `usize` (an anonymous object's field order), as an explicit value.
    Entry(usize),
    /// Under a spread's optional field (`Value::Over`): evaluated after the spreads.
    Under(usize),
    /// Overridden by a later spread, so never evaluated.
    Dropped,
}

impl<'e> FnCx<'_, '_> {
    /// The properties in `written` that would be evaluated out of written order, each checked
    /// and bound to a temporary at its position in `lets` (see the module docs).
    pub(super) fn hold_written(
        &mut self,
        written: Vec<Written<'e>>,
        entries: &mut [(String, Value<'e>)],
        target: Option<&(DefId, Vec<Option<TyId>>)>,
        lets: &mut Vec<hir::Stmt>,
    ) {
        if written.is_empty() {
            return;
        }
        let places: Vec<Place> = written.iter().map(|w| locate(entries, w.1)).collect();
        let fields = target.and_then(|(d, _)| self.cx.adt(*d).map(|a| a.fields.clone()));
        // Where each one runs among the others when not held: its field's position in a
        // struct literal, its entry's in an anonymous one.
        let rank = |w: &Written, e: usize| match &fields {
            Some(fs) => fs
                .iter()
                .position(|f| f.name == w.1.name)
                .unwrap_or(usize::MAX),
            None => e,
        };
        let mut hold: Vec<bool> = written
            .iter()
            .zip(&places)
            .map(|(w, p)| !matches!(p, Place::Entry(_)) || w.0 < lets.len())
            .collect();
        let ranks: Vec<usize> = (0..written.len())
            .filter(|&k| !hold[k])
            .filter_map(|k| match places[k] {
                Place::Entry(e) => Some(rank(&written[k], e)),
                _ => None,
            })
            .collect();
        if ranks.windows(2).any(|w| w[0] > w[1]) {
            hold.iter_mut().for_each(|h| *h = true);
        }
        let mut added = 0;
        for ((w, place), hold) in written.into_iter().zip(places).zip(hold) {
            if !hold {
                continue;
            }
            let (at, key, value) = w;
            let exp = target.and_then(|(_, slots)| {
                let f = fields.as_ref()?.iter().find(|f| f.name == key.name)?;
                Some(self.cx.subst_known(f.ty, slots))
            });
            let h = self.expr(value, exp, Want::Move);
            let span = h.span;
            let entry = match place {
                Place::Entry(e) | Place::Under(e) => e,
                Place::Dropped => {
                    // Evaluated for its effects; the spread's value is the field's.
                    lets.insert(
                        at + added,
                        hir::Stmt {
                            kind: S::Expr(h),
                            span,
                        },
                    );
                    added += 1;
                    continue;
                }
            };
            let ty = h.ty;
            let l = self.new_local("<prop>", ty, false, span, LocalKind::Temp);
            let stmt = hir::Stmt {
                kind: S::Let {
                    local: l,
                    init: Some(h),
                },
                span,
            };
            lets.insert(at + added, stmt);
            added += 1;
            let mode = self.use_mode(ty, Want::Move);
            let read = self.mk(H::Local(l, mode), ty, span);
            if let Some(v) = find_prop(&mut entries[entry].1, key) {
                *v = Value::Held(key.clone(), value, read);
            }
        }
    }

    /// `values` of a struct literal of `props` by field index: when they would be evaluated out
    /// of written order (see the module docs), each value that is not a constant is bound in
    /// written order, by the statements returned.
    pub(super) fn bind_in_written_order(
        &mut self,
        values: &mut [Option<hir::Expr>],
        fields: &[crate::defs::FieldInfo],
        props: &[ast::ObjectProp],
    ) -> Vec<hir::Stmt> {
        let mut stmts = vec![];
        let written: Vec<usize> = (self.props(props).iter())
            .filter_map(|(k, _)| fields.iter().position(|f| f.name == k.name))
            .collect();
        let moving: Vec<usize> = (0..values.len())
            .filter(|&i| values[i].as_ref().is_some_and(|h| !constant(h)))
            .collect();
        let effects = moving.iter().any(|&i| {
            let h = values[i].as_ref().expect("ICE: a value");
            !self.hir_no_effects(h)
        });
        // The written positions of the values that are not constants, in declaration order.
        let order: Vec<usize> = moving
            .iter()
            .filter_map(|i| written.iter().position(|w| w == i))
            .collect();
        if !effects || order.windows(2).all(|w| w[0] < w[1]) {
            return stmts;
        }
        for &i in &written {
            if !moving.contains(&i) {
                continue;
            }
            let Some(h) = values[i].take() else { continue };
            let mode = self.use_mode(h.ty, Want::Move);
            let mut t = self.temp("<field>", h, &mut stmts);
            set_place_mode(&mut t, mode);
            values[i] = Some(t);
        }
        stmts
    }

    /// Does evaluating the checked `h` have no effect (a read, a constant, or arithmetic and
    /// comparisons of numbers and booleans over them; see `no_effects`)?
    fn hir_no_effects(&mut self, h: &hir::Expr) -> bool {
        let primitive = |s: &Self, t: TyId| s.cx.ty.is_numeric(t) || t == s.cx.ty.bool_;
        match &h.kind {
            _ if constant(h) || super::spread_args::read_without_effects(h) => true,
            H::Cast(x) => self.hir_no_effects(x),
            H::Unary { expr, .. } => primitive(self, expr.ty) && self.hir_no_effects(expr),
            H::Binary { op, lhs, rhs } => {
                let int_div = matches!(op, BinOp::Div | BinOp::Rem) && self.cx.ty.is_int(lhs.ty);
                !int_div
                    && primitive(self, lhs.ty)
                    && primitive(self, rhs.ty)
                    && self.hir_no_effects(lhs)
                    && self.hir_no_effects(rhs)
            }
            _ => false,
        }
    }

    /// `no field `k` on type `sname``, unless the parser may have dropped its declaration.
    pub(super) fn no_such_field(&mut self, d: DefId, k: &ast::Ident, sname: &str) {
        if !crate::collect::exports::def_member_maybe_dropped(self.cx, d, &k.name) {
            let msg = format!("no field `{}` on type `{sname}`", k.name);
            self.cx.err(msg, k.span);
        }
    }
}

/// A literal or a closure: evaluating it has no effect and reads nothing.
fn constant(h: &hir::Expr) -> bool {
    matches!(h.kind, H::Lit(_) | H::Closure(_))
}

/// Where the property keyed `key` (told by its span) ended up in `entries`.
fn locate(entries: &[(String, Value)], key: &ast::Ident) -> Place {
    for (e, (_, v)) in entries.iter().enumerate() {
        if is_prop(v, key) {
            return Place::Entry(e);
        }
        let mut v = v;
        while let Value::Over(_, _, prev) = v {
            if is_prop(prev, key) {
                return Place::Under(e);
            }
            v = prev;
        }
    }
    Place::Dropped
}

fn is_prop(v: &Value, key: &ast::Ident) -> bool {
    matches!(v, Value::Prop(k, _) if k.span == key.span)
}

/// The explicit property keyed `key` in `v` or in what `v` is over.
fn find_prop<'a, 'e>(v: &'a mut Value<'e>, key: &ast::Ident) -> Option<&'a mut Value<'e>> {
    if is_prop(v, key) {
        return Some(v);
    }
    match v {
        Value::Over(_, _, prev) => find_prop(prev, key),
        _ => None,
    }
}
