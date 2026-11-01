//! `AnyTo` an interface type `I<B>` (any.rs `any_unbox`) when the `any` holds a value of a type
//! implementing `I<A>`: an object literal's struct or a class instance, which is what a
//! conversion of an interface value to `any` holds (`make_dyn_any`). The value becomes an
//! `I<A>` through its own implementation (or that of the nearest base class implementing the
//! interface) and, when `A` is not `B`, an `I<B>` through the instance of the generic adapter
//! sema made for `I<B>` (velt_sema `iface_adapt_dynamic.rs`) at `A`, as Node keeps the object.
//!
//! The conversion is the glue `AnyIface` of `I<B>`, `(data, desc, out) -> bool`: one comparison
//! of the `any`'s descriptor per type converting to `I<B>`, each with that type's own
//! implementation and arguments. A value of another type converts through none of them (the
//! caller then reports a `TypeError`). The types are those the program's `any` values may hold
//! (`facts.dyn_types`), which are known only once everything else is lowered: the glue is
//! built last and again whenever more such types appeared ([`Cx::build_iface_glues`]).

use velt_sema::hir::{DefId, TyId, TyKind};

use super::glue::Glue;
use super::operand::proj;
use super::{Cx, FnLower, ScopeKind, VtableKey, Work};
use crate::vir::{self, BinOp, Callee, Const, Operand, Place, Rvalue, Terminator, Ty};

/// How a value of one concrete type converts to the target: its implementation and the type
/// (it or a base class) the implementation is for, and the adapter to the target's arguments
/// (its implementation, its instance, the wrapped interface type).
struct Source {
    desc_ty: TyId,
    imp: u32,
    imp_ty: TyId,
    adapter: Option<(u32, TyId, TyId)>,
}

impl Cx<'_> {
    /// The implementation of `iface` a value of the concrete type `t` converts through: that of
    /// `t` or of its nearest base class implementing it, as the implementation index, the type
    /// it is for, and the interface's arguments.
    /// An `extend … implements` block's impl comes after those, as in a direct conversion
    /// (impl_lookup.rs).
    fn iface_impl_of(
        &mut self,
        t: TyId,
        iface: DefId,
        to: &[TyId],
    ) -> Option<(u32, TyId, Vec<TyId>)> {
        if let Some(found) = self.own_iface_impl_of(t, iface, to) {
            return Some(found);
        }
        if self.hir.extension_impls.is_empty() {
            return None;
        }
        let fit = self.ext_impl_for(iface, None, t)?;
        let unknown = self.intern(TyKind::Error);
        let xs: Vec<TyId> = fit.binds.iter().map(|b| b.unwrap_or(unknown)).collect();
        let args = self.hir.impls[fit.index as usize].iface_args.clone();
        let from: Vec<TyId> = args.iter().map(|a| self.subst(*a, &xs)).collect();
        from.iter()
            .all(|a| self.closed(*a, 0))
            .then_some((fit.index, fit.ty, from))
    }

    /// [`Self::iface_impl_of`] among the impls of the type and its base classes: the first one,
    /// or one at the arguments `to` when the type has several (an object type converted to the
    /// interface at several arguments, velt_sema `object_ifaces.rs`).
    fn own_iface_impl_of(
        &mut self,
        t: TyId,
        iface: DefId,
        to: &[TyId],
    ) -> Option<(u32, TyId, Vec<TyId>)> {
        let mut cur = t;
        let impls = self.impls_of(iface);
        let mut first = None;
        for _ in 0..64 {
            for &j in impls.iter() {
                if self.hir.extension_impls.contains_key(&j) {
                    continue;
                }
                let j = j as usize;
                let imp = &self.hir.impls[j];
                if imp.iface != iface || self.is_adapter(imp.ty) || self.is_dynamic(imp.ty) {
                    continue;
                }
                let (ty, n, args) = (imp.ty, imp.generics, imp.iface_args.clone());
                if n == 0 {
                    if ty == cur && args.as_slice() == to {
                        return Some((j as u32, cur, args));
                    }
                    if ty == cur && first.is_none() {
                        first = Some((j as u32, cur, args));
                    }
                    continue;
                }
                if first.is_some() {
                    continue;
                }
                if let Some(xs) = self.identity_instance(ty, n, cur) {
                    let from: Vec<TyId> = args.iter().map(|a| self.subst(*a, &xs)).collect();
                    if from.iter().all(|a| self.closed(*a, 0)) {
                        return Some((j as u32, cur, from));
                    }
                }
            }
            if first.is_some() {
                return first;
            }
            let TyKind::Adt(d, xs) = self.kind(cur) else {
                return None;
            };
            match self.adt_def(d).base {
                Some(b) if self.is_class(cur) => cur = self.subst(b, &xs),
                _ => return None,
            }
        }
        None
    }

    /// The type arguments of `t` when it is an instance of `pat`, a generic type whose type
    /// arguments are among its implementation's `generics` parameters (`G<T0, …>`; an object
    /// literal's struct in a generic function may name some of them only, in another order).
    fn identity_instance(&mut self, pat: TyId, generics: u32, t: TyId) -> Option<Vec<TyId>> {
        let (TyKind::Adt(d, params), TyKind::Adt(e, args)) = (self.kind(pat), self.kind(t)) else {
            return None;
        };
        if d != e || params.len() != args.len() {
            return None;
        }
        let top = params.iter().filter_map(|p| match self.kind(*p) {
            TyKind::Param(n) => Some(n + 1),
            _ => None,
        });
        let n = top.max().unwrap_or(0).max(generics).min(64);
        let mut slots: Vec<Option<TyId>> = vec![None; n as usize];
        for (p, a) in params.iter().zip(&args) {
            match self.kind(*p) {
                TyKind::Param(n) if (n as usize) < slots.len() => match slots[n as usize] {
                    Some(b) if b != *a => return None,
                    _ => slots[n as usize] = Some(*a),
                },
                _ => return None,
            }
        }
        // A parameter the type does not name stands for an unknown type (the implementation's
        // arguments must not mention it, `iface_impl_of`).
        let unknown = self.intern(TyKind::Error);
        Some(slots.into_iter().map(|s| s.unwrap_or(unknown)).collect())
    }

    /// The generic adapter sema made to `iface<to>`: its implementation and its struct.
    fn dynamic_adapter(&self, iface: DefId, to: &[TyId]) -> Option<(u32, DefId)> {
        (self.hir.impls.iter().enumerate()).find_map(|(k, a)| {
            if a.iface != iface || a.generics == 0 || a.iface_args != to || !self.is_adapter(a.ty) {
                return None;
            }
            match self.kind(a.ty) {
                TyKind::Adt(d, _) => Some((k as u32, d)),
                _ => None,
            }
        })
    }

    /// The instance of the adapter `(k, d)` to `iface<to>` for a value of `iface<from>`, and the
    /// interface type it wraps: `from`'s arguments where `to`'s are dynamic, the others equal.
    /// `None` when the arguments differ elsewhere or one is a type the adapter's slots cannot
    /// box (a promise, `void`, `never`).
    fn adapter_instance(&mut self, (k, d): (u32, DefId), from: &[TyId]) -> Option<(TyId, TyId)> {
        let imp = &self.hir.impls[k as usize];
        let (iface, to) = (imp.iface, imp.iface_args.clone());
        let inner = self.adt_def(d).fields[0].ty;
        let TyKind::Dyn(_, pattern) = self.kind(inner) else {
            return None;
        };
        if pattern.len() != from.len() {
            return None;
        }
        let mut args = vec![];
        for ((p, f), t) in pattern.iter().zip(from).zip(&to) {
            match self.kind(*p) {
                TyKind::Param(_) if self.boxable(*f, 0) => args.push(*f),
                _ if p == t && f == t => {}
                _ => return None,
            }
        }
        let aty = self.intern(TyKind::Adt(d, args));
        let wrapped = self.intern(TyKind::Dyn(iface, from.to_vec()));
        Some((aty, wrapped))
    }

    /// Can a value of the concrete type `t` go into an `any` and come back (what a generic
    /// adapter's slots do with its type arguments)?
    fn boxable(&self, t: TyId, depth: u32) -> bool {
        let all = |ts: &[TyId]| ts.iter().all(|x| self.boxable(*x, depth + 1));
        if depth > 32 {
            return false;
        }
        match self.types.kind(t) {
            TyKind::Param(_) | TyKind::Error | TyKind::Op(..) => false,
            TyKind::Promise(..) | TyKind::Unit | TyKind::Never => false,
            TyKind::Adt(_, xs) | TyKind::Dyn(_, xs) | TyKind::Tuple(xs) => all(xs),
            TyKind::Array(x) | TyKind::Option(x, _) | TyKind::Shared(x) => all(&[*x]),
            TyKind::Map(a, b) | TyKind::Result(a, b) => all(&[*a, *b]),
            TyKind::FnPtr {
                params,
                ret,
                throws,
                ..
            } => all(params) && all(&[*ret]) && self.closed(*throws, depth + 1),
            _ => true,
        }
    }

    /// Does `t` mention no type parameter (nor a deferred type)?
    fn closed(&self, t: TyId, depth: u32) -> bool {
        let all = |ts: &[TyId]| ts.iter().all(|x| self.closed(*x, depth + 1));
        if depth > 32 {
            return false;
        }
        match self.types.kind(t) {
            TyKind::Param(_) | TyKind::Error | TyKind::Op(..) => false,
            TyKind::Adt(_, xs) | TyKind::Dyn(_, xs) | TyKind::Tuple(xs) => all(xs),
            TyKind::Array(x) | TyKind::Option(x, _) | TyKind::Shared(x) => all(&[*x]),
            TyKind::Map(a, b) | TyKind::Promise(a, b) | TyKind::Result(a, b) => all(&[*a, *b]),
            TyKind::FnPtr {
                params,
                ret,
                throws,
                ..
            } => all(params) && all(&[*ret, *throws]),
            _ => true,
        }
    }

    /// The types `any` values hold that convert to the interface type `ty` (module docs).
    fn iface_sources(&mut self, ty: TyId) -> Vec<Source> {
        let TyKind::Dyn(iface, to) = self.kind(ty) else {
            return vec![];
        };
        let mut held: Vec<TyId> = self.facts.dyn_types.iter().copied().collect();
        held.sort_unstable_by_key(|t| t.0);
        let adapter = self.dynamic_adapter(iface, &to);
        let mut out = vec![];
        for t in held {
            if self.is_adapter(t) || self.is_dynamic(t) || !self.closed(t, 0) {
                continue;
            }
            let Some((imp, imp_ty, from)) = self.iface_impl_of(t, iface, &to) else {
                continue;
            };
            let adapter = match (from == to, adapter) {
                (true, _) => None,
                (false, Some(a)) => match self.adapter_instance(a, &from) {
                    Some((aty, wrapped)) => Some((a.0, aty, wrapped)),
                    None => continue,
                },
                (false, None) => continue,
            };
            out.push(Source {
                desc_ty: t,
                imp,
                imp_ty,
                adapter,
            });
        }
        out
    }

    /// Build the `AnyIface` glues (module docs) once everything else is lowered, and again
    /// while building them (or what they reach) adds types `any` values hold. Returns whether
    /// it built any, so the caller drains the queue again.
    pub(super) fn build_iface_glues(&mut self) -> bool {
        let mut built = false;
        loop {
            let held = self.facts.dyn_types.len();
            let stale: Vec<(vir::FuncId, Work)> = (self.iface_glues.iter())
                .filter(|g| g.2 != Some(held))
                .map(|g| (g.0, g.1.clone()))
                .collect();
            if stale.is_empty() {
                return built;
            }
            for (fid, work) in stale {
                self.funcs[fid.0 as usize] = None;
                self.build_now(fid, &work);
                if let Some(g) = self.iface_glues.iter_mut().find(|g| g.0 == fid) {
                    g.2 = Some(held);
                }
                built = true;
            }
            while let Some((fid, work)) = self.queue.pop_front() {
                self.build_now(fid, &work);
            }
        }
    }
}

impl FnLower<'_, '_> {
    /// The `any` at `p` as the interface type `ty` through an implementation of its interface
    /// (module docs): on a match the value goes into `out` and on to `done`; otherwise the code
    /// goes on in the current block.
    pub(super) fn any_to_iface(&mut self, p: &Place, ty: TyId, out: &Place, done: vir::BlockId) {
        if !matches!(self.cx.kind(ty), TyKind::Dyn(..))
            || self.cx.is_dynamic(ty)
            || self.cx.is_narrow_dynamic(ty)
        {
            return;
        }
        let f = self.cx.late_glue(Glue::AnyIface, ty);
        let vt = self.cx.ty(ty);
        let slot = Place::local(self.temp(vt));
        let ok = Place::local(self.temp(Ty::Bool));
        let args = vec![
            Operand::Copy(proj(p, vir::Proj::Field(0))),
            Operand::Copy(proj(p, vir::Proj::Field(1))),
            self.rvalue_temp(Ty::Ptr, Rvalue::AddrOf(slot.clone())),
        ];
        self.call(Callee::Func(f), args, Some(ok.clone()), false);
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(Operand::Copy(ok), yes, no);
        self.switch_to(yes);
        self.assign(out.clone(), Rvalue::Use(Operand::Copy(slot)));
        self.goto(done);
        self.switch_to(no);
    }

    /// `Glue::AnyIface` of the interface type `ty`: the `any` of data `data` and descriptor
    /// `desc` as a `ty` written to `out` (taking the data) and `true`, or `false` (module docs).
    pub(super) fn any_iface_body(
        &mut self,
        data: vir::Local,
        desc: vir::Local,
        out: vir::Local,
        ty: TyId,
    ) {
        let any_imp = self.cx.hir.any_impl.unwrap_or(0);
        let dest = self.deref_param(out, ty);
        for s in self.cx.iface_sources(ty) {
            // A generic closure's table is per instance: `any_to_generic_fn` finds it.
            if matches!(self.cx.kind(s.imp_ty), TyKind::Closure(_)) {
                continue;
            }
            let own = self.vtable_addr(VtableKey::Impl(any_imp, s.desc_ty));
            let d = Operand::Copy(Place::local(desc));
            let hit = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, d, own));
            let (yes, no) = (self.new_block(), self.new_block());
            self.branch(hit, yes, no);
            self.switch_to(yes);
            self.push_scope(ScopeKind::Temps);
            let mid = s.adapter.map_or(ty, |a| a.2);
            let table = self.vtable_addr(VtableKey::Impl(s.imp, s.imp_ty));
            let v = self.build_agg(mid, vec![Operand::Copy(Place::local(data)), table]);
            let mut v = self.take_owned(v);
            if let Some((k, aty, wrapped)) = s.adapter {
                // Values of the wrapped type are compared by identity (adapters.rs).
                self.cx.note_identity(wrapped);
                let w = self.build_agg(aty, vec![v]);
                let w = self.take_owned(w);
                let r = self.value_to_dyn(w, aty, ty, k);
                v = self.take_owned(r);
            }
            self.assign(dest.clone(), Rvalue::Use(v));
            self.pop_scope();
            self.terminate(Terminator::Return(Operand::Const(
                Const::Bool(true),
                Ty::Bool,
            )));
            self.switch_to(no);
        }
        self.terminate(Terminator::Return(Operand::Const(
            Const::Bool(false),
            Ty::Bool,
        )));
    }
}

impl Cx<'_> {
    /// The glue `g` of `ty` that depends on the types `any` values hold (`AnyIface`,
    /// `AnyNumeric`), built last ([`Cx::build_iface_glues`]).
    pub(super) fn late_glue(&mut self, g: Glue, ty: TyId) -> vir::FuncId {
        let work = Work::Glue(g, ty);
        if let Some(&f) = self.work_map.get(&work) {
            return f;
        }
        let f = vir::FuncId(self.funcs.len() as u32);
        self.funcs.push(None);
        self.work_map.insert(work.clone(), f);
        self.iface_glues.push((f, work, None));
        f
    }
}
