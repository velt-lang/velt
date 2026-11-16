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
    /// Implementations whose field-setter part is placeholders ([`Ctx::field_setter_part`]).
    placeholders: FxHashSet<u32>,
    /// Adapters (an `I<A>` as an `I<B>`, `body/expr/iface_adapt.rs`): their setters write the
    /// wrapped value's.
    adapters: FxHashMap<u32, crate::body::expr::iface_adapt_slots::AdapterParts>,
    /// (interface, method) -> its function-type site and that site's module
    /// ([`Ctx::method_site`]).
    sites: FxHashMap<(DefId, usize), (Span, usize)>,
    /// (object type, field) -> the site of the function stored there under a written method's
    /// name (`body/expr/object.rs`), the first literal of that shape.
    field_sites: FxHashMap<(DefId, usize), Span>,
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

    /// The function-type site of method `j` of `iface`, written as a property at `at` in module
    /// `module`, when it has no `throws` clause: its declaration, or (an interface of a module
    /// without sites, std's) the first write, which is the same in every check. `None` with a
    /// clause.
    pub(crate) fn method_site(
        &mut self,
        iface: DefId,
        j: usize,
        at: Span,
        module: usize,
    ) -> Option<(Span, usize)> {
        if let Some(&s) = self.method_writes.sites.get(&(iface, j)) {
            return Some(s);
        }
        let info = self.iface(iface)?;
        let m = info.methods.get(j)?;
        if m.throws.is_some() {
            return None;
        }
        let (decl, home) = (m.span, info.module);
        let user = self.scopes.get(home).is_some_and(|s| !s.is_std);
        let site = match user {
            true => (decl, home),
            false => (at, module),
        };
        self.method_writes.sites.insert((iface, j), site);
        Some(site)
    }

    /// The function type of method `j` of `iface` (at `args`) written as a property at `at` in
    /// module `module`: what its setter takes. Without a `throws` clause it is the method's
    /// function-type site ([`Self::method_site`]), whose errors are inferred from what the
    /// program assigns to it (`throws/fn_types.rs` `method_site_ty`).
    pub(crate) fn method_site_fn_ty(
        &mut self,
        iface: DefId,
        args: &[TyId],
        j: usize,
        (at, module): (Span, usize),
    ) -> Option<TyId> {
        let Some((span, home)) = self.method_site(iface, j, at, module) else {
            return self.method_fn_ty(iface, args, j);
        };
        let m = self.iface(iface)?.methods.get(j)?.clone();
        let params: Vec<TyId> = m.params.iter().map(|p| self.subst(p.ty, args)).collect();
        let ret = self.subst(m.ret, args);
        match self.method_site_ty(params, ret, span, home) {
            Some(t) => Some(t),
            None => self.method_fn_ty(iface, args, j),
        }
    }

    /// Field `i` of object type `d` stores a function under a written method's name, and is the
    /// site at `at` (`body/expr/object.rs`).
    pub(crate) fn note_method_field_site(&mut self, d: DefId, i: usize, at: Span) {
        self.method_writes.field_sites.entry((d, i)).or_insert(at);
    }

    /// Link the site of method `j` of `iface` with the sites of the fields its implementations
    /// store it in, both ways: they throw the same errors (`throws/fn_flows.rs` `site_flow`).
    fn link_method_sites(&mut self, iface: DefId, j: usize) {
        let Some(m) = self.method_site_span(iface, j) else {
            return;
        };
        let impls: Vec<u32> = (0..self.impls.len() as u32)
            .filter(|&k| self.impls[k as usize].iface == iface)
            .collect();
        for k in impls {
            let src =
                (self.method_writes.sources.get(&k)).and_then(|s| s.get(j).copied().flatten());
            let Some(s) = src else {
                continue;
            };
            if let Some(&f) = self.method_writes.field_sites.get(&(s.d, s.index)) {
                self.site_flow(m, f);
                self.site_flow(f, m);
            }
        }
    }

    /// The site span of method `j` of `iface` ([`Self::method_site`]), once written.
    pub(crate) fn method_site_span(&self, iface: DefId, j: usize) -> Option<Span> {
        self.method_writes.sites.get(&(iface, j)).map(|s| s.0)
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

    /// Can a field of function type `field` store method `j` of `iface` (at `args`)? The same
    /// parameters and result; the error types are compared by identity only, never unified (a
    /// field's is inferred from what is stored into it, `crate::fn_sites`).
    pub(crate) fn stores_method(
        &mut self,
        iface: DefId,
        args: &[TyId],
        j: usize,
        field: TyId,
    ) -> bool {
        let Some(m) = self.method_fn_ty(iface, args, j) else {
            return false;
        };
        let parts = |t: TyId, cx: &Self| match cx.ty.kind(t) {
            crate::hir::TyKind::FnPtr {
                params, ret, rest, ..
            } => Some((params.clone(), *ret, *rest)),
            _ => None,
        };
        match (parts(m, self), parts(field, self)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
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
        if let Some(m) = self.iface(iface).and_then(|i| i.methods.get(j)) {
            if m.throws.is_none() {
                let name = m.name.clone();
                self.note_written_method(&name);
            }
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
        self.link_method_sites(iface, j);
        ok
    }

    /// Is implementation `k`'s field-setter part placeholders (below)?
    pub(crate) fn method_writes_placeholder(&self, k: u32) -> bool {
        self.method_writes.placeholders.contains(&k)
    }

    /// Implementation `k`, made with method setters by [`Self::upcast_method_setters`]: its
    /// field-setter part is placeholders when no field of its interface is written.
    pub(crate) fn note_placeholders(&mut self, k: u32, iface: DefId) {
        if self.methods_written(iface) && !self.iface_written(iface) {
            self.method_writes.placeholders.insert(k);
        }
    }

    /// Implementation `k`'s placeholders were replaced by its field setters.
    pub(crate) fn method_writes_filled(&mut self, k: u32) {
        self.method_writes.placeholders.remove(&k);
    }

    /// The table's field-setter part for implementation `k`, when a method of its interface is
    /// written and no field is: placeholders, never called, so field writes stay off (and their
    /// checks unrun) until a field is written through the interface, which then replaces them
    /// (`iface_writes.rs` `add_setters`). An interface whose fields are written gets its real
    /// field setters instead.
    fn field_setter_part(&mut self, k: u32) {
        let iface = self.impls[k as usize].iface;
        if self.iface_written(iface) {
            self.add_setters_pub(k);
            return;
        }
        let Some(info) = self.iface(iface) else {
            return;
        };
        let (base, fields) = (info.methods.len() + info.fields.len(), info.fields.clone());
        let imp = self.impls[k as usize].clone();
        if imp.methods.len() != base {
            return;
        }
        let crate::hir::TyKind::Adt(d, _) = *self.ty.kind(imp.ty) else {
            return;
        };
        let mut targs = imp.iface_args.clone();
        targs.push(imp.ty);
        let mut defs = vec![];
        for f in &fields {
            let ty = self.subst(f.ty, &targs);
            let name = format!("{} unset", f.name);
            defs.push(crate::collect::getters::unused_setter(
                self, d, imp.ty, &name, ty,
            ));
        }
        self.impls[k as usize].methods.extend(defs);
        self.method_writes.placeholders.insert(k);
    }

    /// Give implementation `k`, of an interface whose methods are written, its method setters
    /// (once), and report each written method it can't store. Returns whether it stores all.
    fn method_setters_of(&mut self, k: u32) -> bool {
        let iface = self.impls[k as usize].iface;
        let Some(info) = self.iface(iface) else {
            return true;
        };
        let (methods, nfields) = (info.methods.clone(), info.fields.len());
        self.field_setter_part(k);
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
        } else if std::env::var_os("VELT_DEBUG_MW").is_some() {
            let shown = self.display(ty);
            eprintln!(
                "method setters skipped for impl {k} of {shown}: {} of {}",
                defs.len(),
                methods.len()
            );
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
        // No field of `b` written: placeholders where its field setters go (the caller marks
        // the new implementation, [`Self::note_placeholders`]).
        if !self.iface_written(b) {
            let fields = self.iface(b).map(|i| i.fields.clone()).unwrap_or_default();
            let mut targs = args.to_vec();
            targs.push(imp.ty);
            for f in &fields {
                let ty = self.subst(f.ty, &targs);
                let name = format!("{} unset", f.name);
                defs.push(crate::collect::getters::unused_setter(
                    self, d, imp.ty, &name, ty,
                ));
            }
        }
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
                self.method_setters_of(k);
            }
        }
        // Implementations made since the writes store the methods too.
        let written: Vec<(DefId, usize)> = self.method_writes.methods.keys().copied().collect();
        for (iface, j) in written {
            self.link_method_sites(iface, j);
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
