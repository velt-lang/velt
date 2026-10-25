//! Field writes through interface values (`x.f = v` with `x: I` or `x: T extends I`, design
//! note on issue #1112, hir_encodings.md "Interface values").
//!
//! An interface's table holds its methods, then one getter per field (`collect::getters`). An
//! interface the program writes a field through also gets one setter slot per field, after the
//! getters: slot `methods + fields + k` for field `k`. A `readonly` field's setter is only used
//! to write back a changed part of the field (`x.o.v = 5`, `body/expr/iface_nested.rs`; TS
//! `readonly` is shallow): user code cannot assign the field itself. The setters are added when the first write through the interface is checked, to every
//! implementation that exists then and to the ones created later, so a program that never
//! writes through an interface keeps its tables (and its code) unchanged.
//!
//! Every implementation records how it stores each field ([`SetterSrc`]) when it is created;
//! the setter methods are synthesized only once the interface is written through.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::collect::forwarders::{synth_method, Host, Target};
use crate::ctx::Ctx;
use crate::defs::{DefInfo, ParamSig};
use crate::hir::{AdtKind, DefId, PassMode, TyId, TyKind};

/// The interfaces written through and how their implementations store fields.
#[derive(Default)]
pub(crate) struct IfaceWrites {
    /// Interfaces with setter slots, and where the first write through each is.
    written: FxHashMap<DefId, Span>,
    /// Per `Ctx::impls` index: how each field of its interface is stored, in field order. An
    /// implementation without an entry cannot be written through.
    sources: FxHashMap<u32, Vec<SetterSrc>>,
    /// `(interface, field)`: a part of this `readonly` field is written through the interface.
    parts: FxHashMap<(DefId, usize), Span>,
    /// The names of interface fields whose values a read copies ([`Ctx::copied_part`]).
    copied_names: Option<FxHashSet<String>>,
    /// `(interface, field)`: the field, or a part of it, is written through the interface
    /// (first at the span).
    fields: FxHashMap<(DefId, usize), Span>,
    /// Adapter fields whose value does not convert back (their setter slot does nothing):
    /// an error once the field is written through the interface ([`Ctx::check_adapter_writes`]).
    unwritable: Vec<(
        Box<crate::body::expr::iface_adapt_slots::AdapterParts>,
        usize,
    )>,
    /// Reads of array fields through interfaces at dynamic arguments (`iface_field`).
    pub(crate) array_reads: crate::body::expr::iface_field::ArrayReads,
}

/// How an implementation stores one interface field.
#[derive(Clone)]
pub(crate) enum SetterSrc {
    /// Field `index` (of type `ty`) of struct / class / object type `d`, self type `self_ty`.
    Field {
        d: DefId,
        self_ty: TyId,
        index: usize,
        ty: TyId,
    },
    /// An object literal's `set f(v)`: the function value in field `index` of the literal's
    /// struct `d` (of function type `fn_ty`, taking a `param`, after the object as interface
    /// value `this.0` through implementation `this.1` when it uses `this`).
    Call {
        d: DefId,
        self_ty: TyId,
        index: u32,
        fn_ty: TyId,
        param: TyId,
        this: Option<(TyId, u32)>,
    },
    /// Class `d`'s `set name(v)` (or the trampoline dispatching to it): the setter slot itself.
    /// Only for writes of the whole field: a part write through a class's accessor is an error
    /// ([`Ctx::accessor_part`]).
    Method { def: DefId, d: DefId, name: String },
    /// A class `d` with a `get` accessor and no `set` for field `name` (of type `ty`): its
    /// setter slot stops with Node's `TypeError` (`collect::getters::getter_only_setter`), and
    /// a part write through it is an error, as for [`SetterSrc::Method`].
    GetterOnly {
        d: DefId,
        self_ty: TyId,
        name: String,
        ty: TyId,
    },
    /// Field `field` of an interface adapter (`body/expr/iface_adapt.rs`): written through the
    /// wrapped value's own setter, the value converted to its field type.
    Adapter {
        parts: Box<crate::body::expr::iface_adapt_slots::AdapterParts>,
        field: usize,
    },
    /// An object literal (at `span`) with a getter and no setter for field `name`.
    Missing { span: Span, name: String },
    /// The same for a `readonly` field (of type `ty`; struct `d`, self type `self_ty`): an
    /// error only when a part of the field is written through the interface.
    Unwritable {
        span: Span,
        name: String,
        d: DefId,
        self_ty: TyId,
        ty: TyId,
    },
}

impl Ctx<'_> {
    /// Does reading a field of type `ty` through an interface give a copy that a write in place
    /// would not reach (a tuple, an object type, an option or a union with one; a type parameter
    /// may be one)?
    pub(crate) fn copied_part(&self, ty: TyId, params: bool) -> bool {
        let ty = self.ty.opt_payload(ty).unwrap_or(ty);
        if let Some((d, _)) = self.union_def(ty) {
            return self.enum_info(d).is_some_and(|e| {
                e.variants.iter().any(|v| {
                    v.payload
                        .first()
                        .is_some_and(|&m| self.copied_part(m, params))
                })
            });
        }
        match self.ty.kind(ty) {
            TyKind::Tuple(_) => true,
            TyKind::Param(_) => params,
            TyKind::Adt(d, _) => self.adt(*d).is_some_and(|a| a.kind != AdtKind::Class),
            _ => false,
        }
    }

    /// Does some interface have a field `name` whose value a read may copy?
    pub(crate) fn copied_field_name(&mut self, name: &str) -> bool {
        if self.iface_writes.copied_names.is_none() {
            let mut names = FxHashSet::default();
            for info in &self.info {
                if let DefInfo::Iface(i) = info {
                    for f in &i.fields {
                        if self.copied_part(f.ty, true) {
                            names.insert(f.name.clone());
                        }
                    }
                }
            }
            self.iface_writes.copied_names = Some(names);
        }
        self.iface_writes
            .copied_names
            .as_ref()
            .is_some_and(|n| n.contains(name))
    }

    /// The indexes of the fields of `iface` (each has a setter slot once it is written through).
    pub(crate) fn setter_fields(&self, iface: DefId) -> Vec<usize> {
        self.iface(iface)
            .map_or(vec![], |i| (0..i.fields.len()).collect())
    }

    /// How implementation `index` stores the writable fields of its interface (`None`: it
    /// cannot be written through).
    pub(crate) fn setter_sources(&self, index: u32) -> Option<&Vec<SetterSrc>> {
        self.iface_writes.sources.get(&index)
    }

    /// Does the program write a field through `iface`?
    pub(crate) fn iface_written(&self, iface: DefId) -> bool {
        self.iface_writes.written.contains_key(&iface)
    }

    /// The setter slot of field `field` of `iface`.
    pub(crate) fn setter_slot(&self, iface: DefId, field: usize) -> Option<u32> {
        let i = self.iface(iface)?;
        (field < i.fields.len()).then_some((i.methods.len() + i.fields.len() + field) as u32)
    }

    /// A part of field `field` of `iface` is written through the interface at `span` (its
    /// setter writes the changed part back). Reports the implementations that cannot take it
    /// (an object literal's getter without a setter for a `readonly` field, a class's `get`
    /// accessor) and returns false then.
    pub(crate) fn mark_part_written(&mut self, iface: DefId, field: usize, span: Span) -> bool {
        self.note_field_written(iface, field, span);
        if !self.mark_iface_written(iface, span) {
            return false;
        }
        if self.iface_writes.parts.contains_key(&(iface, field)) {
            return true;
        }
        self.iface_writes.parts.insert((iface, field), span);
        let mut found = vec![];
        for (&i, srcs) in &self.iface_writes.sources {
            if self.impls[i as usize].iface != iface {
                continue;
            }
            match srcs.get(field) {
                Some(SetterSrc::Unwritable { span, name, .. }) => {
                    found.push((*span, name.clone(), None));
                }
                Some(SetterSrc::Method { d, name, .. } | SetterSrc::GetterOnly { d, name, .. }) => {
                    let at = self.adt(*d).map_or(Span::DUMMY, |a| a.span);
                    found.push((at, name.clone(), Some(*d)));
                }
                _ => {}
            }
        }
        found.sort_by_key(|(s, _, _)| (s.file, s.lo, s.hi));
        let ok = found.is_empty();
        for (at, name, class) in found {
            match class {
                Some(d) => self.accessor_part(iface, d, &name, Some(span)),
                None => self.part_unwritable(iface, at, &name),
            }
        }
        ok
    }

    /// A part of field `name` of `iface` is changed through it (at `write`, when known) and
    /// class `d` provides the field with a `get` accessor: Node changes the object the getter
    /// returns in place, while Velt would change a copy and write it back through a setter
    /// Node never calls, so this is an error.
    fn accessor_part(&mut self, iface: DefId, d: DefId, name: &str, write: Option<Span>) {
        // Through an intersection (`{ k: number } & I`), the interface part declaring the
        // field, as written.
        let declared = (self.iface(iface).is_some_and(|i| i.decl.is_none()))
            .then(|| self.declaring_iface(d, name))
            .flatten();
        let iname = declared.unwrap_or_else(|| self.iface_name(iface));
        let (cname, at) = self
            .adt(d)
            .map_or((String::new(), Span::DUMMY), |a| (a.name.clone(), a.span));
        let mut e = Diagnostic::error(
            format!("`{cname}` provides `{name}` with a `get` accessor, and a part of `{name}` is changed through `{iname}`, which is not supported yet (#350)"),
            at,
        )
        .with_note("Node changes the object the getter returns in place and never calls a setter; Velt reads a copy here and would have to write it back through a setter")
        .with_note(format!("store `{name}` as a field of `{cname}`, or assign the whole value through `{iname}` (`x.{name} = …`)"));
        if let Some(w) = write {
            e = e.with_label(
                w,
                format!("a part of `{name}` is changed through `{iname}` here"),
            );
        }
        self.error(e);
    }

    /// The name of an interface class `d` (or a base class) implements that declares field
    /// `name`.
    fn declaring_iface(&self, d: DefId, name: &str) -> Option<String> {
        let mut cur = Some(d);
        for _ in 0..64 {
            let a = self.adt(cur?)?;
            for b in &a.implements {
                if let Some(i) = self.iface(b.iface).filter(|i| i.decl.is_some()) {
                    if i.fields.iter().any(|f| f.name == name) {
                        return Some(i.name.clone());
                    }
                }
            }
            cur = a.base.and_then(|b| self.class_of(b)).map(|(b, _)| b);
        }
        None
    }

    fn part_unwritable(&mut self, iface: DefId, span: Span, name: &str) {
        let iname = self.iface_name(iface);
        self.error(
            Diagnostic::error(
                format!("this object literal has a getter but no setter for `{name}`, a part of which is changed through `{iname}`"),
                span,
            )
            .with_note(format!("add `set {name}(v)` to the object literal: the changed value is written back through it")),
        );
    }

    /// Implementation `index` stores the writable fields of its interface as `srcs` (in field
    /// order). Adds its setters at once when the interface is already written through.
    pub(crate) fn note_setters(&mut self, index: u32, srcs: Vec<SetterSrc>) {
        self.iface_writes.sources.insert(index, srcs);
        let iface = self.impls[index as usize].iface;
        if self.iface_written(iface) {
            self.add_setters(index);
        }
    }

    /// A field of `iface` is written through an interface value at `span`: from now on its
    /// implementations have setter slots. Returns false (after reporting) when one of them
    /// cannot be written through.
    pub(crate) fn mark_iface_written(&mut self, iface: DefId, span: Span) -> bool {
        if self.iface_written(iface) {
            return true;
        }
        let impls: Vec<u32> = (0..self.impls.len() as u32)
            .filter(|&i| self.impls[i as usize].iface == iface)
            .collect();
        if let Some(&i) = impls
            .iter()
            .find(|&&i| !self.iface_writes.sources.contains_key(&i))
        {
            let ty = self.impls[i as usize].ty;
            let (iname, tn) = (self.iface_name(iface), self.display(ty));
            self.err(
                format!("writing a field through `{iname}` is not supported yet: `{tn}` implements it in a way that has no setters"),
                span,
            );
            return false;
        }
        self.iface_writes.written.insert(iface, span);
        for i in impls {
            self.add_setters(i);
        }
        // The dispatch groups gain the setters (`throws/groups.rs`): built again on next use.
        self.groups = None;
        true
    }

    /// An implementation of `iface` with no setters is created after a write through `iface`
    /// (where the write is): reported as when it existed at the write.
    pub(crate) fn no_setters_after_write(&mut self, index: u32) {
        let iface = self.impls[index as usize].iface;
        let Some(&span) = self.iface_writes.written.get(&iface) else {
            return;
        };
        let ty = self.impls[index as usize].ty;
        let (iname, tn) = (self.iface_name(iface), self.display(ty));
        self.err(
            format!("writing a field through `{iname}` is not supported yet: `{tn}` implements it in a way that has no setters"),
            span,
        );
    }

    /// Has field `field` of `iface` (or a part of it) been written through the interface?
    pub(crate) fn field_written(&self, iface: DefId, field: usize) -> bool {
        self.iface_writes.fields.contains_key(&(iface, field))
    }

    /// Once every body is checked, for the interface conversions `pairs` (`a` to `b`,
    /// `crate::synth_ifaces`): a field of `b` written through it (or a part of one) is written
    /// through `a` too, whose implementations then give `b`'s setters. A field `a` gives with
    /// a getter (`get k()`) can only be written whole, through its setter: a part of it
    /// changed through `b` is an error, as for a class's accessor.
    pub(crate) fn writes_through_upcasts(&mut self, pairs: &[(DefId, DefId)]) {
        // Each is tried once: one that cannot be written through is reported once.
        let mut tried: FxHashSet<DefId> = FxHashSet::default();
        let mut parts: FxHashSet<(DefId, usize)> = FxHashSet::default();
        let mut reported: FxHashSet<(DefId, usize)> = FxHashSet::default();
        loop {
            let mut changed = false;
            for &(a, b) in pairs {
                let Some(&span) = self.iface_writes.written.get(&b) else {
                    continue;
                };
                let mut fields: Vec<(usize, Span)> = (self.iface_writes.fields.iter())
                    .filter(|((i, _), _)| *i == b)
                    .map(|(&(_, k), &at)| (k, at))
                    .collect();
                fields.sort_unstable_by_key(|f| f.0);
                for (k, at) in fields {
                    let part = self.iface_writes.parts.contains_key(&(b, k));
                    let name = self
                        .iface(b)
                        .map_or(String::new(), |i| i.fields[k].name.clone());
                    let ours =
                        (self.iface(a)).and_then(|i| i.fields.iter().position(|f| f.name == name));
                    match ours {
                        Some(g) if part && parts.insert((a, g)) => {
                            changed = true;
                            self.mark_part_written(a, g, at);
                        }
                        Some(g) if !self.field_written(a, g) => {
                            changed = true;
                            self.note_field_written(a, g, at);
                        }
                        Some(_) => {}
                        None if part => {
                            let iname = self.iface_name(b);
                            self.error(
                                Diagnostic::error(
                                    format!("a part of `{name}` is changed through `{iname}`, whose value gives `{name}` with a getter, which is not supported yet (#350)"),
                                    at,
                                )
                                .with_note("Node changes the object the getter returns in place and never calls a setter; Velt reads a copy here and would have to write it back through a setter"),
                            );
                            self.iface_writes.parts.remove(&(b, k));
                        }
                        None => {
                            let set = format!("set {name}");
                            let setter = self
                                .iface(a)
                                .is_some_and(|i| i.methods.iter().any(|m| m.name == set));
                            let readonly = self.iface(b).is_some_and(|i| i.fields[k].readonly);
                            if !setter && !readonly && reported.insert((a, k)) {
                                let iname = self.iface_name(b);
                                self.error(
                                    Diagnostic::error(
                                        format!("`{name}` is written through `{iname}`, whose value gives `{name}` with a getter and no setter"),
                                        at,
                                    )
                                    .with_note(format!("Node throws a `TypeError` here; add `set {name}(v)` to the object literal")),
                                );
                            }
                        }
                    }
                }
                if !self.iface_written(a) && tried.insert(a) && self.mark_iface_written(a, span) {
                    changed = true;
                }
            }
            if !changed {
                return;
            }
        }
    }

    /// Field `field` of `iface` (or a part of it) is written through the interface.
    pub(crate) fn note_field_written(&mut self, iface: DefId, field: usize, span: Span) {
        self.iface_writes
            .fields
            .entry((iface, field))
            .or_insert(span);
    }

    /// Once every body is checked: report the adapter fields written through their interface
    /// that do not convert back ([`IfaceWrites::unwritable`]).
    pub(crate) fn check_adapter_writes(&mut self) {
        let list = std::mem::take(&mut self.iface_writes.unwritable);
        let mut reported = FxHashSet::default();
        for (parts, field) in list {
            let key = (parts.iface, field);
            if let Some(&at) = self.iface_writes.fields.get(&key) {
                if reported.insert(key) {
                    self.adapter_unwritable(at, &parts, field);
                }
            }
        }
    }

    /// A field written through `iface` that an adapter from another instance of it cannot write
    /// back (its value does not convert to the wrapped value's field type).
    fn adapter_unwritable(
        &mut self,
        span: Span,
        parts: &crate::body::expr::iface_adapt_slots::AdapterParts,
        field: usize,
    ) {
        let iface = parts.iface;
        let fname = self
            .iface(iface)
            .and_then(|i| i.fields.get(field))
            .map_or(String::new(), |f| f.name.clone());
        let to = self.ty.intern(TyKind::Dyn(iface, parts.to.clone()));
        let from = self.ty.intern(TyKind::Dyn(iface, parts.from.clone()));
        let t = self.display(to);
        // A generic adapter's source arguments are its own type parameters.
        let f = match parts.generics {
            0 => format!("a `{}`", self.display(from)),
            _ => format!(
                "a value implementing `{}` at other type arguments",
                parts.iname
            ),
        };
        self.error(
            Diagnostic::error(
                format!("writing `{fname}` through `{t}` is not supported yet: {f} is converted to it here, and the value would not convert back to its field (#1185)"),
                parts.span,
            )
            .with_label(span, format!("`{fname}` is written through `{t}` here")),
        );
    }

    pub(crate) fn iface_name(&self, iface: DefId) -> String {
        self.iface(iface).map_or(String::new(), |i| i.name.clone())
    }

    /// Appends the setters of implementation `index` (once).
    fn add_setters(&mut self, index: u32) {
        let imp = &self.impls[index as usize];
        let iface = imp.iface;
        let Some(i) = self.iface(iface) else {
            return;
        };
        if imp.methods.len() != i.methods.len() + i.fields.len() {
            return;
        }
        let srcs = self.iface_writes.sources[&index].clone();
        let mut defs = vec![];
        for (k, src) in srcs.into_iter().enumerate() {
            match src {
                SetterSrc::Field {
                    d,
                    self_ty,
                    index,
                    ty,
                } => defs.push(crate::collect::getters::field_setter(
                    self, d, self_ty, index, ty,
                )),
                SetterSrc::Call {
                    d,
                    self_ty,
                    index,
                    fn_ty,
                    param,
                    this,
                } => defs.push(self.literal_setter(d, self_ty, (index, fn_ty), param, this)),
                SetterSrc::Method { d, name, .. } | SetterSrc::GetterOnly { d, name, .. }
                    if self.iface_writes.parts.contains_key(&(iface, k)) =>
                {
                    let write = self.iface_writes.parts.get(&(iface, k)).copied();
                    self.accessor_part(iface, d, &name, write);
                    return;
                }
                SetterSrc::Method { def, .. } => defs.push(def),
                SetterSrc::Adapter { parts, field } => {
                    match crate::body::expr::iface_adapt_slots::adapter_setter(self, &parts, field)
                    {
                        Some(def) => defs.push(def),
                        None => {
                            // A setter that is never called unless the field is written
                            // through the interface, which is then an error.
                            let f = self.iface(iface).and_then(|i| i.fields.get(field).cloned());
                            let Some(f) = f else {
                                return;
                            };
                            let mut targs = parts.to.clone();
                            targs.push(parts.self_ty);
                            let ty = self.subst(f.ty, &targs);
                            defs.push(crate::collect::getters::unused_setter(
                                self,
                                parts.def,
                                parts.self_ty,
                                &f.name,
                                ty,
                            ));
                            self.iface_writes.unwritable.push((parts, field));
                        }
                    }
                }
                SetterSrc::GetterOnly {
                    d,
                    self_ty,
                    name,
                    ty,
                } => defs.push(crate::collect::getters::getter_only_setter(
                    self, d, self_ty, &name, ty,
                )),
                SetterSrc::Missing { span, name } => {
                    let iname = self.iface_name(iface);
                    self.error(
                        Diagnostic::error(
                            format!("this object literal has a getter but no setter for `{name}`, which is written through `{iname}`"),
                            span,
                        )
                        .with_note(format!("add `set {name}(v)` to the object literal")),
                    );
                    return;
                }
                SetterSrc::Unwritable {
                    span,
                    name,
                    d,
                    self_ty,
                    ty,
                } => {
                    if self.iface_writes.parts.contains_key(&(iface, k)) {
                        self.part_unwritable(iface, span, &name);
                        return;
                    }
                    defs.push(crate::collect::getters::unused_setter(
                        self, d, self_ty, &name, ty,
                    ));
                }
            }
        }
        self.impls[index as usize].methods.extend(defs);
    }

    /// The setter slot of an object literal's `set f(v)`: calls the function value it stores.
    fn literal_setter(
        &mut self,
        d: DefId,
        self_ty: TyId,
        (index, fn_ty): (u32, TyId),
        param: TyId,
        this: Option<(TyId, u32)>,
    ) -> DefId {
        let host = Host::adt(self, d);
        let name = format!("{}.<set>", self.adt(d).map_or("", |a| &a.qual_name));
        let mode = if self.is_copy(param) {
            PassMode::Copy
        } else {
            PassMode::Owned
        };
        let params = vec![ParamSig {
            name: "v".into(),
            span: host.span,
            ty: param,
            mode,
            default: None,
        }];
        let unit = self.ty.unit;
        let target = Target::Field { index, fn_ty, this };
        synth_method(self, &host, self_ty, name, params, unit, false, target)
    }
}
