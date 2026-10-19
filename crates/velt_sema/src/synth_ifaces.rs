//! Interfaces sema makes for types written with interfaces that have methods (#350), as
//! TypeScript has them:
//!
//! - an intersection with such an interface, `I & { errors: unknown }` or `A & B`, is an
//!   interface whose members are every part's: the methods and fields of the interfaces and the
//!   fields of the object types. A field in several parts gets the intersection of their types.
//!   It extends each interface part, so its values convert to them;
//! - `Pick<I, K>` and `Omit<I, K>` of such an interface is an interface with the members named
//!   (or not named) by `K`.
//!
//! Each is made once per form, over the type parameters its parts mention (its arguments), and
//! its members are filled once interfaces are flattened (`collect::iface_extends`), so a part
//! declared later in the program is complete. An object literal implements it like any
//! interface; a class implementing every interface part (and having the object parts' fields)
//! implements it too ([`Ctx::synth_impl`]), the same object.
//!
//! Interface values convert to an interface whose every member they have with the same type
//! (a part of an intersection, a `Pick` of it, an interface it extends): [`Ctx::iface_upcast`]
//! records the pair, and the value is an `Upcast` keeping the same object
//! (hir_encodings.md "Interface upcasts").

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::anon::ShapeField;
use crate::ctx::Ctx;
use crate::defs::{Bound, DefInfo, FieldInfo, Generics, IfaceInfo};
use crate::hir::{DefId, ImplDef, Nothing, TyId, TyKind};

/// What a synthesized interface is made of, over its own type parameters.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Form {
    /// `I1 & I2 & { … }`: the interface parts and the merged object part.
    Meet {
        ifaces: Vec<TyId>,
        object: Option<TyId>,
    },
    /// `Pick<I, K>` (`omit` false) or `Omit<I, K>`.
    Pick {
        iface: TyId,
        keys: Vec<String>,
        omit: bool,
    },
}

/// The interfaces made here.
#[derive(Default)]
pub(crate) struct SynthIfaces {
    by_form: HashMap<Form, DefId>,
    forms: HashMap<DefId, (Form, Span)>,
    /// Made before interfaces were flattened: filled by [`Ctx::fill_synth_ifaces`].
    pending: Vec<DefId>,
    flattened: bool,
    /// The interface conversions the program makes, `(from, to)`, in order.
    pub upcasts: Vec<(DefId, DefId)>,
    /// Where the intersection being made is written (for its errors).
    pub span: Span,
    /// (type, interface) pairs [`Ctx::synth_impl`] tried.
    tried: HashSet<(TyId, DefId)>,
}

impl Ctx<'_> {
    /// Is `t` an interface value of an interface with methods (not `any`, `unknown`, `object`
    /// or `Function`)?
    pub(crate) fn is_method_iface(&self, t: TyId) -> bool {
        matches!(self.ty.kind(t), TyKind::Dyn(..)) && self.dynamic(t).is_none()
    }

    /// `a & b` where one of them is an interface with methods, or an interface made here from
    /// an intersection: the interface with both's members (module docs). `None` when the
    /// other is not an interface or an object type.
    pub(crate) fn meet_iface(
        &mut self,
        a: TyId,
        b: TyId,
        module: usize,
        span: Span,
    ) -> Option<Result<TyId, String>> {
        if !(self.is_method_iface(a) || self.is_method_iface(b)) {
            return None;
        }
        let mut ifaces: Vec<TyId> = vec![];
        let mut objects: Vec<TyId> = vec![];
        for t in [a, b] {
            if let Some((is, o)) = self.meet_parts(t) {
                for i in is {
                    if !ifaces.contains(&i) {
                        ifaces.push(i);
                    }
                }
                objects.extend(o);
            } else if self.is_method_iface(t) {
                if !ifaces.contains(&t) {
                    ifaces.push(t);
                }
            } else if self.is_object_type(t) {
                objects.push(t);
            } else {
                return None;
            }
        }
        let mut object = None;
        for o in objects {
            object = Some(match object {
                None => o,
                Some(acc) => match self.meet(acc, o, module, 1) {
                    Ok(m) => m,
                    Err(e) => return Some(Err(e)),
                },
            });
        }
        // The fields of the object part that an interface part has too must meet.
        if let Some(o) = object {
            let fields = self.public_fields(o);
            for &i in &ifaces {
                for (name, ty) in self.iface_field_types(i) {
                    let Some(f) = fields.iter().find(|f| f.name == name) else {
                        continue;
                    };
                    if self.meet(ty, f.ty, module, 1).is_err() {
                        let (x, y) = (self.display(ty), self.display(f.ty));
                        return Some(Err(format!(
                            "field `{name}` is `{x}` in one part and `{y}` in another"
                        )));
                    }
                }
            }
        }
        if let Some(why) = self.method_clash(&ifaces) {
            return Some(Err(why));
        }
        if object.is_none() && ifaces.len() == 1 {
            return Some(Ok(ifaces[0]));
        }
        Some(Ok(self.synth_iface(
            Form::Meet { ifaces, object },
            module,
            span,
        )))
    }

    /// The parts of `t` when it is an intersection interface made here, with `t`'s arguments.
    pub(crate) fn meet_parts(&mut self, t: TyId) -> Option<(Vec<TyId>, Option<TyId>)> {
        let TyKind::Dyn(d, args) = self.ty.kind(t).clone() else {
            return None;
        };
        let (Form::Meet { ifaces, object }, _) = self.synth_ifaces.forms.get(&d)?.clone() else {
            return None;
        };
        let ifaces = ifaces.iter().map(|&i| self.subst(i, &args)).collect();
        let object = object.map(|o| self.subst(o, &args));
        Some((ifaces, object))
    }

    /// For a value of an intersection interface made here where an object type is expected:
    /// why it does not convert.
    pub(crate) fn object_part_note(&mut self, expected: TyId, found: TyId) -> Option<String> {
        self.meet_parts(found)?;
        if !self.is_object_type(expected) {
            return None;
        }
        let e = self.display(expected);
        Some(format!("`{e}` is an object type: the interface value would be copied into another object; build one from its fields, or keep the interface type"))
    }

    /// The fields of interface value type `t` (its arguments substituted): the flattened ones,
    /// or before flattening the declared ones.
    fn iface_field_types(&mut self, t: TyId) -> Vec<(String, TyId)> {
        let TyKind::Dyn(d, args) = self.ty.kind(t).clone() else {
            return vec![];
        };
        let fields: Vec<(String, TyId)> = self.iface(d).map_or(vec![], |i| {
            i.fields
                .iter()
                .map(|f| (f.name.clone(), f.declared))
                .collect()
        });
        fields
            .into_iter()
            .map(|(n, ty)| (n, self.subst(ty, &args)))
            .collect()
    }

    /// `Pick<I, K>` / `Omit<I, K>` of an interface with methods `t`, `keys` the names `K` lists.
    pub(crate) fn pick_iface(
        &mut self,
        t: TyId,
        mut keys: Vec<String>,
        omit: bool,
        module: usize,
        span: Span,
    ) -> TyId {
        keys.sort();
        keys.dedup();
        self.synth_iface(
            Form::Pick {
                iface: t,
                keys,
                omit,
            },
            module,
            span,
        )
    }

    /// The interface for `form` (written over the caller's type parameters), made once.
    fn synth_iface(&mut self, form: Form, module: usize, span: Span) -> TyId {
        let mut params: Vec<u32> = vec![];
        let parts: Vec<TyId> = match &form {
            Form::Meet { ifaces, object } => ifaces.iter().chain(object).copied().collect(),
            Form::Pick { iface, .. } => vec![*iface],
        };
        for &p in &parts {
            crate::types::collect_params(&self.ty, p, &mut params);
        }
        let own: Vec<TyId> = (0..params.len() as u32).map(|i| self.ty.param(i)).collect();
        let renumber = |cx: &mut Self, t: TyId| {
            cx.ty.map(t, &mut |k| match k {
                TyKind::Param(i) => params.iter().position(|p| p == i).map(|j| own[j]),
                _ => None,
            })
        };
        let generic = match form {
            Form::Meet { ifaces, object } => Form::Meet {
                ifaces: ifaces.iter().map(|&i| renumber(self, i)).collect(),
                object: object.map(|o| renumber(self, o)),
            },
            Form::Pick { iface, keys, omit } => Form::Pick {
                iface: renumber(self, iface),
                keys,
                omit,
            },
        };
        let args: Vec<TyId> = params.iter().map(|&n| self.ty.param(n)).collect();
        if let Some(&d) = self.synth_ifaces.by_form.get(&generic) {
            return self.ty.intern(TyKind::Dyn(d, args));
        }
        let name = self.form_name(&generic);
        let mut generics = Generics::default();
        for i in 0..params.len() {
            generics.push(&format!("T{i}"));
        }
        let d = self.alloc_def(
            Span::DUMMY,
            DefInfo::Iface(Box::new(IfaceInfo {
                name: name.clone(),
                qual_name: name,
                module,
                span: Span::DUMMY,
                generics,
                fields: vec![],
                methods: vec![],
                parents: vec![],
                decl: None,
            })),
        );
        self.synth_ifaces.by_form.insert(generic.clone(), d);
        self.synth_ifaces.forms.insert(d, (generic, span));
        match self.synth_ifaces.flattened {
            true => self.fill_synth(d),
            false => self.synth_ifaces.pending.push(d),
        }
        self.ty.intern(TyKind::Dyn(d, args))
    }

    /// How interface value type `Dyn(d, args)` is shown when `d` is made here: its form with
    /// `args` for its parameters (`names` naming those in scope).
    pub(crate) fn synth_display(
        &self,
        d: DefId,
        args: &[TyId],
        names: &[String],
    ) -> Option<String> {
        let (form, _) = self.synth_ifaces.forms.get(&d)?;
        let bound: Vec<String> = args.iter().map(|&t| self.display_in(t, names)).collect();
        let show = |t: TyId| self.display_in(t, &bound);
        Some(match form {
            Form::Meet { ifaces, object } => {
                let parts: Vec<String> = ifaces.iter().chain(object).map(|&t| show(t)).collect();
                parts.join(" & ")
            }
            Form::Pick { iface, keys, omit } => {
                let op = if *omit { "Omit" } else { "Pick" };
                let keys: Vec<String> = keys.iter().map(|k| format!("{k:?}")).collect();
                format!("{op}<{}, {}>", show(*iface), keys.join(" | "))
            }
        })
    }

    /// The name of synthesized interface `d`.
    fn synth_ifaces_name(&self, d: DefId) -> String {
        self.iface(d).map_or(String::new(), |i| i.name.clone())
    }

    /// How a form is shown (its parameters as `T0`, `T1`, …, which are its own).
    fn form_name(&mut self, form: &Form) -> String {
        match form {
            Form::Meet { ifaces, object } => {
                let parts: Vec<String> = ifaces
                    .iter()
                    .chain(object)
                    .map(|&t| self.display(t))
                    .collect();
                parts.join(" & ")
            }
            Form::Pick { iface, keys, omit } => {
                let op = if *omit { "Omit" } else { "Pick" };
                let keys: Vec<String> = keys.iter().map(|k| format!("{k:?}")).collect();
                format!("{op}<{}, {}>", self.display(*iface), keys.join(" | "))
            }
        }
    }

    /// Interfaces are flattened: fill the interfaces made before.
    pub(crate) fn fill_synth_ifaces(&mut self) {
        self.synth_ifaces.flattened = true;
        for d in std::mem::take(&mut self.synth_ifaces.pending) {
            self.fill_synth(d);
        }
    }

    /// Why interface parts `ifaces` don't combine: a member of one name with different types
    /// in two of them (methods are overloads in TypeScript).
    fn method_clash(&mut self, ifaces: &[TyId]) -> Option<String> {
        for (k, &i) in ifaces.iter().enumerate() {
            let TyKind::Dyn(d, args) = self.ty.kind(i).clone() else {
                continue;
            };
            let ours = self.iface_members(d, &args);
            for &other in &ifaces[k + 1..] {
                let TyKind::Dyn(e, eargs) = self.ty.kind(other).clone() else {
                    continue;
                };
                for (name, m) in self.iface_members(e, &eargs) {
                    if ours.iter().any(|(n, x)| *n == name && *x != m) {
                        let (a, b) = (self.display(i), self.display(other));
                        return Some(format!(
                            "member `{name}` differs between `{a}` and `{b}` (overloads are not supported)"
                        ));
                    }
                }
            }
        }
        None
    }

    /// The members of synthesized interface `d` (module docs).
    fn fill_synth(&mut self, d: DefId) {
        let (form, span) = self.synth_ifaces.forms[&d].clone();
        match form {
            Form::Meet { ifaces, object } => {
                // Checked again now that the parts' methods are known.
                if let Some(why) = self.method_clash(&ifaces) {
                    self.error(
                        Diagnostic::error(
                            format!("no value has type `{}`: {why}", self.synth_ifaces_name(d)),
                            span,
                        )
                        .with_note("a value of an intersection has every part's members at once"),
                    );
                    return;
                }
                let mut own: Vec<FieldInfo> = vec![];
                if let Some(o) = object {
                    let inherited: Vec<(String, TyId)> = ifaces
                        .iter()
                        .flat_map(|&i| self.iface_field_types(i))
                        .collect();
                    for f in self.public_fields(o) {
                        let mut f = f;
                        if let Some(&(_, ty)) = inherited.iter().find(|(n, _)| *n == f.name) {
                            if let Ok(m) = self.meet(ty, f.ty, 0, 1) {
                                f.ty = m;
                            }
                        }
                        own.push(self.iface_field(&f));
                    }
                }
                iface_mut(self, d).fields = own;
                let mut parents: Vec<Bound> = vec![];
                for &i in &ifaces {
                    let TyKind::Dyn(p, args) = self.ty.kind(i).clone() else {
                        continue;
                    };
                    let b = Bound { iface: p, args };
                    crate::collect::iface_extends::inherit_from(self, d, &b, &|_| true);
                    let grand = self.iface(p).map(|x| x.parents.clone()).unwrap_or_default();
                    let grand: Vec<Bound> = grand
                        .into_iter()
                        .map(|g| Bound {
                            iface: g.iface,
                            args: g.args.iter().map(|&t| self.subst(t, &b.args)).collect(),
                        })
                        .collect();
                    for a in std::iter::once(b).chain(grand) {
                        if !parents.contains(&a) {
                            parents.push(a);
                        }
                    }
                }
                iface_mut(self, d).parents = parents;
            }
            Form::Pick { iface, keys, omit } => {
                let TyKind::Dyn(p, args) = self.ty.kind(iface).clone() else {
                    return;
                };
                let names: Vec<String> = self.iface(p).map_or(vec![], |i| {
                    i.fields
                        .iter()
                        .map(|f| f.name.clone())
                        .chain(i.methods.iter().map(|m| m.name.clone()))
                        .collect()
                });
                if !omit {
                    if let Some(k) = keys.iter().find(|k| !names.contains(k)) {
                        let shown = self.display(iface);
                        self.error(
                            Diagnostic::error(
                                format!("`Pick` of `{shown}`: it has no member `{k}`"),
                                span,
                            )
                            .with_note("`Pick<T, K>` takes the names of members of `T`"),
                        );
                    }
                }
                let b = Bound { iface: p, args };
                let keep = |n: &str| keys.iter().any(|k| k == n || n == format!("set {k}")) != omit;
                crate::collect::iface_extends::inherit_from(self, d, &b, &keep);
            }
        }
    }

    /// An interface field declared as `f`.
    fn iface_field(&mut self, f: &ShapeField) -> FieldInfo {
        let dynamic = f.optional && self.dynamic(f.ty).is_some();
        let ty = match f.optional && !dynamic {
            true => self.ty.option_of(f.ty, Nothing::Undefined),
            false => f.ty,
        };
        FieldInfo {
            name: f.name.clone(),
            ty,
            declared: f.ty,
            span: Span::DUMMY,
            readonly: f.readonly,
            optional: f.optional,
            has_default: false,
            default: None,
            default_throws: vec![],
            private_to: None,
        }
    }

    /// The members of interface `d` at `args` (its `Self` and its methods' own type parameters
    /// as fixed placeholders), for comparing two interfaces: per name, the field's read type
    /// and whether it is `readonly`, or the method's type.
    fn iface_members(&mut self, d: DefId, args: &[TyId]) -> Vec<(String, Member)> {
        let Some(i) = self.iface(d) else {
            return vec![];
        };
        let fields: Vec<(String, TyId, bool)> = i
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty, f.readonly))
            .collect();
        let methods: Vec<(String, Vec<TyId>, TyId, bool, usize)> = i
            .methods
            .iter()
            .map(|m| {
                (
                    m.name.clone(),
                    m.params.iter().map(|p| p.ty).collect(),
                    m.ret,
                    m.is_getter,
                    m.generics.len(),
                )
            })
            .collect();
        let mut out = vec![];
        for (n, ty, readonly) in fields {
            out.push((n, Member::Field(self.subst(ty, args), readonly)));
        }
        for (n, params, ret, getter, k) in methods {
            let mut margs = args.to_vec();
            margs.push(self.ty.param(SELF_PLACEHOLDER));
            margs.extend((0..k as u32).map(|j| self.ty.param(SELF_PLACEHOLDER + 1 + j)));
            let params = params.iter().map(|&p| self.subst(p, &margs)).collect();
            let ret = self.subst(ret, &margs);
            out.push((n, Member::Method(params, ret, getter, k)));
        }
        out
    }

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
        // A conversion back (`A` to `B` and `B` to `A`) would make each table hold the other.
        let known = self.synth_ifaces.upcasts.contains(&(have, want));
        if !known && self.reaches_upcast(want, have) {
            return false;
        }
        let ours = self.iface_members(have, from);
        let theirs = self.iface_members(want, to);
        let fits = theirs.iter().all(|(n, m)| {
            ours.iter().any(|(o, x)| {
                o == n
                    && match (x, m) {
                        (Member::Field(a, ra), Member::Field(b, rb)) => a == b && (*rb || !ra),
                        (a, b) => a == b,
                    }
            })
        });
        if fits && !known {
            self.synth_ifaces.upcasts.push((have, want));
        }
        fits
    }

    /// Does an interface value of `from` convert to `to`, directly or through others?
    fn reaches_upcast(&self, from: DefId, to: DefId) -> bool {
        let mut stack = vec![from];
        let mut seen = HashSet::new();
        while let Some(d) = stack.pop() {
            if d == to {
                return true;
            }
            if !seen.insert(d) {
                continue;
            }
            for &(a, b) in &self.synth_ifaces.upcasts {
                if a == d {
                    stack.push(b);
                }
            }
        }
        false
    }

    /// An implementation of synthesized interface `iface` for `ty`, a class or struct that
    /// implements each of its interface parts and has its object part's fields (or, for a
    /// `Pick`, implements the picked interface): its slots are those implementations' methods
    /// and the fields' getters, so the value is the same object.
    pub(crate) fn synth_impl(&mut self, ty: TyId, iface: DefId) -> Option<(u32, Vec<TyId>, TyId)> {
        let (form, _) = self.synth_ifaces.forms.get(&iface)?.clone();
        let TyKind::Adt(d, targs) = self.ty.kind(ty).clone() else {
            return None;
        };
        // Only a nominal type: an object literal implements the interface itself.
        if !self.class_of(ty).is_some() && self.adt(d)?.kind != crate::hir::AdtKind::Struct {
            return None;
        }
        if !targs.is_empty() || !self.synth_ifaces.tried.insert((ty, iface)) {
            return None;
        }
        let parts: Vec<TyId> = match &form {
            Form::Meet { ifaces, .. } => ifaces.clone(),
            Form::Pick { iface, .. } => vec![*iface],
        };
        let n = self.iface(iface)?.generics.len();
        let mut slots: Vec<Option<TyId>> = vec![None; n];
        // Per part: the implementation for `ty`.
        let mut impls: Vec<(DefId, u32)> = vec![];
        for &p in &parts {
            let TyKind::Dyn(pd, pargs) = self.ty.kind(p).clone() else {
                return None;
            };
            let (index, iargs, imp_ty) = self.find_impl(ty, pd)?;
            if imp_ty != ty {
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
        for (name, fty) in fields {
            if let Some(m) = self.part_slot(&impls, &name, true) {
                methods.push(m);
                continue;
            }
            let k = own
                .iter()
                .position(|f| f.name == name && f.private_to.is_none())?;
            let want = self.subst(fty, &args);
            if own[k].ty != want {
                return None;
            }
            methods.push(crate::collect::getters::field_getter(self, d, ty, k, want));
        }
        self.impls.push(ImplDef {
            ty,
            generics: 0,
            iface,
            iface_args: args.clone(),
            methods,
        });
        let index = (self.impls.len() - 1) as u32;
        Some((index, args, ty))
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
                true => ai
                    .fields
                    .iter()
                    .position(|f| f.name == name)
                    .map(|k| ai.methods.len() + k),
            };
            s.expect("ICE: an upcast target member the source lacks")
        };
        let mut slots: Vec<usize> = bi.methods.iter().map(|m| slot_of(&m.name, false)).collect();
        slots.extend(bi.fields.iter().map(|f| slot_of(&f.name, true)));
        if self.iface_written(b) {
            let n = ai.methods.len() + ai.fields.len();
            let writable = self.writable_fields(a);
            for k in self.writable_fields(b) {
                let name = &self.iface(b).expect("ICE: iface").fields[k].name;
                let f = self
                    .iface(a)
                    .expect("ICE: iface")
                    .fields
                    .iter()
                    .position(|f| &f.name == name);
                let at = f.and_then(|f| writable.iter().position(|&w| w == f));
                slots.push(n + at.expect("ICE: an upcast target setter the source lacks"));
            }
        }
        // `b`'s arguments: from the interface types the conversion was checked with, the
        // members agree, so `a`'s implementation arguments give `b`'s by matching its fields
        // and methods; interfaces made here and parents name them directly.
        let iface_args = self.upcast_args(a, &imp.iface_args, b);
        let methods = slots
            .into_iter()
            .map(|s| *imp.methods.get(s).expect("ICE: upcast slot out of range"))
            .collect();
        self.impls.push(ImplDef {
            ty: imp.ty,
            generics: imp.generics,
            iface: b,
            iface_args,
            methods,
        });
        (self.impls.len() - 1) as u32
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
                (Member::Field(p, _), Member::Field(t, _)) => {
                    self.match_ty(*p, *t, &mut slots);
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

/// A placeholder parameter for an interface method's `Self` (and, after it, the method's own
/// type parameters) when two interfaces' members are compared.
const SELF_PLACEHOLDER: u32 = 100_000;

/// An interface member, for comparing two interfaces.
#[derive(PartialEq)]
enum Member {
    /// A field: its read type and whether it is `readonly`.
    Field(TyId, bool),
    /// A method: parameter types, result, getter, own type parameter count.
    Method(Vec<TyId>, TyId, bool, usize),
}

fn iface_mut<'a, 'm>(cx: &'a mut Ctx<'m>, d: DefId) -> &'a mut IfaceInfo<'m> {
    match &mut cx.info[d.0 as usize] {
        DefInfo::Iface(i) => i,
        _ => panic!("ICE: def {d:?} is not an interface"),
    }
}
