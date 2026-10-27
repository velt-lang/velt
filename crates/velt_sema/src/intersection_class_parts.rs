//! A class instance brand (`crate::brands`: `C & { $set(v: C): void }`, sigx's `Signal<T>` at a
//! class `T`) as the value of an intersection with a type parameter part in generic code
//! (`s.$set(v)` for `s: Signal<T>`, `crate::intersection_members`): generic code reads the
//! object part's members through an interface made of that part, which the brand's class
//! implements here, for that one brand's class type. Each member is read from the object at run
//! time, as through `any`, and checked as the member's type then (a reactive proxy's `get` trap
//! provides `$set`; the class's own fields are read through its `any` table too). A write of
//! such a member through the interface is an error (`SetterSrc::Missing`).

use velt_common::Span;

use crate::collect::getters::getter_def;
use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, FnKind, ThisSig};
use crate::hir::{
    self, Callee, Def, DefId, ExprKind as H, ImplDef, Intrinsic, LocalId, PassMode, TyId, UseMode,
};
use crate::iface_writes::SetterSrc;

impl Ctx<'_> {
    /// The impl of part interface `iface` (made of `part`) for the class of brand `r`, when `r`
    /// brands a class instance and its object part has `part`'s fields. True when `r` is such a
    /// brand (linked here or not).
    pub(crate) fn link_class_part_iface(&mut self, iface: DefId, part: TyId, r: TyId) -> bool {
        let (Some(base), Some(tag)) = (self.brand_base(r), self.brand_tag(r)) else {
            return false;
        };
        let Some((class, _)) = self.class_of(base) else {
            return false;
        };
        let hir::TyKind::Adt(brand, _) = *self.ty.kind(r) else {
            return false;
        };
        if self.part_ifaces.linked_brands.contains(&(iface, brand)) {
            return true;
        }
        let have = self.public_fields(tag);
        let want = self.public_fields(part);
        let n = self.iface(iface).map_or(0, |i| i.generics.len());
        let mut slots: Vec<Option<TyId>> = vec![None; n];
        let mut found = vec![];
        for f in &want {
            let Some(g) = have.iter().find(|g| g.name == f.name) else {
                return true;
            };
            if !self.match_ty(f.ty, g.ty, &mut slots) {
                return true;
            }
            found.push(g.clone());
        }
        let Some(args) = slots.into_iter().collect::<Option<Vec<TyId>>>() else {
            return true;
        };
        for (f, g) in want.iter().zip(&found) {
            if self.subst(f.ty, &args) != g.ty {
                return true;
            }
        }
        // Without `any` in the program (no prelude) nothing reads a member the class lacks.
        let Some(defs) = self.any else {
            return true;
        };
        let any_ty = self.ty.intern(hir::TyKind::Dyn(defs.any, vec![]));
        let Some(imp) = self.any_impl() else {
            return true;
        };
        self.part_ifaces.linked_brands.insert((iface, brand));
        let span = self.adt(class).map_or(Span::DUMMY, |a| a.span);
        let methods = (found.iter())
            .map(|g| self.dynamic_getter(class, base, &g.name, g.ty, (any_ty, imp), span))
            .collect();
        let setters = (found.iter())
            .map(|g| SetterSrc::Missing {
                span,
                name: g.name.clone(),
            })
            .collect();
        self.impls.push(ImplDef {
            ty: base,
            generics: 0,
            iface,
            iface_args: args,
            methods,
        });
        let index = (self.impls.len() - 1) as u32;
        self.note_setters(index, setters);
        true
    }

    /// A getter `C.<any name>` (the name velt_vir's typed proxies know it by) of the class
    /// instance type `self_ty` (of class `d`) that reads member
    /// `name` from the object through `any` and converts it to `ty`.
    fn dynamic_getter(
        &mut self,
        d: DefId,
        self_ty: TyId,
        name: &str,
        ty: TyId,
        any: (TyId, u32),
        span: Span,
    ) -> DefId {
        let qual = self
            .adt(d)
            .map_or_else(String::new, |a| a.qual_name.clone());
        let module = self.adt(d).map_or(0, |a| a.module);
        let fname = format!("{qual}.<any {name}>");
        let mut info =
            crate::collect::fn_placeholder(fname.clone(), span, span, module, FnKind::Method, None);
        info.this = Some(ThisSig {
            ty: self_ty,
            mode: PassMode::Borrow,
        });
        info.ret = ty;
        info.fixed_modes = true;
        info.state = BodyState::Done;
        info.owner = Some(d);
        info.local_kinds = vec![crate::body::LocalKind::This];
        let def = self.alloc_def(span, DefInfo::Fn(Box::new(info)));
        let (any, imp) = any;
        self.note_dyn_members();
        let call = |i: Intrinsic, args: Vec<hir::Expr>, ty: TyId| hir::Expr {
            kind: H::Call {
                callee: Callee::Intrinsic(i),
                args,
            },
            ty,
            span,
        };
        let this = hir::Expr {
            kind: H::Local(LocalId(0), UseMode::Borrow),
            ty: self_ty,
            span,
        };
        let shared = call(Intrinsic::Share, vec![this], self_ty);
        let boxed = hir::Expr {
            kind: H::ToDyn {
                expr: Box::new(shared),
                impl_index: imp,
            },
            ty: any,
            span,
        };
        let key = hir::Expr {
            kind: H::Lit(hir::Lit::Str(name.to_string())),
            ty: self.ty.str_,
            span,
        };
        let read = call(Intrinsic::AnyGet, vec![boxed, key], any);
        let value = call(Intrinsic::AnyTo, vec![read], ty);
        self.defs[def.0 as usize] = Some(Def::Fn(getter_def(fname, 0, self_ty, ty, value, span)));
        def
    }
}
