//! Writes of an interface's method member through an interface value (#379, step B of
//! `docs/internals/design/casts-across-layouts.md`): `ctx.update = () => …`, where the interface
//! declares `update(): void`. TypeScript allows it (a method signature is a writable property),
//! and Node replaces the object's property, which later calls see.
//!
//! An interface the program writes a method through gets one more setter slot per method, after
//! its field setters (`crate::iface_writes`): slot `methods + 2 × fields + j` for method `j`,
//! named `<set m()>`. An implementation holding the method as a field of the method's function
//! type (an object literal `{ update: () => {} }`, through `unknown` or written as the interface)
//! fills it with a write of that field; its method slot calls the field, so later calls see the
//! new function. An implementation that can't store it (a class's method, an object literal's
//! method using `this`, a field of another function type) makes the write a compile error naming
//! the type; the other methods' slots of such an implementation are empty setters.
//!
//! Like field writes, nothing changes for an interface never written through: its tables and
//! code stay as they are.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{DefId, TyId};

/// How an implementation stores method `j` of its interface: in field `index` of type `ty`
/// (the method's function type) of its object type `d` (self type `self_ty`).
#[derive(Clone, Copy)]
pub(crate) struct MethodSrc {
    pub d: DefId,
    pub self_ty: TyId,
    pub index: usize,
    pub ty: TyId,
}

/// The interfaces whose methods are written, and how implementations store them.
#[derive(Clone, Default)]
pub(crate) struct MethodWrites {
    /// Interface -> where a method of it is first written.
    written: FxHashMap<DefId, Span>,
    /// (interface, method) pairs written, each with where first.
    methods: FxHashMap<(DefId, usize), Span>,
    /// Per implementation (`Ctx::impls` index): how it stores each method, `None` where it can't.
    sources: FxHashMap<u32, Vec<Option<MethodSrc>>>,
    /// Implementations given their method setters.
    done: FxHashSet<u32>,
    /// (implementation, method) pairs reported.
    reported: FxHashSet<(u32, usize)>,
    /// Adapters (an `I<A>` as an `I<B>`, `body/expr/iface_adapt.rs`): their setters write the
    /// wrapped value's.
    adapters: FxHashMap<u32, crate::body::expr::iface_adapt_slots::AdapterParts>,
}

impl Ctx<'_> {
    /// The function type of method `j` of `iface` at the interface arguments `args`: what its
    /// setter takes (`(params) => ret`, throwing what its `throws` clause allows, else nothing).
    pub(crate) fn method_fn_ty(&mut self, iface: DefId, args: &[TyId], j: usize) -> Option<TyId> {
        let m = self.iface(iface)?.methods.get(j)?.clone();
        let params: Vec<TyId> = m.params.iter().map(|p| self.subst(p.ty, args)).collect();
        let ret = self.subst(m.ret, args);
        let throws = match m.throws.and_then(|t| t.ty) {
            Some(t) => self.subst(t, args),
            None => self.ty.never,
        };
        Some(self.ty.intern(crate::hir::TyKind::FnPtr {
            params,
            ret,
            throws,
            rest: false,
        }))
    }

    /// Implementation `index` is an adapter of parts `p`: its method setters write the wrapped
    /// value's.
    pub(crate) fn note_method_adapter(
        &mut self,
        index: u32,
        p: crate::body::expr::iface_adapt_slots::AdapterParts,
    ) {
        self.method_writes.adapters.insert(index, p);
    }

    /// Implementation `index` stores its interface's methods as `srcs` (in method order).
    pub(crate) fn note_method_sources(&mut self, index: u32, srcs: Vec<Option<MethodSrc>>) {
        self.method_writes.sources.insert(index, srcs);
    }

    /// The setter slot of method `j` of `iface`.
    pub(crate) fn method_setter_slot(&self, iface: DefId, j: usize) -> Option<u32> {
        let i = self.iface(iface)?;
        (j < i.methods.len()).then_some((i.methods.len() + 2 * i.fields.len() + j) as u32)
    }

    /// Is a method of `iface` written through an interface value?
    pub(crate) fn methods_written(&self, iface: DefId) -> bool {
        self.method_writes.written.contains_key(&iface)
    }

    /// Method `j` of `iface` is written through an interface value at `span`: its
    /// implementations get their method setters. Reports those that can't store it (once each),
    /// and returns false then.
    pub(crate) fn mark_method_written(&mut self, iface: DefId, j: usize, span: Span) -> bool {
        // The field setters come first in the table.
        if !self.mark_iface_written(iface, span) {
            return false;
        }
        self.method_writes.written.entry(iface).or_insert(span);
        self.method_writes.methods.entry((iface, j)).or_insert(span);
        let impls: Vec<u32> = (0..self.impls.len() as u32)
            .filter(|&k| self.impls[k as usize].iface == iface)
            .collect();
        let mut ok = true;
        for k in impls {
            ok &= self.method_setters_of(k);
        }
        ok
    }

    /// Give implementation `k`, of an interface whose methods are written, its method setters
    /// (once), and report each written method it can't store. Returns whether it stores all.
    fn method_setters_of(&mut self, k: u32) -> bool {
        let iface = self.impls[k as usize].iface;
        let Some(info) = self.iface(iface) else {
            return true;
        };
        let (methods, nfields) = (info.methods.clone(), info.fields.len());
        if let Some(p) = self.method_writes.adapters.get(&k).cloned() {
            return self.adapter_method_setters(k, &p, methods.len(), nfields);
        }
        let srcs = (self.method_writes.sources.get(&k).cloned())
            .unwrap_or_else(|| vec![None; methods.len()]);
        let mut ok = true;
        let written: Vec<(usize, Span)> = (self.method_writes.methods.iter())
            .filter(|((i, _), _)| *i == iface)
            .map(|((_, j), s)| (*j, *s))
            .collect();
        for (j, span) in written {
            if srcs.get(j).copied().flatten().is_none() {
                ok = false;
                if self.method_writes.reported.insert((k, j)) {
                    self.cannot_store_method(k, &methods[j].name, span);
                }
            }
        }
        let have = self.impls[k as usize].methods.len();
        if have == 2 * (methods.len() + nfields) {
            // Made with them (`upcast_method_setters`).
            self.method_writes.done.insert(k);
        }
        if have != methods.len() + 2 * nfields || !self.method_writes.done.insert(k) {
            return ok;
        }
        let ty = self.impls[k as usize].ty;
        let d = match self.ty.kind(ty) {
            crate::hir::TyKind::Adt(d, _) => Some(*d),
            _ => None,
        };
        let mut defs = vec![];
        for (j, m) in methods.iter().enumerate() {
            let name = format!("{}()", m.name);
            let def = match (srcs.get(j).copied().flatten(), d) {
                (Some(s), _) => crate::collect::getters::field_setter_named(
                    self, s.d, s.self_ty, s.index, s.ty, &name,
                ),
                (None, Some(d)) => {
                    let args = self.impls[k as usize].iface_args.clone();
                    let Some(fty) = self.method_fn_ty(iface, &args, j) else {
                        continue;
                    };
                    crate::collect::getters::unused_setter(self, d, ty, &name, fty)
                }
                (None, None) => continue,
            };
            defs.push(def);
        }
        if defs.len() == methods.len() {
            self.impls[k as usize].methods.extend(defs);
            self.groups = None;
        }
        ok
    }

    /// [`Self::method_setters_of`] for adapter `k` (parts `p`): each setter writes the wrapped
    /// value's (`adapter_method_setter`); a written method whose function can't convert is
    /// reported.
    fn adapter_method_setters(
        &mut self,
        k: u32,
        p: &crate::body::expr::iface_adapt_slots::AdapterParts,
        nmethods: usize,
        nfields: usize,
    ) -> bool {
        let have = self.impls[k as usize].methods.len();
        if have != nmethods + 2 * nfields || !self.method_writes.done.insert(k) {
            return true;
        }
        let iface = self.impls[k as usize].iface;
        let names: Vec<String> = (self.iface(iface).into_iter())
            .flat_map(|i| i.methods.iter().map(|m| m.name.clone()))
            .collect();
        let mut ok = true;
        let mut defs = vec![];
        for (j, name) in names.iter().enumerate() {
            match crate::body::expr::iface_adapt_slots::adapter_method_setter(self, p, j) {
                Some(def) => defs.push(def),
                None => {
                    if let Some(&span) = self.method_writes.methods.get(&(iface, j)) {
                        ok = false;
                        if self.method_writes.reported.insert((k, j)) {
                            self.cannot_store_method(k, name, span);
                        }
                    }
                    let args = self.impls[k as usize].iface_args.clone();
                    let Some(fty) = self.method_fn_ty(iface, &args, j) else {
                        return ok;
                    };
                    let setter = crate::collect::getters::unused_setter;
                    defs.push(setter(self, p.def, p.self_ty, &format!("{name}()"), fty));
                }
            }
        }
        self.impls[k as usize].methods.extend(defs);
        self.groups = None;
        ok
    }

    /// The method setters of an implementation of `b` made of implementation `index` (of an
    /// interface whose values convert to `b`, `synth_ifaces/upcasts.rs`), when `b`'s methods are
    /// written, and how it stores each of `b`'s methods: as `index` stores the method of that
    /// name, which its setter (or one made from its field) writes.
    pub(crate) fn upcast_method_setters(
        &mut self,
        index: u32,
        b: DefId,
        args: &[TyId],
    ) -> (Vec<DefId>, Vec<Option<MethodSrc>>) {
        let imp = self.impls[index as usize].clone();
        let (Some(ai), Some(bi)) = (self.iface(imp.iface), self.iface(b)) else {
            return (vec![], vec![]);
        };
        let (a_methods, b_methods) = (ai.methods.clone(), bi.methods.clone());
        let srcs_a = self
            .method_writes
            .sources
            .get(&index)
            .cloned()
            .unwrap_or_default();
        let srcs: Vec<Option<MethodSrc>> = (b_methods.iter())
            .map(|m| {
                let j = a_methods.iter().position(|x| x.name == m.name)?;
                srcs_a.get(j).copied().flatten()
            })
            .collect();
        if !self.methods_written(b) {
            return (vec![], srcs);
        }
        let d = match self.ty.kind(imp.ty) {
            crate::hir::TyKind::Adt(d, _) => *d,
            _ => return (vec![], srcs),
        };
        let mut defs = vec![];
        for (j, m) in b_methods.iter().enumerate() {
            let name = format!("{}()", m.name);
            // `a`'s own setter of the method, when `a` is written through too.
            let aj = a_methods.iter().position(|x| x.name == m.name);
            let own = (aj.filter(|_| self.methods_written(imp.iface)))
                .and_then(|aj| self.method_setter_slot(imp.iface, aj))
                .and_then(|s| imp.methods.get(s as usize).copied());
            let def = match (own, srcs[j]) {
                (Some(def), _) => def,
                (None, Some(s)) => crate::collect::getters::field_setter_named(
                    self, s.d, s.self_ty, s.index, s.ty, &name,
                ),
                (None, None) => {
                    let Some(fty) = self.method_fn_ty(b, args, j) else {
                        return (vec![], srcs);
                    };
                    crate::collect::getters::unused_setter(self, d, imp.ty, &name, fty)
                }
            };
            defs.push(def);
        }
        (defs, srcs)
    }

    /// Give every implementation of an interface whose methods are written its method setters:
    /// those made after the first write (once every body is checked, before the passes that read
    /// the tables).
    pub(crate) fn finish_method_setters(&mut self) {
        if self.method_writes.written.is_empty() {
            return;
        }
        for k in 0..self.impls.len() as u32 {
            let iface = self.impls[k as usize].iface;
            if self.methods_written(iface) {
                // Field setters first, as `mark_iface_written` gives the earlier ones.
                self.add_setters_pub(k);
                self.method_setters_of(k);
            }
        }
    }

    /// Implementation `k` can't store method `name` written at `span`.
    fn cannot_store_method(&mut self, k: u32, name: &str, span: Span) {
        let ty = self.impls[k as usize].ty;
        let iface = self.impls[k as usize].iface;
        let shown = self.display(ty);
        let iname = self.iface(iface).map_or(String::new(), |i| i.name.clone());
        let literal = match self.ty.kind(ty) {
            crate::hir::TyKind::Adt(d, _) => self.literal_structs.contains(d),
            _ => false,
        };
        let what = if literal && self.class_of(ty).is_some() {
            format!(
                "an object literal written as `{iname}` defines `{name}` as a method using `this`"
            )
        } else if self.class_of(ty).is_some() {
            format!("class `{shown}` implements `{iname}` with a method `{name}`, which its objects share through the class")
        } else {
            format!("`{shown}` implements `{iname}` without a field of `{name}`'s function type to replace")
        };
        self.error(
            Diagnostic::error(format!("cannot assign to method `{name}` of `{iname}`"), span)
                .with_note(format!("{what}; a write through the interface could not reach it"))
                .with_note(format!("make `{name}` a field of function type (`{name}: () => …`) where the object is made")),
        );
    }
}
