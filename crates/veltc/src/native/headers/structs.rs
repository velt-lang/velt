//! Checking `declare c struct`s against their library's C headers (#2187 layer 4,
//! docs/internals/design/c-ffi.md "Checking declarations against C headers"): each struct a
//! library's declarations reach (as a pointer's pointee, an `Out<T>`'s `T`, or another reached
//! struct's field) must be a struct the headers define, under its tag or a typedef, with the same
//! fields in the same order, by name and by type through the table functions use; a nested
//! struct is compared by name. Its
//! size, alignment and every field's offset are clang's own answers, from a generated file whose
//! enumerators are `sizeof`, `_Alignof` and `__builtin_offsetof` (as `declare c const`s' values come), so
//! `#pragma pack`, `aligned(…)` and padding are compared, not assumed. Unions, bit-fields,
//! flexible array members, C arrays (until `T[N]`), `packed` structs and structs only declared
//! are refused, with the header line.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use velt_common::{Diagnostic, Diagnostics};
use velt_sema::hir::{self, Def, DefId, TyId, TyKind};

use super::compare::{self, Shape};
use super::ctype::Headers;
use super::shape::{field_shape, short_name};
use super::{Decl, LibraryHeaders};

/// What a `declare c struct` is, for the check: its fields in order and Velt's layout of it on the
/// 64-bit targets: fields in order at their natural alignment, as velt_vir lays it out (and checks
/// against C's rules, `check_c_struct_layout`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StructDecl {
    pub fields: Vec<(String, Shape)>,
    pub offsets: Vec<u32>,
    pub size: u32,
    pub align: u32,
}

/// Adds the C structs a value of type `t` reaches to `out`: `t` itself, a pointer's pointee, a
/// type argument (`Out<T>`'s), and a reached struct's fields.
pub(super) fn reached(p: &hir::Program, t: TyId, out: &mut BTreeSet<DefId>) {
    match p.types.kind(t) {
        TyKind::Ptr(a) | TyKind::Option(a, _) => reached(p, *a, out),
        TyKind::Adt(d, args) => {
            if let Def::Adt(a) = p.def(*d) {
                if a.c_struct {
                    if out.insert(*d) {
                        for f in &a.fields {
                            reached(p, f.ty, out);
                        }
                    }
                    return;
                }
            }
            for &a in args {
                reached(p, a, out);
            }
        }
        _ => {}
    }
}

/// The check's `Decl` of C struct `d`.
pub(super) fn decl(p: &hir::Program, d: DefId) -> Option<Decl> {
    let Def::Adt(a) = p.def(d) else { return None };
    let (offsets, size, align) = layout(p, a);
    let s = StructDecl {
        fields: (a.fields.iter())
            .map(|f| (f.name.clone(), field_shape(p, f.ty)))
            .collect(),
        offsets,
        size,
        align,
    };
    Some(Decl {
        name: short_name(&a.name),
        params: vec![],
        ret: Shape::Void,
        variadic: false,
        variable: None,
        structure: Some(Box::new(s)),
        span: a.span,
    })
}

/// Velt's layout of C struct `a`: fields in order at their natural alignment, the size rounded up
/// to the largest (each field's offset, the size, the alignment).
fn layout(p: &hir::Program, a: &hir::AdtDef) -> (Vec<u32>, u32, u32) {
    let (mut off, mut align, mut offsets) = (0u32, 1u32, vec![]);
    for f in &a.fields {
        let (s, al) = size_align(p, f.ty);
        off = off.next_multiple_of(al);
        offsets.push(off);
        off += s;
        align = align.max(al);
    }
    (offsets, off.max(1).next_multiple_of(align), align)
}

fn size_align(p: &hir::Program, t: TyId) -> (u32, u32) {
    match p.types.kind(t) {
        TyKind::Int(i) => (i.bits() / 8, i.bits() / 8),
        TyKind::Float(hir::FloatTy::F32) => (4, 4),
        TyKind::Bool => (1, 1),
        TyKind::Adt(d, _) => match p.def(*d) {
            Def::Adt(a) => {
                let (_, s, al) = layout(p, a);
                (s, al)
            }
            _ => (8, 8),
        },
        // `f64`, and pointers (`Ptr<T>`, `Ptr<T> | null`: one word).
        _ => (8, 8),
    }
}

/// Checks C struct `d` (whose `structure` is `s`) against the headers' definition: it exists,
/// is a struct C can share, and has the same fields. On success, how C spells its type
/// (`struct point`, `point_t`), for the layout probe.
pub(super) fn check_fields(
    d: &Decl,
    s: &StructDecl,
    h: &Headers,
    (headers, long_bits): (&[String], u32),
    diags: &mut Diagnostics,
) -> Option<String> {
    let name = &d.name;
    let refuse = |diags: &mut Diagnostics, message: String, note: &str| {
        diags.push(Diagnostic::error(message, d.span).with_note(note.to_string()));
        None
    };
    let Some((r, spelling)) = h.record(name) else {
        return refuse(
            diags,
            format!(
                "C struct `{name}` is not defined in its library's headers ({})",
                headers.join(", ")
            ),
            "a `declare c struct` names a struct the headers define, by its tag (`struct name`) or a typedef",
        );
    };
    let at = &r.at;
    if r.union {
        return refuse(
            diags,
            format!("C struct `{name}` is a union in {at}"),
            "C unions can't be declared yet: a union's fields share their memory",
        );
    }
    if !r.defined {
        return refuse(
            diags,
            format!("C struct `{name}` is only declared in {at}, never defined"),
            "C hides what is in it: declare it `declare c type` and use it behind a pointer",
        );
    }
    if r.packed {
        return refuse(
            diags,
            format!("C struct `{name}` is packed (`__attribute__((packed))`) in {at}"),
            "a packed struct's fields aren't at their alignment, which Velt's values always are",
        );
    }
    if r.fields.len() != s.fields.len() {
        return refuse(
            diags,
            format!(
                "C struct `{name}` has {} in {at}; the declaration has {}",
                fields(r.fields.len()),
                s.fields.len()
            ),
            "declare every field, in C's order",
        );
    }
    let before = diags.len();
    for (i, (c, (field, shape))) in r.fields.iter().zip(&s.fields).enumerate() {
        let what = format!("field `{field}` of C struct `{name}`");
        if c.bitfield {
            refuse(
                diags,
                format!("{what} is a bit-field in {at}"),
                "a bit-field shares its bytes with its neighbours: Velt can't declare one",
            );
            continue;
        }
        if c.name != *field {
            refuse(
                diags,
                match c.name.is_empty() {
                    true => format!(
                        "field {} of C struct `{name}` is an anonymous member in {at}, not the declaration's `{field}`",
                        i + 1
                    ),
                    false => format!(
                        "field {} of C struct `{name}` is `{}` in {at}, not the declaration's `{field}`",
                        i + 1,
                        c.name
                    ),
                },
                "declare every field by its C name, in C's order",
            );
            continue;
        }
        if let Some((_, rest)) = c.ty.split_once('[') {
            let note = match rest.trim_start().starts_with(']') {
                true => "a flexible array member: the struct has no fixed size",
                false => "a C array: until fixed-size arrays (`T[N]`), declare its elements as fields of their own",
            };
            refuse(diags, format!("{what} is `{}` in {at}", c.ty), note);
            continue;
        }
        if let Err(note) = compare::compare_in(shape, &h.resolve(&c.ty, long_bits), h) {
            refuse(
                diags,
                format!(
                    "{what} is `{}` in {at}, not the declaration's `{}`",
                    c.ty,
                    compare::describe(shape)
                ),
                &note,
            );
        }
    }
    (diags.len() == before).then_some(spelling)
}

/// `n` fields, in words.
fn fields(n: usize) -> String {
    match n {
        1 => "1 field".into(),
        n => format!("{n} fields"),
    }
}

/// The C file asking clang for the layout of `structs` (each `(spelling, fields)`): its size,
/// alignment and each field's offset, as enumerators.
fn probe_source(includes: &str, structs: &[(String, Vec<String>)]) -> String {
    let mut source = includes.to_string();
    for (i, (spelling, fields)) in structs.iter().enumerate() {
        source.push_str(&format!(
            "enum {{ __velt_z{i} = sizeof({spelling}), __velt_a{i} = _Alignof({spelling}) }};\n"
        ));
        for (j, f) in fields.iter().enumerate() {
            source.push_str(&format!(
                "enum {{ __velt_o{i}_{j} = __builtin_offsetof({spelling}, {f}) }};\n"
            ));
        }
    }
    source
}

/// Each enumerator of the probe, by name, with its value.
fn probe_values(json: &serde_json::Value) -> BTreeMap<String, i64> {
    let mut values = BTreeMap::new();
    for node in json["inner"].as_array().into_iter().flatten() {
        if node["kind"] != "EnumDecl" {
            continue;
        }
        for e in node["inner"].as_array().into_iter().flatten() {
            let Some(name) = e["name"].as_str().filter(|n| n.starts_with("__velt_")) else {
                continue;
            };
            if let Some(v) = super::probe::constant_expr(e) {
                values.insert(name.to_string(), v);
            }
        }
    }
    values
}

/// Asks clang (zig's, for `zig_target`, in `dir`) for the layout of `structs` in the headers of
/// the package at `root`, and compares each with Velt's; `target` names the target in messages.
#[allow(clippy::too_many_arguments)]
pub(super) fn check_layouts(
    zig: &Path,
    dir: &Path,
    root: &Path,
    read: LibraryHeaders,
    (zig_target, target): (&str, &str),
    structs: &[(&Decl, String)],
    diags: &mut Diagnostics,
) -> Result<(), String> {
    let asked: Vec<(String, Vec<String>)> = (structs.iter())
        .map(|(d, spelling)| {
            let fields = (d.structure.iter())
                .flat_map(|s| s.fields.iter().map(|(f, _)| f.clone()))
                .collect();
            (spelling.clone(), fields)
        })
        .collect();
    let includes = super::includes(root, read.headers)?;
    std::fs::write(dir.join(SOURCE), probe_source(&includes, &asked)).map_err(|e| e.to_string())?;
    let mut cc1 = super::cc1_command(zig, dir, root, SOURCE, read.defines, zig_target)?;
    cc1.push("-ast-dump=json".into());
    let out = super::zig_command(zig, dir)
        .args(&cc1)
        .output()
        .map_err(|e| format!("running {}: {e}", zig.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let errors: Vec<&str> = stderr
            .lines()
            .filter(|l| l.contains("error"))
            .take(3)
            .collect();
        return Err(format!(
            "clang can't lay the structs out: {}",
            errors.join("; ")
        ));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("clang's AST is not JSON: {e}"))?;
    let values = probe_values(&json);
    let value = |name: String| -> Result<u32, String> {
        let v = values
            .get(&name)
            .ok_or_else(|| format!("clang gave no `{name}`"))?;
        u32::try_from(*v).map_err(|_| format!("clang gave `{name}` = {v}"))
    };
    for (i, (d, spelling)) in structs.iter().enumerate() {
        let Some(s) = &d.structure else { continue };
        let (size, align) = (
            value(format!("__velt_z{i}"))?,
            value(format!("__velt_a{i}"))?,
        );
        let mut wrong = vec![];
        for (j, ((f, _), &mine)) in s.fields.iter().zip(&s.offsets).enumerate() {
            let c = value(format!("__velt_o{i}_{j}"))?;
            if c != mine {
                wrong.push(format!("`{f}` is at offset {c} in C and {mine} in Velt"));
            }
        }
        if size != s.size || align != s.align {
            wrong.push(format!(
                "C's `{spelling}` is {size} bytes aligned to {align}, Velt's {} aligned to {}",
                s.size, s.align
            ));
        }
        if !wrong.is_empty() {
            diags.push(
                Diagnostic::error(
                    format!(
                        "C struct `{}` is laid out differently in its headers on {target}: {}",
                        d.name,
                        wrong.join("; ")
                    ),
                    d.span,
                )
                .with_note(
                    "Velt lays a C struct's fields out in order at their natural alignment; the headers change that (`#pragma pack`, `aligned(…)`), which Velt can't declare",
                ),
            );
        }
    }
    Ok(())
}

/// The probe's file, in the check's directory.
const SOURCE: &str = "layouts.c";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_asks_for_size_alignment_and_each_offset() {
        let src = probe_source(
            "#include \"a.h\"\n",
            &[("struct point".into(), vec!["x".into(), "y".into()])],
        );
        assert_eq!(
            src,
            "#include \"a.h\"\n\
             enum { __velt_z0 = sizeof(struct point), __velt_a0 = _Alignof(struct point) };\n\
             enum { __velt_o0_0 = __builtin_offsetof(struct point, x) };\n\
             enum { __velt_o0_1 = __builtin_offsetof(struct point, y) };\n"
        );
    }
}
