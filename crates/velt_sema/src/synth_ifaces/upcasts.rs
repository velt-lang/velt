//! Conversions between interface values (`Upcast` of `Dyn(A)` to `Dyn(B)`) and the
//! implementations of synthesized interfaces for classes and structs (`crate::synth_ifaces`).

use std::collections::HashMap;

use super::{Form, Member};
use crate::collect::forwarders::{synth_method, Host, Target};
use crate::ctx::Ctx;
use crate::defs::ParamSig;
use crate::hir::{DefId, ImplDef, PassMode, TyId, TyKind};
use crate::iface_writes::SetterSrc;

impl Ctx<'_> {
    /// May an interface value of `have<from>` be used as a `want<to>`: does it have every
    /// member of `want`, with the same type (module docs)? Records the conversion.
    pub(crate) fn iface_upcast(
        &mut self,
        (have, from): (DefId, &[TyId]),
        (want, to): (DefId, &[TyId]),
    ) -> bool {
        if have == want {
            return false;
        }
        // A conversion back (`A` to `B` and `B` to `A`) makes each table hold the other
        // (lowering fills in a table's targets once the table is registered).
        let known = self.synth_ifaces.upcasts.contains(&(have, want));
        let ours = self.iface_members(have, from);
        let theirs = self.iface_members(want, to);
        let fits = theirs.iter().all(|(n, m)| {
            ours.iter().any(|(o, x)| {
                o == n
                    && match (x, m) {
                        (Member::Field(a, ra), Member::Field(b, rb)) => a == b && (*rb || !ra),
                        // A getter fills a field's slot, as TypeScript allows; writing the
                        // field takes a setter `set k` (`upcast_setter`), which must take the
                        // field's type.
                        (Member::Method(ps, a, true, 0), Member::Field(b, rb)) => {
                            ps.is_empty() && a == b && (*rb || setter_param(&ours, n, *b).is_none())
                        }
                        // A field holding a function fills a method's slot (`fn_field_method`).
                        (Member::Field(f, _), Member::Method(ps, r, false, 0)) => {
                            self.fn_fills(*f, ps, *r)
                        }
                        (a, b) => a == b,
                    }
            })
        });
        if fits && !known {
            self.synth_ifaces.upcasts.push((have, want));
        }
        fits
    }

    /// An implementation of synthesized interface `iface` for `ty`, a class or struct (generic
    /// or not) that implements each of its interface parts, itself or through a base class, and
    /// has its object part's fields (or, for a `Pick`, implements the picked interface): its
    /// slots are those implementations' methods and the fields' getters (and setters), so the
    /// value is the same object. Made once per definition, over its type parameters.
    pub(crate) fn synth_impl(&mut self, ty: TyId, iface: DefId) -> Option<(u32, Vec<TyId>, TyId)> {
        let (form, _) = self.synth_ifaces.forms.get(&iface)?.clone();
        let TyKind::Adt(d, targs) = self.ty.kind(ty).clone() else {
            return None;
        };
        // Only a nominal type: an object literal implements the interface itself.
        let a = self.adt(d)?;
        if self.class_of(ty).is_none() && a.kind != crate::hir::AdtKind::Struct {
            return None;
        }
        let n = a.generics.len();
        if targs.len() != n {
            return None;
        }
        // Over the definition's type parameters (`Box<T>` implementing `Get<T>` and `Put<T>`),
        // else for this instance only (`Get<number> & Put<number>`).
        let own_ty = crate::collect::shapes::self_type(self, d, n);
        let mut tries = vec![(own_ty, n)];
        if own_ty != ty {
            tries.push((ty, 0));
        }
        for (t, k) in tries {
            if !self.synth_ifaces.tried.insert((t, iface)) {
                continue;
            }
            if let Some(index) = self.synth_impl_of(t, d, k, form.clone(), iface) {
                return self.try_impl(index, ty);
            }
        }
        None
    }

    /// [`Self::synth_impl`] for `own_ty`, definition `d` over its `n` type parameters: the
    /// index of the implementation made.
    fn synth_impl_of(
        &mut self,
        own_ty: TyId,
        d: DefId,
        n: usize,
        form: Form,
        iface: DefId,
    ) -> Option<u32> {
        let parts: Vec<TyId> = match &form {
            Form::Meet { ifaces, .. } => ifaces.clone(),
            Form::Pick { iface, .. } => vec![*iface],
        };
        let k = self.iface(iface)?.generics.len();
        let mut slots: Vec<Option<TyId>> = vec![None; k];
        // Per part: the implementation for `own_ty` (or a base class of it).
        let mut impls: Vec<(DefId, u32)> = vec![];
        for &p in &parts {
            let TyKind::Dyn(pd, pargs) = self.ty.kind(p).clone() else {
                return None;
            };
            let (index, iargs, imp_ty) = self.find_impl(own_ty, pd)?;
            if imp_ty != own_ty && !self.is_base_class(imp_ty, own_ty) {
                return None;
            }
            for (&pa, &ia) in pargs.iter().zip(&iargs) {
                if !self.match_ty(pa, ia, &mut slots) {
                    return None;
                }
            }
            impls.push((pd, index));
        }
        let args: Vec<TyId> = slots
            .into_iter()
            .map(|s| s.unwrap_or(self.ty.never))
            .collect();
        let (names, fields): (Vec<String>, Vec<(String, TyId)>) = {
            let i = self.iface(iface)?;
            (
                i.methods.iter().map(|m| m.name.clone()).collect(),
                i.fields.iter().map(|f| (f.name.clone(), f.ty)).collect(),
            )
        };
        let mut methods = vec![];
        for name in &names {
            methods.push(self.part_slot(&impls, name, false)?);
        }
        let own = self.adt(d)?.fields.clone();
        // `own_ty`'s arguments: a field `val: T` of `Cell<string>` stores a `string`.
        let own_args = match self.ty.kind(own_ty) {
            TyKind::Adt(_, a) => a.clone(),
            _ => vec![],
        };
        // How each field is stored: a part's implementation's, or `d`'s own field.
        let mut stored: Vec<Option<SetterSrc>> = vec![];
        for (name, fty) in fields {
            if let Some(m) = self.part_slot(&impls, &name, true) {
                methods.push(m);
                stored.push(self.part_setter(&impls, &name));
                continue;
            }
            let want = self.subst(fty, &args);
            let field = own
                .iter()
                .position(|f| f.name == name && f.private_to.is_none());
            let Some(k) = field.filter(|&k| self.subst(own[k].ty, &own_args) == want) else {
                // A `get` (and `set`) accessor of the class provides the field.
                let (g, set) = crate::collect::impls::accessor_slots(self, d, own_ty, &name, want)?;
                methods.push(g);
                // Without a `set`, writing the field throws, as in Node (a part write through
                // a class's accessor is an error, `iface_writes`).
                let set = match set {
                    Some(def) => SetterSrc::Method {
                        def,
                        d,
                        name: name.clone(),
                    },
                    None => SetterSrc::GetterOnly {
                        d,
                        self_ty: own_ty,
                        name: name.clone(),
                        ty: want,
                    },
                };
                stored.push(Some(set));
                continue;
            };
            methods.push(crate::collect::getters::field_getter(
                self, d, own_ty, k, want,
            ));
            stored.push(Some(SetterSrc::Field {
                d,
                self_ty: own_ty,
                index: k,
                ty: want,
            }));
        }
        let setters: Option<Vec<SetterSrc>> = stored.into_iter().collect();
        self.impls.push(ImplDef {
            ty: own_ty,
            generics: n as u32,
            iface,
            iface_args: args,
            methods,
        });
        let index = (self.impls.len() - 1) as u32;
        // A part written through in a way with no setters leaves this one without them too.
        match setters {
            Some(setters) => self.note_setters(index, setters),
            None => self.no_setters_after_write(index),
        }
        Some(index)
    }

    /// Is `base` a base class of class type `t`?
    fn is_base_class(&mut self, base: TyId, t: TyId) -> bool {
        let mut cur = self.base_of(t);
        for _ in 0..64 {
            match cur {
                Some(b) if b == base => return true,
                Some(b) => cur = self.base_of(b),
                None => return false,
            }
        }
        false
    }

    /// How the implementation of the first of `impls` whose interface has field
    /// `name` stores it (`crate::iface_writes`).
    fn part_setter(&self, impls: &[(DefId, u32)], name: &str) -> Option<SetterSrc> {
        for &(p, index) in impls {
            let i = self.iface(p)?;
            let Some(f) = i.fields.iter().position(|f| f.name == name) else {
                continue;
            };
            return self.setter_sources(index)?.get(f).cloned();
        }
        None
    }

    /// The method in the slot of member `name` (a field's getter when `field`) of the first of
    /// `impls` (per interface, its implementation) whose interface has it.
    fn part_slot(&self, impls: &[(DefId, u32)], name: &str, field: bool) -> Option<DefId> {
        for &(p, index) in impls {
            let i = self.iface(p)?;
            let slot = match field {
                false => i.methods.iter().position(|m| m.name == name),
                true => i
                    .fields
                    .iter()
                    .position(|f| f.name == name)
                    .map(|k| i.methods.len() + k),
            };
            if let Some(s) = slot {
                return self.impls[index as usize].methods.get(s).copied();
            }
        }
        None
    }

    /// For an interface value of `found` where one of `expected` is expected: a setter that
    /// would write a field of `expected` but takes another type, or a field holding a function
    /// that may throw where `expected` has a method.
    pub(crate) fn upcast_member_note(&mut self, expected: TyId, found: TyId) -> Option<String> {
        let (TyKind::Dyn(want, to), TyKind::Dyn(have, from)) =
            (self.ty.kind(expected).clone(), self.ty.kind(found).clone())
        else {
            return None;
        };
        let ours = self.iface_members(have, &from);
        let theirs = self.iface_members(want, &to);
        let throwing = theirs.iter().find_map(|(n, m)| match m {
            Member::Method(..) => ours.iter().find_map(|(o, x)| match x {
                Member::Field(f, _) if o == n => match self.ty.kind(*f) {
                    TyKind::FnPtr { throws, .. } if *throws != self.ty.never => Some(n.clone()),
                    _ => None,
                },
                _ => None,
            }),
            _ => None,
        });
        if let Some(name) = throwing {
            let e = self.display(expected);
            return Some(format!("`{name}` holds a function that may throw, which does not fill the method slot of `{e}` yet; write the literal where `{e}` is expected (`const v: {e} = {{ … }}`)"));
        }
        let (name, field, param) = theirs.iter().find_map(|(n, m)| match m {
            Member::Field(b, false) => setter_param(&ours, n, *b).map(|p| (n.clone(), *b, p)),
            _ => None,
        })?;
        let (e, f, p) = (
            self.display(expected),
            self.display(field),
            self.display(param),
        );
        Some(format!("the setter `set {name}` takes a `{p}`, but `{name}` is a `{f}` field of `{e}`, which writes it through that setter; give the setter a `{f}` parameter, or make the field `readonly`"))
    }

    /// After every body is checked, before errors are reported: a value converted to an
    /// interface written through is written through its own (`Ctx::writes_through_upcasts`).
    pub(crate) fn upcast_writes(&mut self) {
        let pairs = self.synth_ifaces.upcasts.clone();
        self.writes_through_upcasts(&pairs);
    }

    /// After every body is checked: per interface `A` converted to others, the targets in
    /// table order, and per implementation of `A` the implementation of each target for the
    /// same type, made from `A`'s (each target slot is the slot of the same member in `A`).
    pub(crate) fn upcast_tables(&mut self) -> (HashMap<DefId, Vec<DefId>>, HashMap<u32, Vec<u32>>) {
        let mut targets: HashMap<DefId, Vec<DefId>> = HashMap::new();
        for &(a, b) in &self.synth_ifaces.upcasts {
            targets.entry(a).or_default().push(b);
        }
        let mut impls_of: HashMap<u32, Vec<u32>> = HashMap::new();
        // Implementations made here are visited too: a target converting further gets its own.
        let mut i = 0;
        while i < self.impls.len() {
            let a = self.impls[i].iface;
            if let Some(bs) = targets.get(&a).cloned() {
                let mut v = vec![];
                for b in bs {
                    v.push(self.upcast_impl(i as u32, b));
                }
                impls_of.insert(i as u32, v);
            }
            i += 1;
        }
        (targets, impls_of)
    }

    /// The implementation of interface `b` for the type of implementation `index` (of an
    /// interface whose values convert to `b`): an existing one, or one made of its slots.
    fn upcast_impl(&mut self, index: u32, b: DefId) -> u32 {
        let imp = self.impls[index as usize].clone();
        if let Some(j) = self
            .impls
            .iter()
            .position(|x| x.iface == b && x.ty == imp.ty && x.generics == imp.generics)
        {
            return j as u32;
        }
        let a = imp.iface;
        let (ai, bi) = (
            self.iface(a).expect("ICE: iface"),
            self.iface(b).expect("ICE: iface"),
        );
        let slot_of = |name: &str, field: bool| -> usize {
            let s = match field {
                false => ai.methods.iter().position(|m| m.name == name),
                // A field's slot is filled by our field's getter, or by a getter of that name.
                true => ai
                    .fields
                    .iter()
                    .position(|f| f.name == name)
                    .map(|k| ai.methods.len() + k)
                    .or_else(|| {
                        ai.methods
                            .iter()
                            .position(|m| m.is_getter && m.name == name)
                    }),
            };
            s.expect("ICE: an upcast target member the source lacks")
        };
        // A method of `b` is filled by our method, or by our field holding a function.
        let mut slots: Vec<Result<usize, (usize, usize)>> = (bi.methods.iter().enumerate())
            .map(
                |(j, m)| match ai.methods.iter().position(|x| x.name == m.name) {
                    Some(s) => Ok(s),
                    None => Err((j, slot_of(&m.name, true))),
                },
            )
            .collect();
        slots.extend(bi.fields.iter().map(|f| Ok(slot_of(&f.name, true))));
        // `b`'s arguments: from the interface types the conversion was checked with, the
        // members agree, so `a`'s implementation arguments give `b`'s by matching its fields
        // and methods; interfaces made here and parents name them directly.
        let iface_args = self.upcast_args(a, &imp.iface_args, b);
        let at = |s: usize| *imp.methods.get(s).expect("ICE: upcast slot out of range");
        let mut methods: Vec<DefId> = vec![];
        for s in slots {
            methods.push(match s {
                Ok(s) => at(s),
                Err((j, g)) => self.fn_field_method(&imp, b, j, at(g)),
            });
        }
        if self.iface_written(b) {
            for k in self.setter_fields(b) {
                methods.push(self.upcast_setter(&imp, b, k, &iface_args));
            }
        }
        self.impls.push(ImplDef {
            ty: imp.ty,
            generics: imp.generics,
            iface: b,
            iface_args,
            methods,
        });
        (self.impls.len() - 1) as u32
    }

    /// Does a field of function type `f` fill the slot of a method taking `ps` and returning
    /// `r`: the same parameters and result, no rest parameter, and nothing thrown (a value
    /// read from the field is called, `fn_field_method`)?
    fn fn_fills(&self, f: TyId, ps: &[TyId], r: TyId) -> bool {
        match self.ty.kind(f) {
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            } => !rest && *throws == self.ty.never && params == ps && *ret == r,
            _ => false,
        }
    }

    /// The method in the slot of method `j` of interface `b` for the type of implementation
    /// `imp`, where our field's getter `getter` gives a function: one calling that function.
    fn fn_field_method(&mut self, imp: &ImplDef, b: DefId, j: usize, getter: DefId) -> DefId {
        let fn_ty = self.fn_info(getter).ret;
        let TyKind::FnPtr {
            params: tys, ret, ..
        } = self.ty.kind(fn_ty).clone()
        else {
            panic!("ICE: a field filling a method slot holds no function");
        };
        let m = self.iface(b).expect("ICE: iface").methods[j].clone();
        let params: Vec<ParamSig> = tys
            .iter()
            .zip(&m.params)
            .map(|(&ty, p)| ParamSig {
                ty,
                mode: if self.is_copy(ty) {
                    PassMode::Copy
                } else {
                    PassMode::Borrow
                },
                default: None,
                ..p.clone()
            })
            .collect();
        let TyKind::Adt(d, _) = *self.ty.kind(imp.ty) else {
            panic!("ICE: an upcast method for a value that is not an object");
        };
        let host = Host::adt(self, d);
        let targs = (0..imp.generics).map(|i| self.ty.param(i)).collect();
        let name = format!(
            "{}.<fn {}>",
            self.adt(d).map_or("", |a| &a.qual_name),
            m.name
        );
        let target = Target::Getter {
            def: getter,
            targs,
            fn_ty,
        };
        synth_method(self, &host, imp.ty, name, params, ret, false, target)
    }

    /// The setter slot of field `k` of interface `b<args>` (written through) for the type of
    /// implementation `imp` of an interface converting to it: our field's setter slot, else
    /// the setter `set k` our getter `k` has. A getter without one gives a slot that is never
    /// called: a `readonly` field, or one whose write was reported (`Ctx::writes_through_upcasts`).
    fn upcast_setter(&mut self, imp: &ImplDef, b: DefId, k: usize, args: &[TyId]) -> DefId {
        let ai = self.iface(imp.iface).expect("ICE: iface");
        let f = self.iface(b).expect("ICE: iface").fields[k].clone();
        let n = ai.methods.len() + ai.fields.len();
        let slot = match ai.fields.iter().position(|g| g.name == f.name) {
            Some(g) => Some(n + g),
            None => {
                let set = format!("set {}", f.name);
                ai.methods.iter().position(|m| m.name == set)
            }
        };
        if let Some(def) = slot.and_then(|s| imp.methods.get(s)) {
            return *def;
        }
        let TyKind::Adt(d, _) = *self.ty.kind(imp.ty) else {
            panic!("ICE: an upcast setter for a value that is not an object");
        };
        let mut targs = args.to_vec();
        targs.push(imp.ty);
        let ty = self.subst(f.ty, &targs);
        crate::collect::getters::unused_setter(self, d, imp.ty, &f.name, ty)
    }

    /// The arguments of `b` for a value of `a<args>`: through `a`'s parents, else by matching
    /// the members' types.
    fn upcast_args(&mut self, a: DefId, args: &[TyId], b: DefId) -> Vec<TyId> {
        let parents = self.iface(a).map(|i| i.parents.clone()).unwrap_or_default();
        if let Some(p) = parents.iter().find(|p| p.iface == b) {
            return p.args.iter().map(|&t| self.subst(t, args)).collect();
        }
        let n = self.iface(b).map_or(0, |i| i.generics.len());
        let generic: Vec<TyId> = (0..n as u32).map(|i| self.ty.param(i)).collect();
        let ours = self.iface_members(a, args);
        let theirs = self.iface_members(b, &generic);
        let mut slots: Vec<Option<TyId>> = vec![None; n];
        for (name, m) in &theirs {
            let Some((_, x)) = ours.iter().find(|(o, _)| o == name) else {
                continue;
            };
            match (m, x) {
                (Member::Field(p, _), Member::Field(t, _) | Member::Method(_, t, true, 0)) => {
                    self.match_ty(*p, *t, &mut slots);
                }
                (Member::Method(ps, r, ..), Member::Field(f, _)) => {
                    if let TyKind::FnPtr { params, ret, .. } = self.ty.kind(*f).clone() {
                        for (p, t) in ps.iter().zip(&params) {
                            self.match_ty(*p, *t, &mut slots);
                        }
                        self.match_ty(*r, ret, &mut slots);
                    }
                }
                (Member::Method(ps, r, ..), Member::Method(ts, s, ..)) => {
                    for (p, t) in ps.iter().zip(ts) {
                        self.match_ty(*p, *t, &mut slots);
                    }
                    self.match_ty(*r, *s, &mut slots);
                }
                _ => {}
            }
        }
        slots
            .into_iter()
            .map(|s| s.unwrap_or(self.ty.never))
            .collect()
    }
}

/// The parameter type of setter `set name` of `ours` when it is not `ty` (`None`: it takes
/// `ty`, or there is no such setter: a write is then reported where it is written).
fn setter_param(ours: &[(String, Member)], name: &str, ty: TyId) -> Option<TyId> {
    let set = format!("set {name}");
    ours.iter().find_map(|(o, m)| match m {
        Member::Method(ps, ..) if *o == set => match ps.as_slice() {
            [p] if *p == ty => None,
            [p, ..] => Some(*p),
            [] => None,
        },
        _ => None,
    })
}
