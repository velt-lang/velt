//! Field writes through interface values (`x.f = v` with `x: I` or `x: T extends I`, design
//! note on issue #1112, hir_encodings.md "Interface values").
//!
//! An interface's table holds its methods, then one getter per field (`collect::getters`). An
//! interface the program writes a field through also gets one setter slot per writable field,
//! after the getters: slot `methods + fields + k`, `k` counting the writable fields before the
//! field. The setters are added when the first write through the interface is checked, to every
//! implementation that exists then and to the ones created later, so a program that never
//! writes through an interface keeps its tables (and its code) unchanged.
//!
//! Every implementation records how it stores each writable field ([`SetterSrc`]) when it is
//! created; the setter methods are synthesized only once the interface is written through.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use crate::collect::forwarders::{synth_method, Host, Target};
use crate::ctx::Ctx;
use crate::defs::ParamSig;
use crate::hir::{DefId, PassMode, TyId};

/// The interfaces written through and how their implementations store fields.
#[derive(Default)]
pub(crate) struct IfaceWrites {
    /// Interfaces with setter slots.
    written: FxHashSet<DefId>,
    /// Per `Ctx::impls` index: how each writable field of its interface is stored, in field
    /// order. An implementation without an entry cannot be written through.
    sources: FxHashMap<u32, Vec<SetterSrc>>,
}

/// How an implementation stores one writable interface field.
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
    /// An object literal (at `span`) with a getter and no setter for field `name`.
    Missing { span: Span, name: String },
}

impl Ctx<'_> {
    /// The indexes of the fields of `iface` that are not `readonly`.
    pub(crate) fn writable_fields(&self, iface: DefId) -> Vec<usize> {
        self.iface(iface).map_or(vec![], |i| {
            (0..i.fields.len())
                .filter(|&k| !i.fields[k].readonly)
                .collect()
        })
    }

    /// How implementation `index` stores the writable fields of its interface (`None`: it
    /// cannot be written through).
    pub(crate) fn setter_sources(&self, index: u32) -> Option<&Vec<SetterSrc>> {
        self.iface_writes.sources.get(&index)
    }

    /// Does the program write a field through `iface`?
    pub(crate) fn iface_written(&self, iface: DefId) -> bool {
        self.iface_writes.written.contains(&iface)
    }

    /// The setter slot of field `field` of `iface` (`None` for a `readonly` field).
    pub(crate) fn setter_slot(&self, iface: DefId, field: usize) -> Option<u32> {
        let i = self.iface(iface)?;
        let k = self
            .writable_fields(iface)
            .iter()
            .position(|&f| f == field)?;
        Some((i.methods.len() + i.fields.len() + k) as u32)
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
        self.iface_writes.written.insert(iface);
        for i in impls {
            self.add_setters(i);
        }
        // The dispatch groups gain the setters (`throws/groups.rs`): built again on next use.
        self.groups = None;
        true
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
        for src in srcs {
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
