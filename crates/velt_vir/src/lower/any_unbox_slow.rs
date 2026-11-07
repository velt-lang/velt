//! The slow path of `AnyTo` (any.rs `any_unbox`): what runs when the `any`'s descriptor is not
//! the target type's own. It tries every other way the value converts (a record built by `{}`,
//! a function of another type, an interface, a proxy or a view, a record of an object's
//! entries, an array view, a copy of an object's members, a number of another numeric type)
//! and stops with the `TypeError` when none applies. For a union target it is what follows the
//! comparisons with its members' descriptors (any_set.rs `any_unbox_union`): a view of an
//! object or array of another type as a member, or the `TypeError`.
//!
//! Each of those is a sequence of tests, and a conversion to an object type converts each of
//! its members the same way, so written out at every site the slow path made one `as VNode`
//! of a nine-member object type about 1,400 blocks. A function with a few dozen such sites (a
//! renderer walking JSX trees through `any`) became tens of thousands of blocks, and every
//! analysis and code generator after lowering paid for them. So a site keeps only the
//! comparison with the target's descriptor and the move of the value out of its box (the fast
//! path) and calls the glue `AnyUnbox` of the target for the rest: one function per target
//! type, whose member conversions call their own targets' glue in turn.
//!
//! A site stays written out when its `TypeError` names what it converts (`any_context`: the
//! glue's message has no such words), when the value is not an `any` (the glue takes one), and
//! for numeric targets, whose slow path is already one call (`Glue::AnyNumericOrError`).

use velt_sema::hir::{TyId, TyKind};

use super::glue::Glue;
use super::operand::proj;
use super::{FnLower, ScopeKind, Work};
use crate::vir::{self, BlockId, Callee, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// Where `any_unbox` found that the `any` at `p` (of type `from`) does not hold a `ty` of
    /// the target's own descriptor: every other conversion, writing `out` and going on to
    /// `done`, or the `TypeError` (module docs). The current block is the one to start in.
    pub(super) fn any_unbox_rest(
        &mut self,
        p: &Place,
        from: TyId,
        ty: TyId,
        out: &Place,
        done: BlockId,
    ) {
        if self.unbox_outlined(from, ty) {
            return self.any_unbox_rest_call(p, ty, out, done);
        }
        self.any_unbox_rest_here(p, from, ty, out, done);
    }

    /// Does the slow path of converting an `any` of type `from` to `ty` go through
    /// `Glue::AnyUnbox` (module docs)?
    fn unbox_outlined(&mut self, from: TyId, ty: TyId) -> bool {
        if self.any_context.is_some() || self.dead() {
            return false;
        }
        if matches!(self.cx.kind(ty), TyKind::Int(_) | TyKind::Float(_)) {
            return false;
        }
        from == self.cx.any_ty()
    }

    /// The call of `Glue::AnyUnbox` of `ty` for the `any` at `p`, which it takes: the value is
    /// written to `out`, then on to `done`. The glue returns only with a value.
    fn any_unbox_rest_call(&mut self, p: &Place, ty: TyId, out: &Place, done: BlockId) {
        let f = self
            .cx
            .func_direct(Work::Glue(Glue::AnyUnbox, ty), self.mode);
        let vt = self.cx.ty(ty);
        let slot = Place::local(self.temp(vt));
        let args = vec![
            Operand::Copy(proj(p, Proj::Field(0))),
            Operand::Copy(proj(p, Proj::Field(1))),
            self.rvalue_temp(Ty::Ptr, Rvalue::AddrOf(slot.clone())),
        ];
        self.call(Callee::Func(f), args, None, false);
        self.assign(out.clone(), Rvalue::Use(Operand::Copy(slot)));
        self.goto(done);
    }

    /// `Glue::AnyUnbox` of `ty`: the slow path (module docs) for the `any` of data `data` and
    /// descriptor `desc`, which it takes, writing the `ty` to `out` or stopping with the
    /// `TypeError`.
    pub(super) fn any_unbox_body(
        &mut self,
        data: vir::Local,
        desc: vir::Local,
        out: vir::Local,
        ty: TyId,
    ) {
        let any = self.cx.any_ty();
        let av = self.cx.ty(any);
        let p = Place::local(self.temp(av));
        let Ty::Agg(a) = av else {
            super::ice("`any` is not an aggregate")
        };
        let parts = vec![
            Operand::Copy(Place::local(data)),
            Operand::Copy(Place::local(desc)),
        ];
        self.assign(p.clone(), Rvalue::Aggregate(a, parts));
        let dest = self.deref_param(out, ty);
        let done = self.new_block();
        self.push_scope(ScopeKind::Temps);
        self.any_unbox_rest_here(&p, any, ty, &dest, done);
        if !self.dead() {
            self.terminate(Terminator::Unreachable);
        }
        self.switch_to(done);
        // Every way through ends here, so a temporary left to this scope was written on one of
        // them only: its drop would read an unwritten place on the others.
        debug_assert!(
            self.scopes.last().is_none_or(|s| s.drops.is_empty()),
            "ICE: a conversion from `any` left a temporary to the scope of its glue"
        );
        self.pop_scope();
        self.terminate(Terminator::Return(super::unit()));
    }

    /// The slow path written out here (module docs).
    fn any_unbox_rest_here(&mut self, p: &Place, from: TyId, ty: TyId, out: &Place, done: BlockId) {
        let desc = Operand::Copy(proj(p, Proj::Field(1)));
        if self.cx.is_union(ty) {
            // No member's own descriptor (any_set.rs `any_unbox_union`): an object or array of
            // another type, as a view of it as a member.
            let n = match self.cx.kind(ty) {
                TyKind::Adt(d, _) => self.cx.enum_def(d).variants.len() as u32,
                _ => 0,
            };
            self.union_array_view(p, desc.clone(), ty, n, out, done);
            self.union_object_view(p, desc.clone(), ty, n, out, done);
            self.any_type_error(desc, ty);
            return;
        }
        // A record built by `{} as D` (sema's `empty_as_deferred`) where `D` is an object type.
        self.any_record_fill(p, from, ty, out, done);
        // A generic function value held as `unknown` (glue/vtable.rs `ClosureAny`).
        self.any_to_generic_fn(p, ty, desc.clone(), out, done);
        // A function of another type (`Program::any_fn_views`, any_generic_fn.rs).
        self.any_to_fn_view(p, ty, desc.clone(), out, done);
        // An object implementing the interface, or the interface at other type arguments.
        self.any_to_iface(p, ty, out, done);
        if self.cx.proxiable(ty) {
            // A proxy converted to a type that may hold one: the tagged `Proxy` object.
            let err = self.new_block();
            let data = Operand::Copy(proj(p, Proj::Field(0)));
            self.proxy_unbox(data, desc.clone(), out, done, err);
            self.switch_to(err);
            // `null` or `undefined`: a view of the nothing (any_view.rs).
            self.any_nothing_view(p, out, done);
            // An object of another type: a view of it (any_view.rs).
            self.any_view(p, ty, out, done);
        }
        // An object read as a `Record<string, V>`: a record of its entries (any_to_record.rs).
        self.any_to_record(p, from, ty, out, done);
        if self.cx.array_view(ty) {
            // An array of another element type: a view of it (array_view.rs).
            self.array_view_make(p, desc.clone(), ty, out, done);
        }
        if !self.cx.proxiable(ty) {
            // An object of another object type, where no view can be made: a copy of its
            // members (any_object_fill.rs).
            self.any_object_fill(p, from, ty, out, done);
        }
        // A number of another numeric type (any_number.rs).
        if !self.any_number_convert(p, ty, desc.clone(), out, done) {
            self.any_type_error(desc, ty);
        }
    }
}
