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

mod upcasts;

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::anon::ShapeField;
use crate::ctx::Ctx;
use crate::defs::{Bound, DefInfo, FieldInfo, Generics, IfaceInfo};
use crate::hir::{DefId, Nothing, TyId, TyKind};

/// What a synthesized interface is made of, over its own type parameters.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum Form {
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
    by_form: FxHashMap<Form, DefId>,
    forms: FxHashMap<DefId, (Form, Span)>,
    /// The parts of an intersection made here as first written (its form has them sorted, so
    /// `A & B` and `B & A` are one interface), for showing it.
    written: FxHashMap<DefId, Vec<TyId>>,
    /// Made before interfaces were flattened: filled by [`Ctx::fill_synth_ifaces`].
    pending: Vec<DefId>,
    flattened: bool,
    /// The interface conversions the program makes, `(from, to)`, in order.
    pub upcasts: Vec<(DefId, DefId)>,
    /// Where the intersection being made is written (for its errors).
    pub span: Span,
    /// (type, interface) pairs [`Ctx::synth_impl`] tried.
    tried: FxHashSet<(TyId, DefId)>,
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
        let mut written: Vec<TyId> = vec![];
        for t in [a, b] {
            match self.written_parts(t) {
                Some(ps) => written.extend(ps),
                None => written.push(t),
            }
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
        if let Some(why) = self.method_clash(&ifaces, object) {
            return Some(Err(why));
        }
        if object.is_none() && ifaces.len() == 1 {
            return Some(Ok(ifaces[0]));
        }
        let form = Form::Meet { ifaces, object };
        Some(Ok(self.synth_iface(form, Some(written), module, span)))
    }

    /// The parts of `t` as written when it is an intersection interface made here, with `t`'s
    /// arguments.
    fn written_parts(&mut self, t: TyId) -> Option<Vec<TyId>> {
        let TyKind::Dyn(d, args) = self.ty.kind(t).clone() else {
            return None;
        };
        let parts = self.synth_ifaces.written.get(&d)?.clone();
        Some(parts.iter().map(|&p| self.subst(p, &args)).collect())
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
        if let Some(note) = self.field_only_parent_note(expected, found) {
            return Some(note);
        }
        self.meet_parts(found)?;
        if !self.is_object_type(expected) {
            return None;
        }
        let e = self.display(expected);
        Some(format!("`{e}` is an object type: the interface value would be copied into another object; build one from its fields, or keep the interface type"))
    }

    /// For a value of an interface that extends a field-only interface, where that interface's
    /// object type is expected: why it does not convert.
    fn field_only_parent_note(&mut self, expected: TyId, found: TyId) -> Option<String> {
        let (TyKind::Dyn(a, _), TyKind::Adt(o, _)) = (self.ty.kind(found), self.ty.kind(expected))
        else {
            return None;
        };
        let b = *self.field_only_of.get(o)?;
        if !self.iface(*a)?.parents.iter().any(|p| p.iface == b) {
            return None;
        }
        let (e, f) = (self.display(expected), self.display(found));
        Some(format!("`{e}` has only fields, so it is an object type with a fixed layout, not an interface: the `{f}` value would be copied into a new object, where TypeScript keeps the same one; keep the type `{f}`, or give `{e}` a method"))
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
        let form = Form::Pick {
            iface: t,
            keys,
            omit,
        };
        self.synth_iface(form, None, module, span)
    }

    /// The interface for `form` (written over the caller's type parameters, an intersection's
    /// parts as `written`), made once.
    fn synth_iface(
        &mut self,
        mut form: Form,
        written: Option<Vec<TyId>>,
        module: usize,
        span: Span,
    ) -> TyId {
        // One interface whatever the order of the parts.
        if let Form::Meet { ifaces, .. } = &mut form {
            let key = |cx: &Self, t: TyId| match cx.ty.kind(t) {
                TyKind::Dyn(d, _) => (d.0, t.0),
                _ => (u32::MAX, t.0),
            };
            ifaces.sort_by_key(|&t| key(self, t));
        }
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
        // Its parameters are named as where it is written (`G<T> & M`, not `G<T0> & M`).
        let own_names: Vec<String> = params
            .iter()
            .map(|&n| crate::ide::named_param(&self.display_params, n))
            .collect();
        let shown_as_written = written.as_ref().map(|w| self.parts_name(w));
        let written: Option<Vec<TyId>> =
            written.map(|w| w.iter().map(|&t| renumber(self, t)).collect());
        let args: Vec<TyId> = params.iter().map(|&n| self.ty.param(n)).collect();
        if let Some(&d) = self.synth_ifaces.by_form.get(&generic) {
            return self.ty.intern(TyKind::Dyn(d, args));
        }
        let name = match shown_as_written {
            Some(n) => n,
            None => {
                let saved = std::mem::replace(&mut self.display_params, own_names.clone());
                let n = self.form_name(&generic);
                self.display_params = saved;
                n
            }
        };
        let mut generics = Generics::default();
        for n in &own_names {
            generics.push(n);
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
        if let Some(w) = written {
            self.synth_ifaces.written.insert(d, w);
        }
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
        if let Some(w) = self.synth_ifaces.written.get(&d) {
            let parts: Vec<String> = w.iter().map(|&t| show(t)).collect();
            return Some(parts.join(" & "));
        }
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

    /// How an intersection of `parts` is shown.
    fn parts_name(&mut self, parts: &[TyId]) -> String {
        let shown: Vec<String> = parts.iter().map(|&t| self.display(t)).collect();
        shown.join(" & ")
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

    /// Have interfaces been flattened (their members are complete)?
    pub(crate) fn synth_ifaces_flattened(&self) -> bool {
        self.synth_ifaces.flattened
    }

    /// Interfaces are flattened: fill the interfaces made before.
    pub(crate) fn fill_synth_ifaces(&mut self) {
        self.synth_ifaces.flattened = true;
        for d in std::mem::take(&mut self.synth_ifaces.pending) {
            self.fill_synth(d);
        }
    }

    /// Why interface parts `ifaces` and object part `object` don't combine: a member of one
    /// name with different types in two interface parts (methods are overloads in
    /// TypeScript), or a field of the object part that is a method of an interface part.
    fn method_clash(&mut self, ifaces: &[TyId], object: Option<TyId>) -> Option<String> {
        let fields = object.map_or(vec![], |o| self.public_fields(o));
        for &i in ifaces {
            let TyKind::Dyn(d, args) = self.ty.kind(i).clone() else {
                continue;
            };
            for (name, m) in self.iface_members(d, &args) {
                if matches!(m, Member::Method(..)) && fields.iter().any(|f| f.name == name) {
                    let (o, i) = (self.display(object?), self.display(i));
                    return Some(format!(
                        "member `{name}` is a field in `{o}` and a method in `{i}`"
                    ));
                }
            }
        }
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
                // Checked again now that the parts' methods are known (its parameters named as
                // where it is written).
                let names = self.iface(d).map(|i| i.generics.names.clone());
                let saved = std::mem::replace(&mut self.display_params, names.unwrap_or_default());
                let clash = self.method_clash(&ifaces, object);
                self.display_params = saved;
                if let Some(why) = clash {
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
}

/// A placeholder parameter for an interface method's `Self` (and, after it, the method's own
/// type parameters) when two interfaces' members are compared.
const SELF_PLACEHOLDER: u32 = 100_000;

/// An interface member, for comparing two interfaces.
#[derive(PartialEq)]
pub(super) enum Member {
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
