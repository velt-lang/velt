//! The order of an element read (#1527). JavaScript reads the base of `base[index]` before it
//! evaluates the index, so an index that replaces the array being read (`rows[0][swap()]` where
//! `swap` stores a new array into `rows[0]`; a getter that assigns `this.xs`; `xs[(xs = ys, 0)]`)
//! leaves the element read from the array that was there. Lowering forms the element's place
//! after the index, which is the same thing whenever the index cannot store into the place the
//! base is read from: then the base stays a lazy place, and the code is what it always was. An
//! array changed in place by the index (a push, an element store) is the same array either way.
//! Otherwise the base is held against the index ([`hold_base`](FnLower::hold_base)): a share,
//! a reference of its own to the same array, which keeps it alive when the index replaces it
//! and sees what the index changes in it. The base's type is counted for that (in the whole
//! program, as any share of it does): a copy would not see the index's own changes to the
//! array, and nothing else keeps an uncounted array alive once its place is stored into.

use rustc_hash::FxHashSet;
use velt_sema::hir::{self, AdtKind, Intrinsic, PassMode, TyId, TyKind};

use super::async_fn::BoxHold;
use super::sequence::{any_operand, assigns_local, root_local};
use super::FnLower;
use crate::vir::Operand;

/// A link of a place: a field of a value of the type, an element, a module global.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Link {
    Field(Option<hir::DefId>, u32),
    Element,
    Global(hir::DefId),
}

/// The fields of object types some code assigns (`o.f = v`, `o.f += v`), by the type's
/// definition and the field's index: a field nothing assigns keeps its array while any call
/// runs. `any_key`: some code writes a field by a key known only at run time (generic
/// `x[k] = v`), so every field counts as assigned.
#[derive(Default)]
pub(super) struct AssignedFields {
    fields: FxHashSet<(hir::DefId, u32)>,
    any_key: bool,
}

impl FnLower<'_, '_> {
    /// The base of the element read `base[index]` (`hold`, `frozen`: as `index_expr` found
    /// them; a frozen value is changed by nothing, and `index_expr` holds its box already).
    pub(super) fn index_base(
        &mut self,
        base: &hir::Expr,
        index: &hir::Expr,
        hold: BoxHold,
        frozen: bool,
    ) -> Operand {
        if !frozen && hold == BoxHold::No && self.index_holds(base, index) {
            return self.hold_base(base);
        }
        self.projection_base(base, hold)
    }

    /// Must the base of `base[index]` be held against the index: can the index store another
    /// value where the base is read from?
    pub(super) fn index_holds(&mut self, base: &hir::Expr, index: &hir::Expr) -> bool {
        let later = self.later_of(index);
        // A call of the standard library on numbers (`xs[Math.floor(x)]`) reaches no array.
        if later.memory && self.reaches_objects(index) && self.base_replaceable(base, index) {
            return true;
        }
        // The index assigns the variable the base is read from (`xs[(xs = ys).length - 1]`).
        root_local(base).is_some_and(|l| later.may_write(l) && assigns_local(index, l))
            && self.assigns_link(index, base)
    }

    /// The base of the assignment target `base[index]`, held before the index runs when it
    /// must be ([`index_holds`](Self::index_holds)): the store then goes into the array the
    /// base held then, as in JavaScript (`rows[0][swap()] = v` writes into the old `rows[0]`).
    pub(super) fn target_base(&mut self, base: &hir::Expr, index: &hir::Expr) -> Option<Operand> {
        if self.dead() || self.frozen_root(base) || !self.index_holds(base, index) {
            return None;
        }
        Some(self.hold_base(base))
    }

    /// The root and indices of the target's base `base` (of `base[index]`) for
    /// [`place_indices`](Self::place_indices), or the base held before the index runs when it
    /// must be ([`target_base`](Self::target_base)), recorded for `index_target_base`.
    pub(super) fn target_indices(
        &mut self,
        base: &hir::Expr,
        index: &hir::Expr,
        out: &mut std::collections::VecDeque<Operand>,
    ) {
        match self.target_base(base, index) {
            Some(h) => (self.held_targets).push((base as *const hir::Expr as usize, h)),
            None => self.place_indices(base, out),
        }
    }

    /// The array of the target `base[index]`: the base [`place_indices`] held before the index
    /// ran, or held now before `index_target` evaluates the index (a target formed without
    /// `place_indices`, `xs[f()] += 1`), else its place.
    ///
    /// [`place_indices`]: Self::place_indices
    pub(super) fn index_target_base(
        &mut self,
        base: &hir::Expr,
        index: &hir::Expr,
        pre: &mut std::collections::VecDeque<Operand>,
    ) -> crate::vir::Place {
        let key = base as *const hir::Expr as usize;
        let held = (self.held_targets.iter())
            .rfind(|(k, _)| *k == key)
            .map(|(_, h)| h.clone());
        let held = match held {
            Some(h) => Some(h),
            // Indices `place_indices` evaluated already: it held what had to be held.
            None if !pre.is_empty() => None,
            None => self.target_base(base, index),
        };
        let bty = self.sub(base.ty);
        match held {
            Some(h) => self.place_of(h, bty),
            None => self.place_expr_with(base, pre),
        }
    }

    /// May what `index` runs store into a link of the place `base` ([`writes_of`])?
    ///
    /// [`writes_of`]: Self::writes_of
    fn writes_link(&mut self, index: &hir::Expr, base: &hir::Expr) -> bool {
        let w = self.writes_of(index);
        if w.all {
            return true;
        }
        self.links(base).iter().any(|l| match l {
            Link::Field(Some(d), i) => w.any_field || w.fields.contains(&(*d, *i)),
            Link::Field(None, _) => w.any_field,
            Link::Element => w.element,
            Link::Global(d) => w.globals.contains(d),
        })
    }

    /// Can code in `index` store another value into the place `base` is read from: a variable
    /// a closure assigns, a module `let` some code reassigns, or a field or element of a
    /// container that code can reach ([`container_reached`](Self::container_reached))?
    fn base_replaceable(&mut self, base: &hir::Expr, index: &hir::Expr) -> bool {
        use hir::ExprKind as K;
        match &base.kind {
            K::Local(l, _) => self.info[l.0 as usize].in_cell && self.writes_of(index).all,
            K::Global(d) => self.slot_reassigned(*d) && self.writes_link(index, base),
            K::Field { base: c, .. } | K::Index { base: c, .. } => {
                self.path_written(base)
                    && self.container_reached(c, base, index)
                    && self.writes_link(index, base)
            }
            K::UnwrapSome(x, _) | K::UnwrapVariant { expr: x, .. } | K::Downcast(x) => {
                self.base_replaceable(x, index)
            }
            _ => false,
        }
    }

    /// Can any code store into a link of the place `e` (`h.inner.xs`): a field some code
    /// assigns, an element, a module `let` some code reassigns, a variable a closure assigns?
    /// A field nothing assigns, of a variable or a constant, holds the same value throughout.
    fn path_written(&mut self, e: &hir::Expr) -> bool {
        use hir::ExprKind as K;
        match &e.kind {
            K::Local(l, _) => self.info[l.0 as usize].in_cell,
            K::Global(d) => self.slot_reassigned(*d),
            K::Field { base, index, .. } => {
                self.field_assigned(base, *index) || self.path_written(base)
            }
            K::Index { .. } => true,
            K::UnwrapSome(x, _) | K::UnwrapVariant { expr: x, .. } | K::Downcast(x) => {
                self.path_written(x)
            }
            _ => true,
        }
    }

    /// Does some code assign field `index` of the value `base` holds (or may it, through
    /// `any` or a key known only at run time)?
    fn field_assigned(&mut self, base: &hir::Expr, index: u32) -> bool {
        let bt = self.sub(base.ty);
        let Some(d) = self.field_key(bt) else {
            return true;
        };
        // An anonymous object type is one def per shape here, which may not be the def an
        // assignment's place names: count it as assigned.
        let anon = matches!(self.cx.hir.def(d), hir::Def::Adt(a) if a.kind == AdtKind::Anon);
        if anon || self.cx.boxing.any_writes {
            return true;
        }
        let hir = self.cx.hir;
        let af = self
            .cx
            .assigned_fields
            .get_or_insert_with(|| assigned_fields(hir));
        af.any_key || af.fields.contains(&(d, index))
    }

    /// Can code in `index` reach the container `c` to store into it? A counted container has
    /// other owners anywhere; a module slot, and a variable a closure assigns, any function
    /// reaches; a variable of this function only a callee it is passed to as changeable
    /// (`BorrowMut`: a method that assigns a field of `this`, a getter that does), or a callee
    /// whose parameters are not known here.
    fn container_reached(&mut self, c: &hir::Expr, base: &hir::Expr, index: &hir::Expr) -> bool {
        use hir::ExprKind as K;
        let ct = self.sub(c.ty);
        if self.cx.counted(ct) {
            return true;
        }
        match &c.kind {
            K::Local(l, _) => {
                self.info[l.0 as usize].in_cell || self.passes_changeable(index, base)
            }
            K::Global(d) => self.slot_of(*d).is_some(),
            K::Field { base: x, .. }
            | K::Index { base: x, .. }
            | K::UnwrapSome(x, _)
            | K::UnwrapVariant { expr: x, .. }
            | K::Downcast(x) => self.container_reached(x, base, index),
            _ => false,
        }
    }

    /// Does evaluating `e` store into a link of the place `base` (an assignment to the same
    /// field, an element, the same variable), or call a function that may change what an
    /// argument points to (a `BorrowMut` parameter, or a callee whose parameters are not known
    /// here)? `this.pos++` in `this.src[this.pos++]` stores no link of `this.src`.
    fn passes_changeable(&mut self, e: &hir::Expr, base: &hir::Expr) -> bool {
        use hir::ExprKind as K;
        let each =
            |lw: &mut Self, xs: &[&hir::Expr]| xs.iter().any(|x| lw.passes_changeable(x, base));
        match &e.kind {
            K::Lit(_) | K::Local(..) | K::Global(_) | K::FnRef(..) | K::Closure(_) => false,
            K::Assign { place, value } | K::CompoundAssign { place, value, .. } => {
                self.stores_link(place, base) || each(self, &[place, value])
            }
            K::Call { callee, args } => {
                let here = match callee {
                    // A generic callee's params may be changed through their bound
                    // (`setXs<T extends G>(o: T)`): its modes are not the instance's.
                    hir::Callee::Def(_, targs) if !targs.is_empty() => true,
                    hir::Callee::Def(d, _) => match self.cx.hir.def(*d) {
                        hir::Def::Fn(f) => f.params.iter().any(|p| p.mode == PassMode::BorrowMut),
                        _ => false,
                    },
                    hir::Callee::Intrinsic(i) => velt_sema::effects::runs_user_code(*i),
                    _ => true,
                };
                here || args.iter().any(|a| self.passes_changeable(a, base))
            }
            K::Unary { expr: x, .. }
            | K::Cast(x)
            | K::Upcast(x)
            | K::Downcast(x)
            | K::WrapSome(x)
            | K::UnwrapSome(x, _)
            | K::UnwrapVariant { expr: x, .. }
            | K::Field { base: x, .. } => each(self, &[x]),
            K::Index { base: b, index, .. } => each(self, &[b, index]),
            K::Binary { lhs, rhs, .. } | K::Logical { lhs, rhs, .. } => each(self, &[lhs, rhs]),
            K::If { cond, then, els } => each(self, &[cond, then, els]),
            K::ArrayLit(xs) => xs.iter().any(|x| self.passes_changeable(x, base)),
            K::ConstArray(c) => c
                .elem_exprs(e.span.file)
                .any(|x| self.passes_changeable(&x, base)),
            K::Block(b) => {
                b.stmts.iter().any(|st| match &st.kind {
                    hir::StmtKind::Let { init, .. } => init
                        .as_ref()
                        .is_some_and(|x| self.passes_changeable(x, base)),
                    hir::StmtKind::Expr(x) => self.passes_changeable(x, base),
                    _ => true,
                }) || b
                    .value
                    .as_deref()
                    .is_some_and(|x| self.passes_changeable(x, base))
            }
            _ => true,
        }
    }

    /// Does evaluating `e` assign a link of the place `base` ([`stores_link`](Self::stores_link)):
    /// `xs[(xs = ys).length - 1]` does; `this.i++` in `this.xs[this.i++]` assigns `this.i`, no
    /// link of `this.xs`, so the base needs no hold for it. Shapes not known here count.
    fn assigns_link(&mut self, e: &hir::Expr, base: &hir::Expr) -> bool {
        use hir::ExprKind as K;
        match &e.kind {
            K::Lit(_) | K::Global(_) | K::FnRef(..) | K::Local(..) | K::Closure(_) => false,
            K::Assign { place, value } | K::CompoundAssign { place, value, .. } => {
                self.stores_link(place, base)
                    || self.assigns_link(place, base)
                    || self.assigns_link(value, base)
            }
            K::Block(b) => {
                b.stmts.iter().any(|st| match &st.kind {
                    hir::StmtKind::Let { init, .. } => {
                        init.as_ref().is_some_and(|x| self.assigns_link(x, base))
                    }
                    hir::StmtKind::Expr(x) => self.assigns_link(x, base),
                    _ => true,
                }) || b
                    .value
                    .as_deref()
                    .is_some_and(|x| self.assigns_link(x, base))
            }
            _ => any_operand(e, &mut |x| self.assigns_link(x, base)).unwrap_or(true),
        }
    }

    /// May storing into `place` replace a link of the place `base`: the same field of the same
    /// type, an element where `base` goes through one, or the variable it starts from?
    fn stores_link(&mut self, place: &hir::Expr, base: &hir::Expr) -> bool {
        use hir::ExprKind as K;
        let mut p = place;
        while let K::UnwrapSome(x, _) | K::UnwrapVariant { expr: x, .. } | K::Downcast(x) = &p.kind
        {
            p = x;
        }
        match &p.kind {
            K::Field {
                base: pb, index, ..
            } => {
                let pt = self.sub(pb.ty);
                let key = self.field_key(pt);
                key.is_none() || self.links(base).contains(&Link::Field(key, *index))
            }
            K::Index { .. } => self.links(base).contains(&Link::Element),
            K::Local(l, _) => root_local(base) == Some(*l),
            K::Global(d) => self.links(base).contains(&Link::Global(*d)),
            _ => true,
        }
    }

    /// The links of the place `e`: its fields (by the type holding them), elements and root.
    fn links(&mut self, e: &hir::Expr) -> Vec<Link> {
        use hir::ExprKind as K;
        let mut out = vec![];
        let mut p = e;
        loop {
            match &p.kind {
                K::Field { base, index, .. } => {
                    let bt = self.sub(base.ty);
                    let key = self.field_key(bt);
                    out.push(Link::Field(key, *index));
                    p = base;
                }
                K::Index { base, .. } => {
                    out.push(Link::Element);
                    p = base;
                }
                K::UnwrapSome(x, _) | K::UnwrapVariant { expr: x, .. } | K::Downcast(x) => p = x,
                K::Global(d) => {
                    out.push(Link::Global(*d));
                    return out;
                }
                _ => return out,
            }
        }
    }

    /// The base `base`, read before an index that may store another value where it is read
    /// (module docs): a share of it, owned by the statement.
    fn hold_base(&mut self, base: &hir::Expr) -> Operand {
        let ty = self.sub(base.ty);
        let v = self.expr(base);
        if self.dead() {
            return v;
        }
        let held = self.share_value(v, ty);
        self.own_value(held, ty)
    }

    /// The key a field of a value of `ty` is assigned under: a class's fields by the root of
    /// its hierarchy (a field written through a subclass or a base class is the same field, at
    /// the same index), another type's by its definition.
    fn field_key(&mut self, ty: TyId) -> Option<hir::DefId> {
        match self.cx.kind(ty) {
            TyKind::Adt(d, _) if self.cx.is_class(ty) => Some(self.cx.class_root(d)),
            TyKind::Adt(d, _) => Some(d),
            _ => None,
        }
    }
}

/// The fields the program assigns (one walk over every body, once). A constructor's
/// assignments to its own object's fields (`this.xs = xs`) don't count: the object is new, so no
/// read of it is under way.
fn assigned_fields(hir: &hir::Program) -> AssignedFields {
    let mut out = AssignedFields::default();
    let ctors: FxHashSet<hir::DefId> = (hir.defs.iter())
        .filter_map(|d| match d {
            hir::Def::Adt(a) => a.ctor,
            _ => None,
        })
        .collect();
    let this_of = |d: hir::DefId| match hir.def(d) {
        hir::Def::Fn(f) if ctors.contains(&d) => f.params.first().map(|p| p.local),
        _ => None,
    };
    super::module_state::walk_writes(hir, &mut |d, e| match &e.kind {
        hir::ExprKind::Assign { place, .. } | hir::ExprKind::CompoundAssign { place, .. } => {
            let mut p = place.as_ref();
            // `o.f!.g = v` assigns `g`; a narrowed or cast place assigns the field under it.
            while let hir::ExprKind::UnwrapSome(x, _)
            | hir::ExprKind::UnwrapVariant { expr: x, .. }
            | hir::ExprKind::Downcast(x) = &p.kind
            {
                p = x;
            }
            if let hir::ExprKind::Field { base, index, .. } = &p.kind {
                if let hir::ExprKind::Local(l, _) = base.kind {
                    if this_of(d) == Some(l) {
                        return;
                    }
                }
                match hir.types.kind(base.ty) {
                    TyKind::Adt(d, _) => {
                        out.fields.insert((root_of(hir, *d), *index));
                    }
                    TyKind::Shared(_) | TyKind::Tuple(_) => {}
                    _ => out.any_key = true,
                }
            }
        }
        hir::ExprKind::Call {
            callee: hir::Callee::Intrinsic(Intrinsic::KeyWrite),
            ..
        } => out.any_key = true,
        _ => {}
    });
    out
}

/// The root of the class hierarchy of `d` (`d` itself for any other definition).
pub(super) fn root_of(hir: &hir::Program, mut d: hir::DefId) -> hir::DefId {
    while let hir::Def::Adt(a) = hir.def(d) {
        match a.base.map(|b| hir.types.kind(b)) {
            Some(TyKind::Adt(b, _)) => d = *b,
            _ => break,
        }
    }
    d
}
