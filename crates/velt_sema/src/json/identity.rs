//! How expressions show an object's identity apart from `===` ([`crate::object_copies`]): a
//! value converted to a dynamic type, compared through one, a `Map` or `Set` key (objects
//! compare by identity there) and the key of a weak operation.

use velt_common::Span;

use super::Need;
use crate::copy_flow::{self, Store};
use crate::hir::{Callee, DefId, Expr, ExprKind as E, Intrinsic, Lit, TyId, TyKind};
use crate::object_copies::Seen;
use crate::types::Types;

/// What the looks of one program's bodies need.
pub(super) struct Looks<'a> {
    /// `Program::any_impl`: a `ToDyn` of it converts to a dynamic type.
    pub any_impl: Option<u32>,
    /// The prelude's `Proxy` class: a `Proxy` object made an `any` shows its target, which is
    /// an `any` already, and never its handler.
    pub proxy: Option<DefId>,
    pub tys: &'a Types,
    /// The copies made so far (span and converted-to type), for [`copy_flow::sources`].
    pub copies: &'a [(Span, TyId)],
    /// The function being visited.
    pub f: DefId,
    /// `Copies::dyn_sources.len()` before this pass: the index of `sources[0]` there.
    pub first: usize,
    /// Where the value of each dynamic conversion seen may come from (`Seen::Dynamic`).
    pub sources: Vec<Vec<Store>>,
}

impl Looks<'_> {
    /// Record the looks of `e` in `direct`.
    pub(super) fn uses(&mut self, direct: &mut Vec<(TyId, Span, Need)>, e: &Expr) {
        match &e.kind {
            E::ToDyn { expr, impl_index } if Some(*impl_index) == self.any_impl => {
                if matches!(self.tys.kind(expr.ty), TyKind::Adt(d, _) if Some(*d) == self.proxy)
                    || self.primitive(expr.ty)
                {
                    return;
                }
                let i = (self.first + self.sources.len()) as u32;
                self.sources
                    .push(copy_flow::sources(self.f, expr, self.copies));
                direct.push((expr.ty, e.span, Need::Seen(Seen::Dynamic(i))));
            }
            E::Call {
                callee: Callee::Intrinsic(i),
                args,
            } => {
                let (how, keys) = match i {
                    // A comparison with a primitive (`x !== undefined`, `k === "a"`) never
                    // compares two objects.
                    Intrinsic::AnyStrictEq | Intrinsic::AnyLooseEq => {
                        if args.iter().any(|a| self.primitive_operand(a)) {
                            return;
                        }
                        (Seen::Identity, &args[..])
                    }
                    // The map's id comes first; `set` has the value last, which is no key.
                    Intrinsic::WeakMapSet
                    | Intrinsic::WeakMapGet
                    | Intrinsic::WeakMapHas
                    | Intrinsic::WeakMapDelete => (Seen::Key, args.get(1..2).unwrap_or(&[])),
                    Intrinsic::WeakRefNew => (Seen::Key, &args[..]),
                    // Hashing a key of std's `Map` (and so `Set`).
                    Intrinsic::Hash => (Seen::Key, &args[..]),
                    _ => return,
                };
                for a in keys {
                    direct.push((a.ty, a.span, Need::Seen(how)));
                }
            }
            _ => {}
        }
    }

    /// Is `t` a type whose values are no objects (`boolean`, numbers, `string`, `void`, and
    /// those or-null)?
    fn primitive(&self, t: TyId) -> bool {
        match self.tys.kind(t) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str | TyKind::Unit => true,
            TyKind::Never => true,
            TyKind::Option(x, _) => self.primitive(*x),
            _ => false,
        }
    }

    /// Is the `any` operand `a` a literal or a primitive made dynamic?
    fn primitive_operand(&self, a: &Expr) -> bool {
        match &a.kind {
            E::Lit(
                Lit::Null | Lit::Unit | Lit::Int(_) | Lit::Float(_) | Lit::Str(_) | Lit::Bool(_),
            ) => true,
            E::ToDyn { expr, .. } => self.primitive_operand(expr) || self.primitive(expr.ty),
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::Share),
                args,
            } => args.first().is_some_and(|x| self.primitive_operand(x)),
            _ => false,
        }
    }
}

/// A call of a generated function (`declare generated`): its code looks at the shape of each of
/// its type arguments, as `JSON.stringify` (whose writer std generates) does when it writes a
/// value, so a copy of an object type's value shows there (`object_copies`).
pub(super) fn generated_look(
    generated: &rustc_hash::FxHashSet<crate::hir::DefId>,
    e: &crate::hir::Expr,
    direct: &mut Vec<(crate::hir::TyId, velt_common::Span, super::Need)>,
) {
    if let crate::hir::ExprKind::Call {
        callee: crate::hir::Callee::Def(d, targs),
        ..
    } = &e.kind
    {
        if generated.contains(d) {
            let seen = super::Need::Seen(crate::object_copies::Seen::Serialized);
            direct.extend(targs.iter().map(|t| (*t, e.span, seen)));
        }
    }
}
