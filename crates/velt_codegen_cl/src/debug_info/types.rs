//! DWARF types of the debug types of source variables (`vir::Program::debug_types`).
//!
//! Each debug type keeps its source name, so a debugger shows `number`, `Point` or
//! `string[]`, and the LLDB formatters (editors/lldb) recognize a type by it:
//! - scalars: a typedef with the source name of the base type of the VIR type that holds the
//!   value (a `number` counter stored as an integer is `number`, a typedef of `i32`);
//! - `string`: a 24-byte structure of three words (rt_abi.md "Strings"), decoded by the
//!   formatters;
//! - arrays: `{ data: T*, len, cap }`; structs, tuples: structures with named members at
//!   their layout offsets;
//! - classes: a typedef named after the class, of a pointer to the object structure
//!   (`$vtable` first when objects have one);
//! - enums without payloads: enumerations over `i64`; tagged types: a typedef of a structure
//!   `<name> $tagged` holding the tag, then a union of one structure per variant (its view
//!   aggregate);
//! - options: the inner type (a pointer niche), a `boolean`, or a typedef of a structure
//!   `<name> $option` holding `{ some, value }`;
//!   `shared<T>`: a typedef of a pointer to `{ count, value }`;
//! - anything else: its raw words.

use std::collections::HashMap;

use cranelift_codegen::gimli::write::{AttributeValue, DwarfUnit, UnitEntryId};
use cranelift_codegen::gimli::{self, DwAte, DwTag};
use velt_vir::vir::{self, AggId, DebugField, DebugKind, DebugTyId, Ty};

use super::dwarf_str;

/// Builds (and shares) the DWARF type entries of one compile unit.
pub(super) struct Types<'p> {
    program: &'p vir::Program,
    /// By debug type and, for scalars, the VIR type holding the value.
    described: HashMap<(DebugTyId, Option<Ty>), UnitEntryId>,
    raw: HashMap<Ty, UnitEntryId>,
}

impl<'p> Types<'p> {
    pub(super) fn new(program: &'p vir::Program) -> Self {
        Types {
            program,
            described: HashMap::new(),
            raw: HashMap::new(),
        }
    }

    /// The DWARF type of a value of debug type `id`; `held` is the VIR type of the local or
    /// field holding it (the encoding of a scalar or enum, which optimizations may narrow),
    /// `None` for its natural one.
    pub(super) fn ty(
        &mut self,
        dwarf: &mut DwarfUnit,
        id: DebugTyId,
        held: Option<Ty>,
    ) -> UnitEntryId {
        let t = self.program.debug_ty(id);
        let held = match t.kind {
            DebugKind::Scalar(natural) => Some(held.unwrap_or(natural)),
            DebugKind::Enum { .. } => Some(held.unwrap_or(Ty::I64)),
            _ => None,
        };
        if let Some(&e) = self.described.get(&(id, held)) {
            return e;
        }
        let name = t.name.clone();
        let entry = match &t.kind {
            // A typedef carries the source name: debuggers show C base types by their C names.
            DebugKind::Scalar(natural) => {
                let base = self.raw(dwarf, held.unwrap_or(*natural));
                let e = new_entry(dwarf, gimli::DW_TAG_typedef, &name);
                set_type(dwarf, e, base);
                self.described.insert((id, held), e);
                return e;
            }
            // Registered before its parts, so a type reached again through itself (a class with
            // a field of its own type) refers to this entry.
            DebugKind::Class { .. }
            | DebugKind::Option { .. }
            | DebugKind::Shared { .. }
            | DebugKind::Tagged { .. } => new_entry(dwarf, gimli::DW_TAG_typedef, &name),
            DebugKind::Enum { .. } => new_entry(dwarf, gimli::DW_TAG_enumeration_type, &name),
            _ => new_entry(dwarf, gimli::DW_TAG_structure_type, &name),
        };
        self.described.insert((id, held), entry);
        let kind = t.kind.clone();
        match kind {
            DebugKind::Scalar(_) => unreachable!("returned above"),
            DebugKind::Str => {
                set_size(dwarf, entry, 24);
                let w = self.raw(dwarf, Ty::U64);
                for (i, n) in ["w0", "w1", "w2"].into_iter().enumerate() {
                    member(dwarf, entry, n, w, 8 * i as u64);
                }
            }
            DebugKind::Array { elem } => {
                set_size(dwarf, entry, 24);
                let e = self.ty(dwarf, elem, None);
                let data = pointer(dwarf, Some(e));
                member(dwarf, entry, "data", data, 0);
                let w = self.raw(dwarf, Ty::U64);
                member(dwarf, entry, "len", w, 8);
                member(dwarf, entry, "cap", w, 16);
            }
            DebugKind::Struct { agg, fields } => {
                set_size(dwarf, entry, self.program.agg(agg).size.into());
                self.fields(dwarf, entry, agg, &fields);
            }
            DebugKind::Class { obj, fields } => {
                let object = new_entry(
                    dwarf,
                    gimli::DW_TAG_structure_type,
                    &format!("{name} object"),
                );
                let layout = self.program.agg(obj);
                set_size(dwarf, object, layout.size.into());
                // The object's fields are the described ones, after a vtable pointer if any.
                if layout.fields.len() == fields.len() + 1 {
                    let p = pointer(dwarf, None);
                    member(dwarf, object, "$vtable", p, 0);
                }
                self.fields(dwarf, object, obj, &fields);
                let p = pointer(dwarf, Some(object));
                set_type(dwarf, entry, p);
            }
            DebugKind::Enum { members } => {
                let repr = held.unwrap_or(Ty::I64);
                set_size(dwarf, entry, repr.scalar_size().unwrap_or(8).into());
                let underlying = self.raw(dwarf, repr);
                set_type(dwarf, entry, underlying);
                for (n, v) in members {
                    let m = new_child(dwarf, entry, gimli::DW_TAG_enumerator, &n);
                    dwarf
                        .unit
                        .get_mut(m)
                        .set(gimli::DW_AT_const_value, AttributeValue::Sdata(v));
                }
            }
            DebugKind::Tagged { agg, variants } => {
                // The typedef names the type; the formatters recognize the structure's suffix.
                let typedef = entry;
                let entry = new_entry(
                    dwarf,
                    gimli::DW_TAG_structure_type,
                    &format!("{name} $tagged"),
                );
                set_type(dwarf, typedef, entry);
                let layout = self.program.agg(agg);
                set_size(dwarf, entry, layout.size.into());
                if let Some(&(tag_ty, _)) = layout.fields.first() {
                    let tag = self.raw(dwarf, tag_ty);
                    member(dwarf, entry, "tag", tag, 0);
                }
                let union = new_child(dwarf, entry, gimli::DW_TAG_union_type, "");
                set_size(dwarf, union, layout.size.into());
                for v in variants {
                    let view = new_entry(
                        dwarf,
                        gimli::DW_TAG_structure_type,
                        &format!("{name}::{}", v.name),
                    );
                    set_size(dwarf, view, self.program.agg(v.view).size.into());
                    self.fields(dwarf, view, v.view, &v.fields);
                    member(dwarf, union, &v.name, view, 0);
                }
                // An anonymous member at offset 0 holding the variants.
                let holder = dwarf.unit.add(entry, gimli::DW_TAG_member);
                let holder = dwarf.unit.get_mut(holder);
                holder.set(gimli::DW_AT_type, AttributeValue::UnitRef(union));
                holder.set(gimli::DW_AT_data_member_location, AttributeValue::Udata(0));
            }
            DebugKind::Option { inner, repr } => match repr {
                Ty::Agg(a) => {
                    // `{ some: bool, value: T }`, which the formatters recognize by its name.
                    let s = new_entry(
                        dwarf,
                        gimli::DW_TAG_structure_type,
                        &format!("{name} $option"),
                    );
                    set_size(dwarf, s, self.program.agg(a).size.into());
                    let flag = self.raw(dwarf, Ty::Bool);
                    member(dwarf, s, "some", flag, 0);
                    if let Some(&(vt, off)) = self.program.agg(a).fields.get(1) {
                        let v = self.ty(dwarf, inner, Some(vt));
                        member(dwarf, s, "value", v, off.into());
                    }
                    set_type(dwarf, entry, s);
                }
                Ty::Bool => {
                    let b = self.raw(dwarf, Ty::Bool);
                    set_type(dwarf, entry, b);
                }
                _ => {
                    let i = self.ty(dwarf, inner, Some(repr));
                    set_type(dwarf, entry, i);
                }
            },
            DebugKind::Shared { boxed, inner } => {
                let b = new_entry(dwarf, gimli::DW_TAG_structure_type, &format!("{name} box"));
                let layout = self.program.agg(boxed);
                set_size(dwarf, b, layout.size.into());
                let count = self.raw(dwarf, Ty::U64);
                member(dwarf, b, "count", count, 0);
                if let Some(&(vt, off)) = layout.fields.get(1) {
                    let v = self.ty(dwarf, inner, Some(vt));
                    member(dwarf, b, "value", v, off.into());
                }
                let p = pointer(dwarf, Some(b));
                set_type(dwarf, entry, p);
            }
            DebugKind::Opaque(repr) => match repr {
                Ty::Agg(a) => {
                    let layout = self.program.agg(a).clone();
                    set_size(dwarf, entry, layout.size.into());
                    for (i, (ft, off)) in layout.fields.into_iter().enumerate() {
                        let f = self.raw(dwarf, ft);
                        member(dwarf, entry, &format!("w{i}"), f, off.into());
                    }
                }
                Ty::Unit => set_size(dwarf, entry, 0),
                scalar => {
                    let size = scalar.scalar_size().unwrap_or(8);
                    set_size(dwarf, entry, size.into());
                    let f = self.raw(dwarf, scalar);
                    member(dwarf, entry, "w0", f, 0);
                }
            },
        }
        entry
    }

    /// Members for `fields` of aggregate `agg` at their layout offsets.
    fn fields(
        &mut self,
        dwarf: &mut DwarfUnit,
        parent: UnitEntryId,
        agg: AggId,
        fields: &[DebugField],
    ) {
        let layout = self.program.agg(agg);
        let slots: Vec<(Ty, u32)> = fields
            .iter()
            .map(|f| layout.fields[f.index as usize])
            .collect();
        for (f, (vt, off)) in fields.iter().zip(slots) {
            let t = self.ty(dwarf, f.ty, Some(vt));
            member(dwarf, parent, &f.name, t, off.into());
        }
    }

    /// A base type named `name` encoded as the scalar `ty`.
    fn base(&mut self, dwarf: &mut DwarfUnit, name: &str, ty: Ty) -> UnitEntryId {
        let (encoding, size): (DwAte, u32) = match ty {
            Ty::F32 | Ty::F64 => (gimli::DW_ATE_float, ty.scalar_size().unwrap_or(8)),
            Ty::Bool => (gimli::DW_ATE_boolean, 1),
            Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 => {
                (gimli::DW_ATE_signed, ty.scalar_size().unwrap_or(8))
            }
            _ => (gimli::DW_ATE_unsigned, ty.scalar_size().unwrap_or(8)),
        };
        let e = new_entry(dwarf, gimli::DW_TAG_base_type, name);
        let entry = dwarf.unit.get_mut(e);
        entry.set(gimli::DW_AT_encoding, AttributeValue::Encoding(encoding));
        entry.set(gimli::DW_AT_byte_size, AttributeValue::Udata(size.into()));
        e
    }

    /// The type of a raw VIR value: a scalar base type, an untyped pointer, or a structure of
    /// an aggregate's raw fields.
    fn raw(&mut self, dwarf: &mut DwarfUnit, ty: Ty) -> UnitEntryId {
        if let Some(&e) = self.raw.get(&ty) {
            return e;
        }
        let e = match ty {
            Ty::Ptr => pointer(dwarf, None),
            Ty::Agg(a) => {
                let layout = self.program.agg(a).clone();
                let e = new_entry(dwarf, gimli::DW_TAG_structure_type, &layout.name);
                set_size(dwarf, e, layout.size.into());
                for (i, (ft, off)) in layout.fields.into_iter().enumerate() {
                    let f = self.raw(dwarf, ft);
                    member(dwarf, e, &format!("w{i}"), f, off.into());
                }
                e
            }
            scalar => self.base(dwarf, &scalar.to_string(), scalar),
        };
        self.raw.insert(ty, e);
        e
    }
}

/// A new top-level entry (in the compile unit) with `tag` and, unless empty, `name`.
fn new_entry(dwarf: &mut DwarfUnit, tag: DwTag, name: &str) -> UnitEntryId {
    let root = dwarf.unit.root();
    new_child(dwarf, root, tag, name)
}

fn new_child(dwarf: &mut DwarfUnit, parent: UnitEntryId, tag: DwTag, name: &str) -> UnitEntryId {
    let e = dwarf.unit.add(parent, tag);
    if !name.is_empty() {
        let s = dwarf.strings.add(dwarf_str(name));
        dwarf
            .unit
            .get_mut(e)
            .set(gimli::DW_AT_name, AttributeValue::StringRef(s));
    }
    e
}

fn set_size(dwarf: &mut DwarfUnit, e: UnitEntryId, size: u64) {
    dwarf
        .unit
        .get_mut(e)
        .set(gimli::DW_AT_byte_size, AttributeValue::Udata(size));
}

fn set_type(dwarf: &mut DwarfUnit, e: UnitEntryId, ty: UnitEntryId) {
    dwarf
        .unit
        .get_mut(e)
        .set(gimli::DW_AT_type, AttributeValue::UnitRef(ty));
}

/// A pointer to `to` (`None`: an untyped pointer).
fn pointer(dwarf: &mut DwarfUnit, to: Option<UnitEntryId>) -> UnitEntryId {
    let root = dwarf.unit.root();
    let p = dwarf.unit.add(root, gimli::DW_TAG_pointer_type);
    set_size(dwarf, p, 8);
    if let Some(to) = to {
        set_type(dwarf, p, to);
    }
    p
}

fn member(dwarf: &mut DwarfUnit, parent: UnitEntryId, name: &str, ty: UnitEntryId, offset: u64) {
    let m = new_child(dwarf, parent, gimli::DW_TAG_member, name);
    let entry = dwarf.unit.get_mut(m);
    entry.set(gimli::DW_AT_type, AttributeValue::UnitRef(ty));
    entry.set(
        gimli::DW_AT_data_member_location,
        AttributeValue::Udata(offset),
    );
}
