//! `AnyTo` an interface type `I<B>` (any.rs `any_unbox`) when the `any` holds a value of a type
//! implementing `I<A>`: an object literal's struct or a class instance, which is what a
//! conversion of an interface value to `any` holds (`make_dyn_any`). The value becomes an
//! `I<A>` through its own implementation and, when `A` is not `B`, an `I<B>` through the
//! adapter sema made for that pair (velt_sema `iface_adapt_dynamic.rs`), as Node keeps the
//! object. Each implementation is one comparison of the descriptor; a class instance is also
//! taken by an implementation of one of its base classes.

use velt_sema::hir::{DefId, TyId, TyKind};

use super::operand::proj;
use super::{cint, Cx, FnLower, VtableKey};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Ty};

/// An implementation an `any` value may convert through: its self type and index, and the
/// adapter to the expected arguments (its struct type, its implementation, the wrapped type).
struct Source {
    self_ty: TyId,
    imp: u32,
    adapter: Option<(TyId, u32, TyId)>,
}

impl Cx<'_> {
    /// The implementations of `iface` a value converts through to `iface<to>`, deeper classes
    /// first (an instance of a subclass is tested by its own implementation before its base's).
    fn iface_sources(&mut self, iface: DefId, to: &[TyId]) -> Vec<Source> {
        let mut out = vec![];
        let mut found = vec![];
        for (j, imp) in self.hir.impls.iter().enumerate() {
            if imp.iface != iface || self.is_adapter(imp.ty) || self.is_dynamic(imp.ty) {
                continue;
            }
            if imp.generics == 0 {
                found.push((j as u32, imp.ty, imp.iface_args.clone()));
                continue;
            }
            let (ty, args, n) = (imp.ty, imp.iface_args.clone(), imp.generics);
            for xs in self.generic_instances(ty, n) {
                let self_ty = self.subst(ty, &xs);
                let from = args.iter().map(|a| self.subst(*a, &xs)).collect();
                found.push((j as u32, self_ty, from));
            }
        }
        for (j, self_ty, from) in found {
            if from == to {
                out.push(Source {
                    self_ty,
                    imp: j,
                    adapter: None,
                });
                continue;
            }
            let impls = &self.hir.impls;
            let adapter = impls.iter().enumerate().find_map(|(k, a)| {
                if a.iface != iface || a.iface_args != to || !self.is_adapter(a.ty) {
                    return None;
                }
                let TyKind::Adt(d, _) = self.kind(a.ty) else {
                    return None;
                };
                let inner = self.adt_def(d).fields[0].ty;
                match self.kind(inner) {
                    TyKind::Dyn(i, args) if i == iface && args == from => {
                        Some((a.ty, k as u32, inner))
                    }
                    _ => None,
                }
            });
            if adapter.is_some() {
                out.push(Source {
                    self_ty,
                    imp: j,
                    adapter,
                });
            }
        }
        out.sort_by_key(|s| std::cmp::Reverse(self.class_depth(s.self_ty)));
        out
    }

    /// The type arguments of the concrete instances of the generic class `self_ty` (`G<T0, …>`,
    /// `generics` parameters in order) the program names (velt_sema `generic_instances`).
    fn generic_instances(&self, self_ty: TyId, generics: u32) -> Vec<Vec<TyId>> {
        let TyKind::Adt(d, params) = self.kind(self_ty) else {
            return vec![];
        };
        let identity = params.len() == generics as usize
            && (params.iter().enumerate())
                .all(|(i, p)| matches!(self.kind(*p), TyKind::Param(n) if n == i as u32));
        if !identity {
            return vec![];
        }
        let mut out = vec![];
        for k in 0..self.types.len() as u32 {
            let TyKind::Adt(e, args) = self.types.kind(TyId(k)) else {
                continue;
            };
            if *e != d || args.len() != generics as usize {
                continue;
            }
            if args.iter().all(|a| self.closed(*a, 0)) && !out.contains(args) {
                out.push(args.clone());
            }
        }
        out
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

    /// How many base classes the class `t` has (0 for other types).
    fn class_depth(&self, t: TyId) -> usize {
        let mut n = 0;
        let mut cur = t;
        while let TyKind::Adt(d, _) = self.kind(cur) {
            if !self.is_class(cur) {
                break;
            }
            match self.adt_def(d).base {
                Some(b) if n < 64 => {
                    // The base's own type arguments do not change its depth.
                    cur = b;
                    n += 1;
                }
                _ => break,
            }
        }
        n
    }
}

impl FnLower<'_, '_> {
    /// The `any` at `p` (of type `from`) as the interface type `ty` through an implementation
    /// of its interface (module docs): on a match the value goes into `out` and on to `done`;
    /// otherwise the code goes on in the current block.
    pub(super) fn any_to_iface(
        &mut self,
        p: &Place,
        from: TyId,
        ty: TyId,
        out: &Place,
        done: vir::BlockId,
    ) {
        let TyKind::Dyn(iface, to) = self.cx.kind(ty) else {
            return;
        };
        if self.cx.is_dynamic(ty) || self.cx.is_narrow_dynamic(ty) {
            return;
        }
        let any_imp = self.cx.hir.any_impl.unwrap_or(0);
        for s in self.cx.iface_sources(iface, &to) {
            let desc = Operand::Copy(proj(p, Proj::Field(1)));
            let own = self.vtable_addr(VtableKey::Impl(any_imp, s.self_ty));
            let hit = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc, own));
            let class = match self.cx.kind(s.self_ty) {
                TyKind::Adt(d, _) if self.cx.is_class(s.self_ty) && !self.cx.has_proxies() => {
                    Some(d)
                }
                _ => None,
            };
            let (yes, no) = (self.new_block(), self.new_block());
            match class {
                Some(class) => {
                    // An instance of a subclass: an object whose class descends from this one.
                    let test = self.new_block();
                    self.branch(hit, yes, test);
                    self.switch_to(test);
                    let tag = self.any_tag_of(Operand::Copy(proj(p, Proj::Field(1))));
                    let obj = cint(super::any::TAG_OBJECT as i128, Ty::U64);
                    let is_obj = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag, obj));
                    let inst = self.new_block();
                    self.branch(is_obj, inst, no);
                    self.switch_to(inst);
                    let sub = self.instance_test(p, from, class);
                    self.branch(sub, yes, no);
                }
                None => self.branch(hit, yes, no),
            }
            self.switch_to(yes);
            let data = Operand::Copy(proj(p, Proj::Field(0)));
            let mid = s.adapter.map_or(ty, |a| a.2);
            let table = self.vtable_addr(VtableKey::Impl(s.imp, s.self_ty));
            let v = self.build_agg(mid, vec![data, table]);
            let mut v = self.take_owned(v);
            if let Some((aty, k, _)) = s.adapter {
                let wrapped = self.build_agg(aty, vec![v]);
                let wrapped = self.take_owned(wrapped);
                let r = self.value_to_dyn(wrapped, aty, ty, k);
                v = self.take_owned(r);
            }
            self.assign(out.clone(), Rvalue::Use(v));
            self.goto(done);
            self.switch_to(no);
        }
    }
}
