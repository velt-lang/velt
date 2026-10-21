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
    parts: FxHashSet<(DefId, usize)>,
    /// The names of interface fields whose values a read copies ([`Ctx::copied_part`]).
    copied_names: Option<FxHashSet<String>>,
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
    /// struct `d` (of function type `fn_ty`, taking a `param`).
    Call {
        d: DefId,
        self_ty: TyId,
        index: u32,
        fn_ty: TyId,
        param: TyId,
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
        if !self.mark_iface_written(iface, span) {
            return false;
        }
        if !self.iface_writes.parts.insert((iface, field)) {
            return true;
        }
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
        let iname = self.iface_name(iface);
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

    fn iface_name(&self, iface: DefId) -> String {
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
                } => defs.push(self.literal_setter(d, self_ty, (index, fn_ty), param)),
                SetterSrc::Method { d, name, .. } | SetterSrc::GetterOnly { d, name, .. }
                    if self.iface_writes.parts.contains(&(iface, k)) =>
                {
                    self.accessor_part(iface, d, &name, None);
                    return;
                }
                SetterSrc::Method { def, .. } => defs.push(def),
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
                    if self.iface_writes.parts.contains(&(iface, k)) {
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
        let target = Target::Field { index, fn_ty };
        synth_method(self, &host, self_ty, name, params, unit, false, target)
    }
}
