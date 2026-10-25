//! A value of an object type where an interface of only accessors and fields is expected
//! (`const t: T = p` with `type T = { get v(): number; set v(x: number) }` and
//! `p: { v: number }`): TypeScript checks the shape, so it fits. The interface value is the same
//! object, through an implementation of the interface for the object type
//! (`Program::impls`, one per object type and interface instance) whose getter slots read its
//! fields and whose setter slots write them.

use rustc_hash::FxHashMap;

use crate::body::FnCx;
use crate::collect::getters::{field_getter, field_setter_taking};
use crate::hir::{self, AdtKind, DefId, ExprKind as H, ImplDef, PassMode, TyId, TyKind};
use crate::iface_writes::SetterSrc;

/// The implementations made so far, by (object type def, interface, interface arguments).
pub(crate) type ObjectImpls = FxHashMap<(DefId, DefId, Vec<TyId>), u32>;

impl FnCx<'_, '_> {
    /// `h`, of an object type, as interface value `exp` (`iface` with `args`); `Err(h)` when
    /// the interface has a method that is no accessor, or the object type lacks a member.
    pub(super) fn object_as_iface(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        iface: DefId,
        args: &[TyId],
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Adt(d, targs) = self.cx.ty.kind(h.ty).clone() else {
            return Err(h);
        };
        let anon = self.cx.adt(d).is_some_and(|a| a.kind == AdtKind::Anon);
        let mut params = vec![];
        for &a in args {
            crate::types::collect_params(&self.cx.ty, a, &mut params);
        }
        let generic = !params.is_empty();
        if !anon || !targs.is_empty() || generic {
            return Err(h);
        }
        let key = (d, iface, args.to_vec());
        let index = match self.cx.object_impls.get(&key) {
            Some(&i) => i,
            None => match self.object_impl(d, h.ty, iface, args) {
                Some(i) => {
                    self.cx.object_impls.insert(key, i);
                    i
                }
                None => return Err(h),
            },
        };
        let mut h = h;
        self.force_move(&mut h);
        let span = h.span;
        let kind = H::ToDyn {
            expr: Box::new(h),
            impl_index: index,
        };
        Ok(self.mk(kind, exp, span))
    }

    /// The implementation of `iface<args>` for object type `d` (type `self_ty`), when its
    /// fields give every member.
    fn object_impl(&mut self, d: DefId, self_ty: TyId, iface: DefId, args: &[TyId]) -> Option<u32> {
        let info = self.cx.iface(iface)?;
        let (methods, ifields) = (info.methods.clone(), info.fields.clone());
        let mut full = args.to_vec();
        full.push(self_ty);
        let fields: Vec<(String, TyId)> = (self.cx.adt(d)?.fields.iter())
            .map(|f| (f.name.clone(), f.ty))
            .collect();
        let find = |cx: &mut crate::ctx::Ctx, name: &str, want: TyId| {
            let i = fields.iter().position(|(n, _)| n == name)?;
            cx.same_ty(fields[i].1, want).then_some((i, fields[i].1))
        };
        let mut defs = vec![];
        for m in &methods {
            if !m.generics.names.is_empty() {
                return None;
            }
            let def = match m.name.strip_prefix("set ") {
                Some(name) => {
                    let p = m.params.first()?;
                    let want = self.cx.subst(p.ty, &full);
                    let (i, ty) = find(self.cx, name, want)?;
                    let borrowed = p.mode == PassMode::Borrow;
                    field_setter_taking(self.cx, d, self_ty, i, ty, borrowed)
                }
                None if m.is_getter => {
                    let want = self.cx.subst(m.ret, &full);
                    let (i, ty) = find(self.cx, &m.name, want)?;
                    field_getter(self.cx, d, self_ty, i, ty)
                }
                None => return None,
            };
            defs.push(def);
        }
        let mut setters = vec![];
        for f in &ifields {
            let want = self.cx.subst(f.ty, &full);
            let (i, ty) = find(self.cx, &f.name, want)?;
            defs.push(field_getter(self.cx, d, self_ty, i, ty));
            setters.push(SetterSrc::Field {
                d,
                self_ty,
                index: i,
                ty,
            });
        }
        let index = self.cx.impls.len() as u32;
        self.cx.impls.push(ImplDef {
            ty: self_ty,
            generics: 0,
            iface,
            iface_args: args.to_vec(),
            methods: defs,
        });
        self.cx.note_setters(index, setters);
        // The dispatch groups gain the new methods (`throws/groups.rs`): built again on next use.
        self.cx.groups = None;
        Some(index)
    }
}
