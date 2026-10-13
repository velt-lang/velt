//! `"b" in o` on an optional field without a presence flag (`b?: T`, `symbols::Presence`) tests
//! that the field is not null: an absent field and one holding `undefined` are one state. Node
//! tells them apart (`"b" in { b: undefined }` is `true`), so a program that both tests such a
//! field with `in` and stores a value that may be `undefined` in a field of that name is
//! rejected (#930), as `crate::object_copies` rejects a copy the program could tell from the
//! original.
//!
//! The check is by field name over the whole program, so that a value reaching the tested
//! object through another object type (a spread, a copy, a generic function) is found too. A
//! store is one of:
//!
//! - a property an object literal gives a value that may be null (`{ b }`, `{ b: o.b }`),
//!   remembered while checking (`Copies::null_literals`);
//! - an assignment of a value that may be null, a compound assignment, or any other writable
//!   use of the field;
//! - a field an object literal fills from something else that may be null (a spread of a
//!   `b: T | null` field).
//!
//! A value of a type without null, a field left out and the same optional field copied from
//! another object by a spread or a conversion (absent there, absent here) are not stores.

use std::collections::HashSet;

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{
    AdtKind, Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, Lit, TyId, TyKind, UseMode,
};
use crate::visit;

/// Report the `in` tests of optional fields that a stored `undefined` would answer wrongly.
pub(crate) fn check(cx: &mut Ctx) {
    let tests = std::mem::take(&mut cx.object_copies.in_optional);
    let literals = std::mem::take(&mut cx.object_copies.null_literals);
    if tests.is_empty() {
        return;
    }
    let names: HashSet<String> = tests.iter().map(|t| t.0.clone()).collect();
    let mut stores: Vec<(String, Span)> = literals
        .into_iter()
        .filter(|(n, _)| names.contains(n))
        .collect();
    for i in 0..cx.defs.len() {
        if let Some(Def::Fn(mut f)) = cx.defs[i].take() {
            let mut w = Walk::new(cx, &names);
            visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| w.expr(e));
            stores.append(&mut w.stores);
            cx.defs[i] = Some(Def::Fn(f));
        } else if let DefInfo::Global(g) = &cx.info[i] {
            if let Some(mut init) = g.init.clone() {
                let mut w = Walk::new(cx, &names);
                visit::expr(&mut init, &mut Exprs(&mut |e: &mut Expr| w.expr(e)));
                stores.append(&mut w.stores);
            }
        }
    }
    stores.sort_by_key(|(_, s)| (s.file, s.lo));
    let mut reported = HashSet::new();
    for (name, ty, at) in &tests {
        let Some((_, store)) = stores.iter().find(|(n, _)| n == name) else {
            continue;
        };
        // Once per field: the first test stands for the others.
        if !reported.insert(name.as_str()) {
            continue;
        }
        let (tn, shown) = (cx.display(*ty), cx.shown_name(name));
        cx.error(
            Diagnostic::error(
                format!("cannot tell an absent `{shown}` from one holding `undefined`: this tests the optional field `{shown}` of `{tn}` with `in`, and the program stores a value that may be `undefined` in a field `{shown}`"),
                *at,
            )
            .with_label(*store, "a value that may be `undefined` is stored here")
            .with_note(format!("Node finds a key whose value is `undefined`; Velt keeps an optional field `{shown}?: T` as its value or nothing, so the test would answer `false` (#930)"))
            .with_note(format!("store the value only when there is one (`if (v != null) o.{shown} = v`), or test the value instead of the key (`o.{shown} != null`)")),
        );
    }
}

struct Exprs<'a>(&'a mut dyn FnMut(&mut Expr));

impl visit::VisitMut for Exprs<'_> {
    fn expr(&mut self, e: &mut Expr) {
        (self.0)(e);
    }
}

/// The stores of a possibly-null value into a field named in `names`, in one body.
struct Walk<'a, 'm> {
    cx: &'a Ctx<'m>,
    names: &'a HashSet<String>,
    stores: Vec<(String, Span)>,
    /// Places already judged where they are used (an assignment's target, the object of
    /// `o.b.c = v`): expressions visited later, by address.
    judged: HashSet<*const Expr>,
}

impl<'a, 'm> Walk<'a, 'm> {
    fn new(cx: &'a Ctx<'m>, names: &'a HashSet<String>) -> Self {
        Walk {
            cx,
            names,
            stores: vec![],
            judged: HashSet::new(),
        }
    }

    /// The name of field `index` of a `t`, if it is a watched one: optional, without a presence
    /// flag, in an object type or struct, and tested with `in` somewhere.
    fn watched(&self, t: TyId, index: u32) -> Option<&'a str> {
        let TyKind::Adt(d, _) = self.cx.ty.kind(t) else {
            return None;
        };
        self.watched_field(*d, index)
    }

    fn watched_field(&self, d: DefId, index: u32) -> Option<&'a str> {
        let a = self.cx.adt(d)?;
        let f = a.fields.get(index as usize)?;
        if !f.optional
            || a.kind == AdtKind::Class
            || crate::anon::has_presence(&self.cx.ty, a.kind, f)
        {
            return None;
        }
        self.names.get(f.name.as_str()).map(String::as_str)
    }

    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::AdtLit { def, fields, .. } => {
                for (i, v) in fields.iter().enumerate() {
                    let Some(name) = self.watched_field(*def, i as u32) else {
                        continue;
                    };
                    // A field left out (a `null` the program wrote is `Copies::null_literals`).
                    if !matches!(v.kind, E::Lit(Lit::Null)) && self.may_be_null(v, name, true) {
                        self.stores.push((name.to_string(), v.span));
                    }
                }
            }
            E::Assign { place, value } => {
                if let Some(name) = self.place_field(place) {
                    self.judged.insert(&**place as *const Expr);
                    if self.may_be_null(value, name, false) {
                        self.stores.push((name.to_string(), e.span));
                    }
                }
            }
            E::CompoundAssign { place, .. } => {
                if let Some(name) = self.place_field(place) {
                    self.judged.insert(&**place as *const Expr);
                    self.stores.push((name.to_string(), e.span));
                }
            }
            // The value inside a present field is changed in place (`o.b.c = v`, `o.xs.push(v)`).
            E::UnwrapSome(inner, _) => {
                self.judged.insert(&**inner as *const Expr);
            }
            // Any other writable use of the field may store anything.
            E::Field {
                base,
                index,
                mode: UseMode::BorrowMut,
            } if !self.judged.contains(&(e as *const Expr)) => {
                if let Some(name) = self.watched(base.ty, *index) {
                    self.stores.push((name.to_string(), e.span));
                }
            }
            _ => {}
        }
    }

    fn place_field(&self, place: &Expr) -> Option<&'a str> {
        match &place.kind {
            E::Field { base, index, .. } => self.watched(base.ty, *index),
            _ => None,
        }
    }

    /// May `v`, stored in a watched field `name`, be null? Not when its type has no null, and
    /// in an object literal (`copies`) not when it is the same optional field of another object:
    /// a spread or a copy leaves the field absent where the source's is. A field read the
    /// program wrote itself (`{ b: o.b }`, which Node gives a key) is `Copies::null_literals`.
    fn may_be_null(&self, v: &Expr, name: &str, copies: bool) -> bool {
        match &v.kind {
            E::WrapSome(inner) => self.nullable(inner.ty),
            E::Field { base, index, .. } if copies => self.watched(base.ty, *index) != Some(name),
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::Share | Intrinsic::Clone),
                args,
            } if args.len() == 1 => self.may_be_null(&args[0], name, copies),
            E::Block(b) => b
                .value
                .as_ref()
                .is_none_or(|x| self.may_be_null(x, name, copies)),
            E::If { then, els, .. } => {
                self.may_be_null(then, name, copies) || self.may_be_null(els, name, copies)
            }
            E::Match { arms, .. } => arms.iter().any(|a| self.may_be_null(&a.body, name, copies)),
            _ => self.nullable(v.ty),
        }
    }

    /// May a value of type `t` be null (a type parameter may stand for a nullable type)?
    fn nullable(&self, t: TyId) -> bool {
        self.cx.ty.opt_payload(t).is_some()
            || matches!(self.cx.ty.kind(t), TyKind::Param(_) | TyKind::Error)
    }
}
