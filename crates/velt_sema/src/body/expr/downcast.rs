//! `instanceof` downcasts: a value of a base class (or an interface value) tested for a
//! subclass (or an implementing class). The test is `PatKind::InstanceOf`, a check of the
//! dynamic class (lowering compares the class id in the object's vtable with the range of ids
//! of the class and its subclasses); a local that passed it reads as the subclass
//! (`ExprKind::Downcast`, flow narrowing in `body::narrow`).
//!
//! The narrowed type keeps the static type's type arguments: a `Base<i64>` tested for
//! `class Derived<T> extends Base<T>` is a `Derived<i64>`. A generic class whose type
//! arguments do not follow from the tested type cannot be narrowed to.

use velt_common::{Diagnostic, Span};

use crate::body::FnCx;
use crate::hir::{self, DefId, PatKind as P, TyId, TyKind};

/// How a (non-null) value of some type relates to a class.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Instance {
    /// Always an instance (the class itself or one of its subclasses).
    Yes,
    /// An instance when its dynamic class is: tested at run time; narrows to this type.
    Maybe(TyId),
    /// Never an instance.
    No,
}

impl FnCx<'_, '_> {
    /// How a value of type `t` relates to class `class`; `Err`: `class` is generic and its type
    /// arguments do not follow from `t`, so there is no type to narrow to.
    pub(crate) fn instance_kind(&mut self, t: TyId, class: DefId) -> Result<Instance, ()> {
        if self.cx.is_instance_of(t, class) {
            return Ok(Instance::Yes);
        }
        let n = self.cx.adt(class).map_or(0, |a| a.generics.len());
        let params: Vec<TyId> = (0..n as u32).map(|i| self.cx.ty.param(i)).collect();
        let own = self.cx.ty.intern(TyKind::Adt(class, params));
        let pattern = match self.cx.ty.kind(t).clone() {
            TyKind::Adt(d, _) if self.cx.class_of(t).is_some() => {
                if !self.cx.class_extends(class, d) {
                    return Ok(Instance::No);
                }
                self.ancestor_type(own, d)
            }
            TyKind::Dyn(i, _) => {
                if !self.class_tree_implements(class, i) {
                    return Ok(Instance::No);
                }
                if n == 0 {
                    return Ok(Instance::Maybe(own));
                }
                match self.cx.impl_args(own, i) {
                    Some(args) => Some(self.cx.ty.intern(TyKind::Dyn(i, args))),
                    None => None,
                }
            }
            _ => return Ok(Instance::No),
        };
        let Some(pattern) = pattern else {
            return Err(());
        };
        let mut slots = vec![None; n];
        if !self.cx.match_ty(pattern, t, &mut slots) {
            return Ok(Instance::No);
        }
        if slots.iter().any(Option::is_none) {
            return Err(());
        }
        Ok(Instance::Maybe(self.cx.subst_known(own, &slots)))
    }

    /// The type of the class `ancestor` in the base chain of class type `t`.
    fn ancestor_type(&mut self, t: TyId, ancestor: DefId) -> Option<TyId> {
        let mut cur = t;
        for _ in 0..64 {
            match self.cx.class_of(cur) {
                Some((d, _)) if d == ancestor => return Some(cur),
                Some(_) => cur = self.cx.base_of(cur)?,
                None => return None,
            }
        }
        None
    }

    /// Does class `class` or one of its subclasses implement interface `iface`?
    pub(super) fn class_tree_implements(&mut self, class: DefId, iface: DefId) -> bool {
        let classes: Vec<DefId> = (0..self.cx.info.len() as u32)
            .map(DefId)
            .filter(|&d| {
                self.cx
                    .adt(d)
                    .is_some_and(|a| a.kind == hir::AdtKind::Class)
                    && self.cx.class_extends(d, class)
            })
            .collect();
        classes.into_iter().any(|d| {
            let n = self.cx.adt(d).map_or(0, |a| a.generics.len());
            let params: Vec<TyId> = (0..n as u32).map(|i| self.cx.ty.param(i)).collect();
            let t = self.cx.ty.intern(TyKind::Adt(d, params));
            self.cx.find_impl(t, iface).is_some()
        })
    }

    /// `e instanceof C` on a value of type `sty` with class or interface parts: the test
    /// (`None` after reporting an error).
    pub(crate) fn class_test(
        &mut self,
        s: hir::Expr,
        class: DefId,
        span: Span,
    ) -> Option<hir::Expr> {
        let sty = s.ty;
        let mut alts = vec![];
        for (p, t) in self.member_patterns(sty, span) {
            let Some(t) = t else { continue };
            match self.instance_kind(t, class) {
                Ok(Instance::Yes) => alts.push(p),
                Ok(Instance::Maybe(_)) => alts.push(instance_leaf(p, class)),
                Ok(Instance::No) => {}
                Err(()) => {
                    self.undetermined_args(t, class, span);
                    return None;
                }
            }
        }
        if alts.is_empty() {
            self.never_instance(sty, class, span);
            return None;
        }
        let yes = match alts.len() {
            1 => alts.pop(),
            _ => Some(self.pat(P::Or(alts), sty, span)),
        };
        Some(self.bool_match(s, yes, span))
    }

    /// A generic class whose type arguments `t` does not determine.
    fn undetermined_args(&mut self, t: TyId, class: DefId, span: Span) {
        let (cn, tn) = (self.class_def_name(class), self.cx.display(t));
        // The base class or interface `t` names, how `class` relates to it, and whether it has
        // type parameters to pass on.
        let (base, relation, generic) = match (self.cx.class_of(t), self.cx.ty.kind(t)) {
            (Some((d, args)), _) => (self.class_def_name(d), "extends", !args.is_empty()),
            (None, TyKind::Dyn(_, args)) => {
                let name = tn.split('<').next().unwrap_or(&tn).to_string();
                (name, "implements", !args.is_empty())
            }
            _ => (tn.clone(), "extends", false),
        };
        let fix = if generic {
            format!(
                "test for a class that is not generic, or give `{cn}` exactly the type parameters it passes to `{base}` (`class {cn}<T> {relation} {base}<T>`)"
            )
        } else {
            format!(
                "`{base}` has no type parameters to determine those of `{cn}`; test for a class that is not generic, such as a non-generic subclass of `{cn}`"
            )
        };
        self.cx.error(
            Diagnostic::error(
                format!("`instanceof {cn}` cannot narrow a `{tn}`: the type arguments of `{cn}` do not follow from it"),
                span,
            )
            .with_note(fix),
        );
    }

    /// "this `instanceof` test is always false".
    fn never_instance(&mut self, sty: TyId, class: DefId, span: Span) {
        let cn = self.class_def_name(class);
        let inner = self.cx.ty.opt_payload(sty).unwrap_or(sty);
        let tn = self.cx.display(inner);
        let (msg, note) = if self.cx.union_def(inner).is_some() {
            (
                format!("no member of `{}` is a `{cn}`", self.cx.display(sty)),
                None,
            )
        } else if let TyKind::Dyn(..) = self.cx.ty.kind(inner) {
            (
                format!("no `{tn}` value is a `{cn}`"),
                Some(format!(
                    "neither `{cn}` nor any of its subclasses implements `{tn}`"
                )),
            )
        } else {
            (
                format!("no `{tn}` value is a `{cn}`"),
                Some(format!(
                    "`{tn}` values are instances of `{tn}` or of its subclasses, and `{cn}` is neither"
                )),
            )
        };
        let mut d = Diagnostic::error(
            format!("this `instanceof` test is always false: {msg}"),
            span,
        );
        if let Some(n) = note {
            d = d.with_note(n);
        }
        self.cx.error(d);
    }
}

/// `p` (a member pattern ending in a wildcard) with the wildcard replaced by the test for
/// `class`.
pub(crate) fn instance_leaf(p: hir::Pat, class: DefId) -> hir::Pat {
    let kind = match p.kind {
        P::Wildcard => P::InstanceOf(class),
        P::Some(inner) => P::Some(Box::new(instance_leaf(*inner, class))),
        P::Variant {
            def,
            variant,
            mut args,
        } => {
            if let Some(a) = args.pop() {
                args.push(instance_leaf(a, class));
            }
            P::Variant { def, variant, args }
        }
        k => k,
    };
    hir::Pat { kind, ..p }
}
