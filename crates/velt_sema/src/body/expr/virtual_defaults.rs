//! Parameter defaults of a virtual call (`x.m(5)` on a `B` holding a `D` whose override has
//! its own default): in JavaScript a default belongs to the function that runs, so a left-out
//! argument (or one passed as `undefined`) takes the default of the override the call reaches.
//! When the overrides below the receiver's static class disagree on a default, the call tests
//! the receiver's dynamic class first and places the default of the override that runs:
//! `{ let <r> = recv; let <class> = match <r> { InstanceOf(E) => 2, InstanceOf(D) => 1, _ => 0 };
//! <r>.m(5, match <class> { 2 => <E's default>, 1 => <D's default>, _ => <B's default> }) }`.
//! The test runs before the arguments (the receiver is evaluated first, as before), each default
//! after them, as in JavaScript. A call whose overrides agree is left as it is.

use crate::body::places::is_path;
use crate::body::FnCx;

use crate::hir::{self, AdtKind, DefId, ExprKind as H, LocalId, PatKind, TyId, UseMode};
use velt_common::Span;

/// The class-dispatched defaults of one call: the classes to test (deepest first) and, per
/// left-out parameter, its position and the default each class gets (`None`: the base's).
pub(super) struct VirtualDefaults {
    classes: Vec<DefId>,
    params: Vec<(usize, Vec<Option<hir::Expr>>)>,
}

impl FnCx<'_, '_> {
    /// The defaults of the parameters a call leaves out or passes `undefined` (`args`: the
    /// arguments as written) of method `def` (vtable `slot` of the receiver's class, of type
    /// `recv_ty`) that overrides below that class replace with defaults of their own; `None`
    /// when every override takes the same ones (or the classes are generic, whose defaults keep
    /// the declaring method's).
    pub(super) fn virtual_defaults(
        &mut self,
        recv_ty: TyId,
        def: DefId,
        slot: u32,
        args: &[velt_syntax::ast::Expr],
    ) -> Option<VirtualDefaults> {
        let (s, targs) = self.adt_of(recv_ty)?;
        let n = self.cx.fn_info(def).params.len();
        let omitted: Vec<usize> = (0..n)
            .filter(|&k| args.get(k).is_none_or(crate::body::defaults::is_undefined))
            .collect();
        if !targs.is_empty()
            || omitted.is_empty()
            || !self.cx.fn_info(def).generics.names.is_empty()
        {
            return None;
        }
        crate::body::defaults::param_defaults(self.cx, def);
        // Defaults that can differ between overrides: not one the function applies itself
        // (the caller passes `undefined`), nor an optional parameter's `undefined`.
        let candidates: Vec<(usize, TyId, hir::Expr)> = (omitted.into_iter())
            .filter_map(|k| {
                let p = &self.cx.fn_info(def).params[k];
                let (ty, d) = (p.ty, p.default.clone()?);
                let same_for_all = self.cx.dynamic(ty).is_some()
                    || crate::body::defaults::undefined_only(self.cx, ty).is_some();
                (!same_for_all).then_some((k, ty, d))
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let below = self.classes_below(s, slot);
        if below.iter().all(|&(_, m)| m == def) {
            return None;
        }
        let mut params = vec![];
        for (k, ty, base_default) in candidates {
            let mut own = vec![];
            for &(_, m) in below.iter() {
                own.push(self.override_default(m, def, k, ty, &base_default));
            }
            if own.iter().any(Option::is_some) {
                params.push((k, own));
            }
        }
        if params.is_empty() {
            return None;
        }
        let classes = below.iter().map(|&(c, _)| c).collect();
        Some(VirtualDefaults { classes, params })
    }

    /// The default of parameter `k` of override `m` when it differs from the base method's
    /// (`base`, of type `ty`): `None` when it is the same constant or `m` cannot replace it
    /// (generic, another parameter type, no default).
    fn override_default(
        &mut self,
        m: DefId,
        def: DefId,
        k: usize,
        ty: TyId,
        base: &hir::Expr,
    ) -> Option<hir::Expr> {
        if m == def || !self.cx.fn_info(m).generics.names.is_empty() {
            return None;
        }
        crate::body::defaults::param_defaults(self.cx, m);
        let p = self.cx.fn_info(m).params.get(k)?;
        let d = p.default.as_ref().filter(|_| p.ty == ty)?;
        (!same_constant(d, base)).then(|| d.clone())
    }

    /// The classes below class `s`, deepest first, each with the method its vtable calls at
    /// `slot` (computed once per class and slot).
    fn classes_below(&mut self, s: DefId, slot: u32) -> crate::ctx::ClassesBelow {
        if let Some(b) = self.cx.classes_below.get(&(s, slot)) {
            return b.clone();
        }
        let mut below: Vec<(usize, DefId, DefId)> = vec![];
        for i in 0..self.cx.info.len() as u32 {
            let c = DefId(i);
            let Some(a) = self.cx.adt(c).filter(|a| a.kind == AdtKind::Class) else {
                continue;
            };
            let Some(&m) = a.vtable.get(slot as usize) else {
                continue;
            };
            if c != s && self.class_extends(c, s) {
                below.push((self.class_depth(c), c, m));
            }
        }
        below.sort_by(|a, b| b.0.cmp(&a.0).then(a.1 .0.cmp(&b.1 .0)));
        let below: crate::ctx::ClassesBelow = below.into_iter().map(|(_, c, m)| (c, m)).collect();
        self.cx.classes_below.insert((s, slot), below.clone());
        below
    }

    /// Does class `c` extend class `s`, directly or not?
    fn class_extends(&self, c: DefId, s: DefId) -> bool {
        let mut cur = self.cx.adt(c).and_then(|a| a.base);
        let mut hops = 0;
        while let Some((b, _)) = cur.and_then(|t| self.cx.class_of(t)) {
            if b == s {
                return true;
            }
            hops += 1;
            if hops > 256 {
                return false;
            }
            cur = self.cx.adt(b).and_then(|a| a.base);
        }
        false
    }

    /// The receiver for a call with class-dispatched defaults: a path is read again for the
    /// class test; anything else is bound to a temporary first (in `stmts`), which the call
    /// then uses. Returns the receiver for the call and the local holding the class number.
    pub(super) fn dispatch_receiver(
        &mut self,
        recv: hir::Expr,
        vd: &VirtualDefaults,
        stmts: &mut Vec<hir::Stmt>,
    ) -> (hir::Expr, LocalId) {
        let (ty, span) = (recv.ty, recv.span);
        let recv = if is_path(strip_casts(&recv)) {
            recv
        } else {
            let l = self.bind_temp("<receiver>", recv, stmts);
            self.mk(H::Local(l, UseMode::Borrow), ty, span)
        };
        let mut test = recv.clone();
        crate::body::places::set_place_mode(&mut test, UseMode::Borrow);
        let i32_ = self.cx.ty.i32;
        let mut arms = vec![];
        for (i, &c) in vd.classes.iter().enumerate() {
            arms.push(hir::Arm {
                pat: self.pat(PatKind::InstanceOf(c), ty, span),
                guard: None,
                body: self.class_number(i as u128 + 1, span),
            });
        }
        arms.push(hir::Arm {
            pat: self.pat(PatKind::Wildcard, ty, span),
            guard: None,
            body: self.class_number(0, span),
        });
        let kind = H::Match {
            scrutinee: Box::new(test),
            arms,
        };
        let pick = self.mk(kind, i32_, span);
        let class = self.bind_temp("<class>", pick, stmts);
        (recv, class)
    }

    /// Replace the left-out arguments in `args` (receiver first) with the default of the
    /// override the class number in `class` names.
    pub(super) fn dispatch_defaults(
        &mut self,
        vd: VirtualDefaults,
        class: LocalId,
        args: &mut [hir::Expr],
        type_args: &[TyId],
        span: Span,
    ) {
        let i32_ = self.cx.ty.i32;
        for (k, own) in vd.params {
            let Some(slot) = args.get_mut(k + 1) else {
                continue;
            };
            let base = std::mem::replace(slot, self.unit_expr(span));
            let ty = base.ty;
            let mut arms = vec![];
            for (i, d) in own.into_iter().enumerate() {
                let Some(d) = d else { continue };
                let body = self.placed_default(&d, type_args, span);
                arms.push(hir::Arm {
                    pat: self.pat(PatKind::Lit(hir::Lit::Int(i as u128 + 1)), i32_, span),
                    guard: None,
                    body,
                });
            }
            arms.push(hir::Arm {
                pat: self.pat(PatKind::Wildcard, i32_, span),
                guard: None,
                body: base,
            });
            let read = self.mk(H::Local(class, UseMode::Copy), i32_, span);
            let kind = H::Match {
                scrutinee: Box::new(read),
                arms,
            };
            args[k + 1] = self.mk(kind, ty, span);
        }
    }

    fn class_number(&self, v: u128, span: Span) -> hir::Expr {
        self.mk(H::Lit(hir::Lit::Int(v)), self.cx.ty.i32, span)
    }
}

fn strip_casts(e: &hir::Expr) -> &hir::Expr {
    match &e.kind {
        H::Upcast(x) | H::Downcast(x) => strip_casts(x),
        _ => e,
    }
}

/// Are defaults `a` and `b` the same constant (`= 1` in both)?
fn same_constant(a: &hir::Expr, b: &hir::Expr) -> bool {
    a.ty == b.ty
        && match (&a.kind, &b.kind) {
            (H::Lit(x), H::Lit(y)) => x == y,
            (H::Unary { op: x, expr: ex }, H::Unary { op: y, expr: ey }) => {
                x == y && same_constant(ex, ey)
            }
            _ => false,
        }
}
