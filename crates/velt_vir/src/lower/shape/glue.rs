//! The conversions to and from `any` that a shared instance cannot write out: their code
//! depends on the whole type (its `any` table, its members' checks), so the shared body
//! calls them through its dictionary, as `Glue::ToAny`/`ToAnyParam` and `Glue::FromAny` of
//! each concrete type. The bodies are what `make_dyn` and `any_to` write at a site of a
//! concrete type.

use velt_sema::hir::{self, TyId, TyKind};

use super::super::glue::Glue;
use super::super::{ice, unit, FnLower, ScopeKind, Work};
use crate::vir::{self, Callee, Operand, Place, Rvalue, Terminator};

impl FnLower<'_, '_> {
    /// `make_dyn` of `e` (of a type built from a placeholder, `cty`) as `ty`, in a shared
    /// instance: through `Glue::ToAny` of the concrete type.
    pub(in crate::lower) fn shape_to_dyn(
        &mut self,
        e: &hir::Expr,
        impl_index: u32,
        cty: TyId,
        ty: TyId,
    ) -> Operand {
        let any = self.cx.any_ty();
        if !self.cx.is_any_impl(impl_index) || ty != any {
            ice("a shared instance converts a value to an interface other than `any`");
        }
        let param = matches!(self.cx.kind(e.ty), TyKind::Param(_));
        let v = self.consume(e);
        if self.dead() {
            return unit();
        }
        let g = if param { Glue::ToAnyParam } else { Glue::ToAny };
        let vt = self.cx.ty(cty);
        let src = self.copy_to_temp(v, vt);
        let avt = self.cx.ty(any);
        let out = self.temp(avt);
        let args = vec![self.addr(Place::local(src)), self.addr(Place::local(out))];
        let f = self.cx.func_direct(Work::Glue(g, cty), self.mode);
        self.call(Callee::Func(f), args, None, false);
        Operand::Copy(Place::local(out))
    }

    /// `any_to` of `a` (an `any`) as `ty`, a type built from a placeholder, in a shared
    /// instance: through `Glue::FromAny` of the concrete type.
    pub(in crate::lower) fn shape_any_to(&mut self, a: &hir::Expr, ty: TyId) -> Operand {
        let from = self.sub(a.ty);
        if from != self.cx.any_ty() {
            ice("a shared instance converts to its type argument from a type other than `any`");
        }
        let v = self.consume(a);
        let vt = self.cx.ty(ty);
        if self.dead() {
            return self.zero_value(vt);
        }
        let fvt = self.cx.ty(from);
        let src = self.copy_to_temp(v, fvt);
        let out = self.temp(vt);
        let args = vec![self.addr(Place::local(src)), self.addr(Place::local(out))];
        let f = self
            .cx
            .func_direct(Work::Glue(Glue::FromAny, ty), self.mode);
        self.call(Callee::Func(f), args, None, false);
        self.own_value(Operand::Copy(Place::local(out)), ty)
    }

    /// `Glue::ToAny` / `ToAnyParam` of `ty`: `make_dyn` after its operand is evaluated.
    pub(in crate::lower) fn glue_to_any_body(
        &mut self,
        src: vir::Local,
        out: vir::Local,
        ty: TyId,
        g: Glue,
    ) {
        let outer = std::mem::replace(&mut self.cx.shapes.std_glue, true);
        let any = self.cx.any_ty();
        let imp = self
            .cx
            .hir
            .any_impl
            .unwrap_or_else(|| ice("`any` without its impl"));
        let v = Operand::Copy(self.deref_param(src, ty));
        let dest = self.deref_param(out, any);
        self.push_scope(ScopeKind::Temps);
        let r = match g == Glue::ToAnyParam && self.any_unpacks(ty) {
            true => self.any_unpack(v, ty, any),
            false => self.value_to_dyn(v, ty, any, imp),
        };
        if !self.dead() {
            // The value goes to the caller: not dropped with this scope's temporaries.
            if let Operand::Copy(p) = &r {
                self.take_temp(p);
            }
            self.assign(dest, Rvalue::Use(r));
        }
        self.pop_scope();
        self.terminate(Terminator::Return(unit()));
        self.cx.shapes.std_glue = outer;
    }

    /// `Glue::FromAny` of `ty`: `any_to` of an `any` after it is evaluated.
    pub(in crate::lower) fn glue_from_any_body(
        &mut self,
        src: vir::Local,
        out: vir::Local,
        ty: TyId,
    ) {
        let outer = std::mem::replace(&mut self.cx.shapes.std_glue, true);
        let any = self.cx.any_ty();
        let av = self.cx.ty(any);
        let p = Place::local(self.temp(av));
        let v = Operand::Copy(self.deref_param(src, any));
        self.assign(p.clone(), Rvalue::Use(v));
        let dest = self.deref_param(out, ty);
        self.push_scope(ScopeKind::Temps);
        let r = self.any_unbox(&p, any, ty);
        if !self.dead() {
            // The value goes to the caller: not dropped with this scope's temporaries.
            if let Operand::Copy(p) = &r {
                self.take_temp(p);
            }
            self.assign(dest, Rvalue::Use(r));
        }
        self.pop_scope();
        self.terminate(Terminator::Return(unit()));
        self.cx.shapes.std_glue = outer;
    }
}
