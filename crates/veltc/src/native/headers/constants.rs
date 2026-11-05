//! `declare c const` (docs/internals/design/c-ffi.md "Declarations"): the value of a constant
//! comes from its package's C headers, per target, never from the binding. A package built here
//! asks clang (each library's headers in turn; a constant is one no matter which library); a
//! fetched one has the values in its `c-headers.json`, so users never run clang.
//!
//! clang evaluates each name (`probe`), so what a declared name may be is what C itself
//! evaluates: an object-like macro or an enumerator (or a `static const` clang folds). The files
//! clang read decide whether what it said still holds, and are the build's inputs; a fetched
//! package's are compared with those its record names.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use velt_common::{Diagnostic, Diagnostics, FileId, SourceMap, Span};
use velt_sema::hir::{self, CTy, CValue, Def};
use vpm::c_headers::{self, CConstant, CConstantValue, ConstantsRecord, Probe};
use vpm::graph::GraphPackage;
use vpm::PackageGraph;

use super::compare::{self, Scalar, Shape};
use super::ctype::{CType, Headers};
use super::probe;
use super::shape::shape;
use super::LibraryHeaders;

/// The constants that were read, per package root (what `c-headers.json` records for the target).
pub type Constants = BTreeMap<PathBuf, ConstantsRecord>;

/// One `declare c const`.
struct Decl {
    def: usize,
    name: String,
    shape: Shape,
    nullable: bool,
    span: Span,
}

impl Decl {
    /// The Velt type declared, as the record keeps it.
    fn declared(&self) -> String {
        let t = compare::describe(&self.shape);
        match self.nullable {
            true => format!("{t} | null"),
            false => t,
        }
    }
}

/// Set the value of each C constant of the program for `target`, from its package's headers or
/// record (`c_decls` having checked it may be declared at all). Errors go to `diags`. The files
/// the values came from go to `inputs`, as build-stamp `file` entries: an unchanged build isn't
/// checked again, and a changed header must change the program.
pub fn read(
    p: &mut hir::Program,
    sm: &SourceMap,
    graph: Option<&PackageGraph>,
    std_files: &HashSet<FileId>,
    target: &str,
    inputs: &mut Vec<String>,
    diags: &mut Diagnostics,
) -> Constants {
    let mut read = Constants::new();
    let mut groups: BTreeMap<PathBuf, (&GraphPackage, Vec<Decl>)> = BTreeMap::new();
    let mut checked = super::super::c_decls::Checked::default();
    for (def, d) in p.defs.iter().enumerate() {
        let Def::Global(g) = d else { continue };
        let Some(c) = g.c_const else { continue };
        let name = g.name.rsplit("::").next().unwrap_or(&g.name).to_string();
        let Some(package) = super::super::c_decls::check_const(
            &name,
            g.span,
            sm,
            graph,
            std_files,
            &mut checked,
            diags,
        ) else {
            continue;
        };
        let decl = Decl {
            def,
            name,
            shape: shape(p, g.ty, c.ty),
            nullable: matches!(c.ty, CTy::Ptr { nullable: true }),
            span: g.span,
        };
        groups
            .entry(package.root.clone())
            .or_insert_with(|| (package, Vec::new()))
            .1
            .push(decl);
    }
    let Some(zig_target) = super::zig_target(target) else {
        return read;
    };
    let mut values = Vec::new();
    for (root, (package, decls)) in groups {
        let (of_package, headers) = match package.fetched {
            true => from_record(package, &decls, zig_target, diags),
            false => from_headers(package, &decls, zig_target, target, diags),
        };
        if package.fetched {
            inputs.push(stamp_entry(&root.join(c_headers::FILE)));
        }
        // A header the package doesn't have (a registry archive ships none) is no input.
        let files = headers.keys().map(|h| root.join(h));
        inputs.extend(files.filter(|f| f.is_file()).map(|f| stamp_entry(&f)));
        if of_package.is_empty() {
            continue;
        }
        let record = read.entry(root.clone()).or_default();
        record.headers = headers;
        for (i, constant) in of_package {
            let d = &decls[i];
            if let Some(v) = to_hir(&constant.value) {
                values.push((d.def, v));
            }
            record.values.insert(d.name.clone(), constant);
        }
    }
    for (def, value) in values {
        if let Def::Global(hir::GlobalDef {
            c_const: Some(c), ..
        }) = &mut p.defs[def]
        {
            c.value = Some(value);
        }
    }
    read
}

/// A build-stamp `file` entry for `file`: without a time, so it is hashed each time.
pub(super) fn stamp_entry(file: &Path) -> String {
    let bytes = std::fs::read(file).unwrap_or_default();
    format!(
        "file {}\t{}",
        vpm::probe::content_hash(&bytes),
        vpm::relpath::absolute(file).display()
    )
}

/// A recorded value as lowering reads it; `None` when it is malformed.
fn to_hir(v: &CConstantValue) -> Option<CValue> {
    Some(match v {
        CConstantValue::Int(s) => CValue::Int(s.parse().ok()?),
        CConstantValue::Float(s) => CValue::Float(f64::from_bits(u64::from_str_radix(s, 16).ok()?)),
        CConstantValue::Bool(b) => CValue::Bool(*b),
        CConstantValue::Pointer(s) => CValue::Address(u64::from_str_radix(s, 16).ok()?),
    })
}

/// A package's own headers some values were read from: `/`-separated path in it → digest.
type HeaderDigests = BTreeMap<String, String>;

/// The headers `package`'s C libraries list.
fn listed(package: &GraphPackage) -> Vec<String> {
    let libraries = package.c_libraries.values();
    libraries.flat_map(|l| l.headers.clone()).collect()
}

/// The values of `decls` in the record of fetched `package`, by index in `decls`, and the
/// headers the record says they were read from, which are compared with the package's.
fn from_record(
    package: &GraphPackage,
    decls: &[Decl],
    zig_target: &str,
    diags: &mut Diagnostics,
) -> (Vec<(usize, CConstant)>, HeaderDigests) {
    let name = &package.name;
    let record = match c_headers::CHeaders::read(&package.root) {
        Ok(Some(record)) => record,
        Ok(None) => {
            diags.push(
                Diagnostic::error(
                    format!(
                        "package `{name}` declares C constants, but was published without a record of their values ({})",
                        c_headers::FILE
                    ),
                    decls[0].span,
                )
                .with_note("its author makes the record with `velt native headers`, which `velt publish` runs; ask for a version that has it"),
            );
            return Default::default();
        }
        Err(e) => {
            diags.push(Diagnostic::error(e, decls[0].span));
            return Default::default();
        }
    };
    let headers = match record.constants.get(zig_target) {
        Some(r) => {
            let of = format!("of package `{name}`'s C constants");
            let at = decls[0].span;
            let listed = listed(package);
            super::record::compare_headers(
                package, &of, &r.headers, &listed, zig_target, at, diags,
            );
            r.headers.clone()
        }
        None => HeaderDigests::new(),
    };
    let long_bits = if zig_target.contains("windows") {
        32
    } else {
        64
    };
    let mut out = Vec::new();
    for (i, d) in decls.iter().enumerate() {
        match record.constant(zig_target, &d.name) {
            Some(c) if c.declared == d.declared() && holds(d, c, long_bits) => {
                out.push((i, c.clone()))
            }
            Some(c) => diags.push(
                Diagnostic::error(
                    format!(
                        "C constant `{}` of package `{name}` is not the one its author checked against its headers for {zig_target}",
                        d.name
                    ),
                    d.span,
                )
                .with_note(format!(
                    "{} records `{}`: the package changed after the check",
                    c_headers::FILE, c.declared
                )),
            ),
            None => diags.push(
                Diagnostic::error(
                    format!(
                        "package `{name}` has no value of C constant `{}` for {zig_target}",
                        d.name
                    ),
                    d.span,
                )
                .with_note(format!(
                    "{} has no entry for it: its author runs `velt native headers` to read every target's",
                    c_headers::FILE
                )),
            ),
        }
    }
    (out, headers)
}

/// Whether recorded constant `c` is one `d` can be: a record is trusted as its author's check,
/// but a value its type can't hold would break the type, or the compiler.
fn holds(d: &Decl, c: &CConstant, long_bits: u32) -> bool {
    let c_type = Headers::default().resolve(&c.c_type, long_bits);
    to_hir(&c.value).is_some() && fit(&d.shape, d.nullable, &c_type, &c.value).is_ok()
}

/// The values of `decls` in the headers of `package`'s C libraries, by index in `decls`, and
/// the package's own headers clang read for them.
fn from_headers(
    package: &GraphPackage,
    decls: &[Decl],
    zig_target: &str,
    target: &str,
    diags: &mut Diagnostics,
) -> (Vec<(usize, CConstant)>, HeaderDigests) {
    let libraries: Vec<(&String, LibraryHeaders)> = package
        .c_libraries
        .iter()
        .filter(|(_, lib)| !lib.headers.is_empty())
        .map(|(name, lib)| {
            let headers = lib.headers.as_slice();
            let defines = lib.defines.as_slice();
            (name, LibraryHeaders { headers, defines })
        })
        .collect();
    if libraries.is_empty() {
        diags.push(
            Diagnostic::error(
                format!(
                    "C constant `{}` takes its value from C headers, but package `{}` lists none",
                    decls[0].name, package.name
                ),
                decls[0].span,
            )
            .with_note("list them: `native: { c: { <library>: { headers: [\"native/<library>.h\"] } } }`; a library that says `unchecked: true` has none to read"),
        );
        return Default::default();
    }
    let names: Vec<&str> = decls.iter().map(|d| d.name.as_str()).collect();
    let long_bits = if target.contains("windows") { 32 } else { 64 };
    let mut found: BTreeMap<&str, (&str, Probe)> = BTreeMap::new();
    let mut failed = false;
    let mut read = Vec::new();
    for (library, read_with) in &libraries {
        let probes = match probes_of(&package.root, *read_with, &names, zig_target, long_bits) {
            Ok((probes, files)) => {
                read.extend(files);
                probes
            }
            Err(e) => {
                diags.push(
                    Diagnostic::error(
                        format!(
                            "the C constants of package `{}` can't be read from the headers of C library `{library}`: {e}",
                            package.name
                        ),
                        decls[0].span,
                    ),
                );
                failed = true;
                continue;
            }
        };
        for d in decls {
            let Some(probe) = probes.get(&d.name) else {
                continue;
            };
            match found.get(d.name.as_str()) {
                Some((other, seen)) if seen.value != probe.value => diags.push(
                    Diagnostic::error(
                        format!(
                            "C constant `{}` has different values in the headers of C libraries `{other}` and `{library}`",
                            d.name
                        ),
                        d.span,
                    ),
                ),
                Some(_) => {}
                None => {
                    found.insert(&d.name, (library, probe.clone()));
                }
            }
        }
    }
    let listed: Vec<&str> = libraries
        .iter()
        .flat_map(|(_, h)| h.headers.iter().map(String::as_str))
        .collect();
    let mut out = Vec::new();
    for (i, d) in decls.iter().enumerate() {
        let Some((_, probe)) = found.get(d.name.as_str()) else {
            if !failed {
                diags.push(
                    Diagnostic::error(
                        format!(
                            "C constant `{}` is not defined in its package's headers ({})",
                            d.name,
                            listed.join(", ")
                        ),
                        d.span,
                    )
                    .with_note("a C constant is an object-like macro or an enumerator"),
                );
            }
            continue;
        };
        match value_for(d, probe, long_bits) {
            Ok(value) => out.push((
                i,
                CConstant {
                    declared: d.declared(),
                    c_type: probe.c_type.clone(),
                    value,
                },
            )),
            Err(e) => diags.push(e),
        }
    }
    (out, super::record::package_headers(&package.root, &read))
}

/// The value of declaration `d`, which clang says is `probe`.
fn value_for(d: &Decl, probe: &Probe, long_bits: u32) -> Result<CConstantValue, Diagnostic> {
    let Some(value) = &probe.value else {
        return Err(Diagnostic::error(
            format!(
                "C constant `{}` has no value a program can use: C gives it type `{}`, and clang can't evaluate it as a number or an address",
                d.name, probe.c_type
            ),
            d.span,
        )
        .with_note("a C constant is an object-like macro or an enumerator, of a number or pointer type"));
    };
    let c = Headers::default().resolve(&probe.c_type, long_bits);
    fit(&d.shape, d.nullable, &c, value).map_err(|note| {
        Diagnostic::error(
            format!(
                "C constant `{}` is `{}` of type `{}` in its headers, not the declaration's `{}`",
                d.name,
                shown(value),
                probe.c_type,
                d.declared()
            ),
            d.span,
        )
        .with_note(note)
    })?;
    Ok(value.clone())
}

/// A value for messages.
fn shown(v: &CConstantValue) -> String {
    match v {
        CConstantValue::Int(s) => s.clone(),
        CConstantValue::Float(s) => u64::from_str_radix(s, 16)
            .map(|b| f64::from_bits(b).to_string())
            .unwrap_or_else(|_| s.clone()),
        CConstantValue::Bool(b) => b.to_string(),
        CConstantValue::Pointer(s) => format!("0x{s}"),
    }
}

/// Whether a constant of C type `c` and value `v` may be declared `shape` (`| null` when
/// `nullable`): its value decides for integers, so a flag `#define`d as `int` can be a `u32`;
/// `Err(note)` says what to declare.
pub(super) fn fit(
    shape: &Shape,
    nullable: bool,
    c: &CType,
    v: &CConstantValue,
) -> Result<(), String> {
    let declare = || format!("declare {}", compare::velt_for(c));
    match (shape, v) {
        (Shape::Scalar(Scalar::Int { bits, signed, .. }), CConstantValue::Int(text)) => {
            let value: i128 = text.parse().map_err(|_| "its value is malformed")?;
            let (min, max) = match signed {
                true => (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1),
                false => (0, (1i128 << bits) - 1),
            };
            match (min..=max).contains(&value) {
                true => Ok(()),
                false => Err(format!(
                    "that type can't hold {value}: declare a wider or differently signed integer"
                )),
            }
        }
        (Shape::Scalar(Scalar::Bool), CConstantValue::Bool(_))
        | (Shape::Scalar(Scalar::F64), CConstantValue::Float(_)) => Ok(()),
        (Shape::Scalar(Scalar::F32), CConstantValue::Float(_)) if *c == CType::Float => Ok(()),
        (Shape::Ptr(_), CConstantValue::Pointer(address)) => {
            let null = u64::from_str_radix(address, 16) == Ok(0);
            if null && !nullable {
                return Err(format!(
                    "it is NULL: declare `{} | null`",
                    compare::describe(shape)
                ));
            }
            compare::compare(shape, c)
        }
        _ => Err(declare()),
    }
}

/// What [`probes_of`] remembers: what clang said, and the files it read, with their digests.
type Remembered = (BTreeMap<String, Probe>, Vec<(PathBuf, String)>);

/// What clang says about `names` in a library's headers, read with its `defines` as its own
/// sources are, and the files it read, remembered like a check's result (`still_holds`): while
/// none of those files change, a target's C headers from zig among them.
fn probes_of(
    root: &Path,
    read_with: LibraryHeaders,
    names: &[&str],
    zig_target: &str,
    long_bits: u32,
) -> Result<Remembered, String> {
    let home = vpm::locations::velt_home()?;
    let mut h = Sha256::new();
    for (name, value) in read_with.defines {
        h.update(format!("{name}={value}\0"));
    }
    h.update(velt_toolchain::zig::VERSION);
    h.update([0]);
    h.update(zig_target);
    h.update([0]);
    h.update(root.to_string_lossy().as_bytes());
    for header in read_with.headers {
        h.update([0]);
        h.update(header);
    }
    h.update([1]);
    for name in names {
        h.update([0]);
        h.update(name);
    }
    let key: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let remembered = home
        .join("cache")
        .join("headers")
        .join(format!("constants-{key}.json"));
    let known = std::fs::read(&remembered)
        .ok()
        .and_then(|b| serde_json::from_slice::<Remembered>(&b).ok());
    if let Some(known) = known.filter(|(_, read)| holds_still(read)) {
        return Ok(known);
    }
    let zig = velt_toolchain::zig::zig(&home)?;
    let dir = super::work_dir(&home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let result = probe::run_clang(&zig, &dir, root, read_with, names, zig_target, long_bits);
    let _ = std::fs::remove_dir_all(&dir);
    let (probes, files) = result?;
    let read = files
        .into_iter()
        .map(|file| Some((file.clone(), super::file_digest(&file)?)))
        .collect::<Option<Vec<_>>>()
        .ok_or("the headers changed while clang read them")?;
    // A failure to write it costs a run next time; written whole, so a torn write can't leave a
    // list of fewer files, which would still hold.
    let known = (probes, read);
    if let Ok(text) = serde_json::to_string(&known) {
        let _ = vpm::json_file::write_atomic(&remembered, &text);
    }
    Ok(known)
}

/// Whether every file of `read` still has its digest (and there is one).
fn holds_still(read: &[(PathBuf, String)]) -> bool {
    !read.is_empty()
        && read
            .iter()
            .all(|(file, digest)| super::file_digest(file).as_deref() == Some(digest))
}

#[cfg(test)]
mod tests {
    use super::super::compare::Pointee;
    use super::*;

    fn int(bits: u32, signed: bool) -> Shape {
        Shape::Scalar(Scalar::Int {
            bits,
            signed,
            pointer_sized: false,
        })
    }

    fn c_int() -> CType {
        CType::Int {
            bits: 32,
            signed: true,
            names: vec![],
            plain_char: false,
        }
    }

    fn v(n: i128) -> CConstantValue {
        CConstantValue::Int(n.to_string())
    }

    #[test]
    fn an_integer_fits_by_its_value() {
        assert!(fit(&int(32, false), false, &c_int(), &v(5)).is_ok());
        assert!(fit(&int(8, true), false, &c_int(), &v(127)).is_ok());
        assert!(fit(&int(8, true), false, &c_int(), &v(128)).is_err());
        assert!(fit(&int(8, true), false, &c_int(), &v(-128)).is_ok());
        assert!(fit(&int(8, true), false, &c_int(), &v(-129)).is_err());
        assert!(fit(&int(32, false), false, &c_int(), &v(-1)).is_err());
        assert!(fit(&int(64, false), false, &c_int(), &v(u64::MAX as i128)).is_ok());
        assert!(fit(&int(64, true), false, &c_int(), &v(u64::MAX as i128)).is_err());
    }

    #[test]
    fn kinds_do_not_convert() {
        let float = CConstantValue::Float(format!("{:x}", 1.5f64.to_bits()));
        let f64_shape = Shape::Scalar(Scalar::F64);
        let f32_shape = Shape::Scalar(Scalar::F32);
        assert!(fit(&f64_shape, false, &CType::Double, &float).is_ok());
        assert!(fit(&f64_shape, false, &CType::Float, &float).is_ok());
        assert!(fit(&f32_shape, false, &CType::Float, &float).is_ok());
        assert_eq!(
            fit(&f32_shape, false, &CType::Double, &float).unwrap_err(),
            "declare `f64`"
        );
        assert_eq!(
            fit(&int(32, true), false, &CType::Double, &float).unwrap_err(),
            "declare `f64`"
        );
        assert_eq!(
            fit(&f64_shape, false, &c_int(), &v(1)).unwrap_err(),
            "declare `i32`"
        );
        let flag = CConstantValue::Bool(true);
        let bool_shape = Shape::Scalar(Scalar::Bool);
        assert!(fit(&bool_shape, false, &CType::Bool, &flag).is_ok());
        assert!(fit(&bool_shape, false, &c_int(), &v(1)).is_err());
    }

    #[test]
    fn a_pointer_matches_its_type_and_null_needs_null() {
        let void_ptr = Shape::Ptr(Pointee::Void);
        let thing = Shape::Ptr(Pointee::CType("thing".into()));
        let c_thing = CType::Pointer {
            pointee: Box::new(CType::Named("thing".into())),
            is_const: false,
            pointee_names: vec![],
        };
        let at = CConstantValue::Pointer("ffffffffffffffff".into());
        let null = CConstantValue::Pointer("0".into());
        assert!(fit(&void_ptr, false, &CType::FunctionPointer, &at).is_ok());
        assert!(fit(&thing, false, &CType::FunctionPointer, &at).is_err());
        assert!(fit(&thing, false, &c_thing, &at).is_ok());
        let other = Shape::Ptr(Pointee::CType("other".into()));
        assert!(fit(&other, false, &c_thing, &at).is_err());
        assert_eq!(
            fit(&thing, false, &c_thing, &null).unwrap_err(),
            "it is NULL: declare `Ptr<thing> | null`"
        );
        assert!(fit(&thing, true, &c_thing, &null).is_ok());
        assert!(fit(&int(64, false), false, &c_thing, &at).is_err());
    }

    /// A record's value is trusted as its author's check only when its type can hold it.
    #[test]
    fn a_recorded_value_its_type_can_t_hold_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let mut graph = PackageGraph::default();
        graph.add("bind", tmp.path(), BTreeMap::new()).fetched = true;
        let package = &graph.packages()[0];
        let target = "x86_64-linux-gnu";
        let float = CConstantValue::Float(format!("{:x}", 1.5f64.to_bits()));
        let pointer = |a: &str| CConstantValue::Pointer(a.into());
        let void_ptr = Shape::Ptr(Pointee::Void);
        let bool_shape = Shape::Scalar(Scalar::Bool);
        // (declared, nullable, C type, value, whether it is accepted)
        let cases = [
            (int(32, true), false, "int", v(7), true),
            (void_ptr.clone(), true, "void *", pointer("0"), true),
            (int(32, true), false, "int", pointer("10"), false),
            (int(32, true), false, "int", float.clone(), false),
            (
                int(32, true),
                false,
                "int",
                CConstantValue::Bool(true),
                false,
            ),
            (Shape::Scalar(Scalar::F64), false, "double", v(1), false),
            (void_ptr.clone(), false, "void *", v(16), false),
            (int(32, true), false, "int", v(5_000_000_000), false),
            (void_ptr, false, "void *", pointer("0"), false),
            (bool_shape, false, "_Bool", v(1), false),
        ];
        for (n, (shape, nullable, c_type, value, accepted)) in cases.into_iter().enumerate() {
            let d = Decl {
                def: 0,
                name: "X".into(),
                shape,
                nullable,
                span: Span::DUMMY,
            };
            let mut record = c_headers::CHeaders::default();
            let constant = CConstant {
                declared: d.declared(),
                c_type: c_type.into(),
                value,
            };
            let values = &mut record.constants.entry(target.into()).or_default().values;
            values.insert("X".into(), constant);
            record.write(tmp.path()).unwrap();
            let mut diags = Diagnostics::default();
            let (out, _) = from_record(package, std::slice::from_ref(&d), target, &mut diags);
            let messages: Vec<_> = diags.iter().map(|d| d.message.clone()).collect();
            match accepted {
                true => assert!(out.len() == 1 && messages.is_empty(), "{n}: {messages:?}"),
                false => {
                    assert!(out.is_empty(), "case {n}");
                    assert_eq!(messages, ["C constant `X` of package `bind` is not the one its author checked against its headers for x86_64-linux-gnu"], "case {n}");
                }
            }
        }
    }

    #[test]
    fn recorded_values_decode() {
        assert_eq!(
            to_hir(&CConstantValue::Int("-7".into())),
            Some(CValue::Int(-7))
        );
        assert_eq!(
            to_hir(&CConstantValue::Pointer("ffffffffffffffff".into())),
            Some(CValue::Address(u64::MAX))
        );
        assert_eq!(
            to_hir(&CConstantValue::Float(format!("{:x}", 3.25f64.to_bits()))),
            Some(CValue::Float(3.25))
        );
        assert_eq!(to_hir(&CConstantValue::Int("x".into())), None);
        assert_eq!(to_hir(&CConstantValue::Pointer("zz".into())), None);
    }
}
