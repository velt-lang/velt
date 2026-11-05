//! `declare c const` (docs/internals/design/c-ffi.md "Declarations"): the value of a constant
//! comes from its package's C headers, per target, never from the binding. A package built here
//! asks clang (each library's headers in turn; a constant is one no matter which library); a
//! fetched one has the values in its `c-headers.json`, so users never run clang.
//!
//! clang evaluates each name in a file of its own making: `__typeof__(NAME)` says its C type and
//! that it exists, and an enumerator initialized from it holds its value, which the JSON dump
//! prints. A float can't initialize an enumerator exactly: those are read from the LLVM IR of a
//! constant initialized from them, in a second run. So what a declared name may be is what C
//! itself evaluates: an object-like macro or an enumerator (or a `static const` clang folds).

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use velt_common::{Diagnostic, Diagnostics, FileId, SourceMap, Span};
use velt_sema::hir::{self, CTy, CValue, Def};
use vpm::c_headers::{self, CConstant, CConstantValue, Probe};
use vpm::graph::GraphPackage;
use vpm::PackageGraph;

use super::compare::{self, Scalar, Shape};
use super::ctype::{CType, Headers};
use super::shape::shape;

/// The constants that were read, per package root (what `c-headers.json` records for the target).
pub type Constants = BTreeMap<PathBuf, BTreeMap<String, CConstant>>;

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
        inputs.extend(input_files(package).iter().map(|f| stamp_entry(f)));
        let of_package = match package.fetched {
            true => from_record(package, &decls, zig_target, diags),
            false => from_headers(package, &decls, zig_target, target, diags),
        };
        for (i, value) in of_package {
            let d = &decls[i];
            if let Some(v) = to_hir(&value) {
                values.push((d.def, v));
            }
            read.entry(root.clone()).or_default().insert(
                d.name.clone(),
                CConstant {
                    declared: d.declared(),
                    value,
                },
            );
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

/// The files `package`'s constants come from.
fn input_files(package: &GraphPackage) -> Vec<PathBuf> {
    if package.fetched {
        return vec![package.root.join(c_headers::FILE)];
    }
    let headers = package.c_libraries.values().flat_map(|l| l.headers.clone());
    super::own_header_files(&package.root, &headers.collect::<Vec<_>>())
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

/// The values of `decls` in the record of fetched `package`, by index in `decls`.
fn from_record(
    package: &GraphPackage,
    decls: &[Decl],
    zig_target: &str,
    diags: &mut Diagnostics,
) -> Vec<(usize, CConstantValue)> {
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
            return Vec::new();
        }
        Err(e) => {
            diags.push(Diagnostic::error(e, decls[0].span));
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for (i, d) in decls.iter().enumerate() {
        match record.constant(zig_target, &d.name) {
            Some(c) if c.declared == d.declared() && to_hir(&c.value).is_some() => {
                out.push((i, c.value.clone()))
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
    out
}

/// The values of `decls` in the headers of `package`'s C libraries, by index in `decls`.
fn from_headers(
    package: &GraphPackage,
    decls: &[Decl],
    zig_target: &str,
    target: &str,
    diags: &mut Diagnostics,
) -> Vec<(usize, CConstantValue)> {
    let libraries: Vec<(&String, &[String])> = package
        .c_libraries
        .iter()
        .filter(|(_, lib)| !lib.headers.is_empty())
        .map(|(name, lib)| (name, lib.headers.as_slice()))
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
        return Vec::new();
    }
    let names: Vec<&str> = decls.iter().map(|d| d.name.as_str()).collect();
    let long_bits = if target.contains("windows") { 32 } else { 64 };
    let mut found: BTreeMap<&str, (&str, Probe)> = BTreeMap::new();
    let mut failed = false;
    for (library, headers) in &libraries {
        let probes = match probes_of(&package.root, headers, &names, zig_target, long_bits) {
            Ok(probes) => probes,
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
        .flat_map(|(_, h)| h.iter().map(String::as_str))
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
            Ok(value) => out.push((i, value)),
            Err(e) => diags.push(e),
        }
    }
    out
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

/// What clang says about `names` in `headers`, remembered like a check's result: the macros of
/// a package's headers are what a value depends on, and a target's C headers come with zig.
fn probes_of(
    root: &Path,
    headers: &[String],
    names: &[&str],
    zig_target: &str,
    long_bits: u32,
) -> Result<BTreeMap<String, Probe>, String> {
    let home = vpm::locations::velt_home()?;
    let mut h = Sha256::new();
    h.update(velt_toolchain::zig::VERSION);
    h.update([0]);
    h.update(zig_target);
    super::hash_headers(&mut h, root, headers);
    for name in names {
        h.update([0]);
        h.update(name);
    }
    let key: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let remembered = home
        .join("cache")
        .join("headers")
        .join(format!("constants-{key}.json"));
    if let Some(known) = std::fs::read(&remembered)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
    {
        return Ok(known);
    }
    let zig = velt_toolchain::zig::zig(&home)?;
    let dir = super::work_dir(&home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let result = run_clang(&zig, &dir, root, headers, names, zig_target, long_bits);
    let _ = std::fs::remove_dir_all(&dir);
    let probes = result?;
    // A failure to write it costs a run next time.
    if let Ok(text) = serde_json::to_vec(&probes) {
        let _ = std::fs::write(&remembered, text);
    }
    Ok(probes)
}

const SOURCE: &str = "constants.c";

/// The C file that asks about `names`: two lines each after `includes`.
fn probe_source(includes: &str, names: &[&str]) -> String {
    let mut source = includes.to_string();
    for (i, name) in names.iter().enumerate() {
        source.push_str(&format!("__typeof__({name}) __velt_t{i};\n"));
        source.push_str(&format!(
            "enum {{ __velt_v{i} = (__INTMAX_TYPE__)({name}) }};\n"
        ));
    }
    source
}

#[allow(clippy::too_many_arguments)]
fn run_clang(
    zig: &Path,
    dir: &Path,
    root: &Path,
    headers: &[String],
    names: &[&str],
    zig_target: &str,
    long_bits: u32,
) -> Result<BTreeMap<String, Probe>, String> {
    let includes = super::includes(root, headers)?;
    let file = dir.join(SOURCE);
    std::fs::write(&file, probe_source(&includes, names)).map_err(|e| e.to_string())?;
    let mut cc1 = super::cc1_command(zig, dir, root, SOURCE, zig_target)?;
    cc1.extend(["-ferror-limit".into(), "0".into(), "-ast-dump=json".into()]);
    let out = super::zig_command(zig, dir)
        .args(&cc1)
        .output()
        .map_err(|e| format!("running {}: {e}", zig.display()))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let wrong = probe_errors(&stderr, headers.len(), names.len())?;
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("clang's AST is not JSON: {e}"))?;
    let (types, values) = read_dump(&json);
    let mut probes = BTreeMap::new();
    let mut floats = Vec::new();
    for (i, name) in names.iter().enumerate() {
        if wrong.undefined.contains(&i) {
            continue;
        }
        let Some(c_type) = types.get(&i) else {
            continue;
        };
        let value = match wrong.not_constant.contains(&i) {
            true => None,
            false => values.get(&i).copied(),
        };
        let c = Headers::default().resolve(c_type, long_bits);
        if matches!(c, CType::Float | CType::Double) && !c_type.contains('[') {
            floats.push((i, *name));
        }
        let value = classify(c_type, &c, value);
        probes.insert(
            name.to_string(),
            Probe {
                c_type: c_type.clone(),
                value,
            },
        );
    }
    if !floats.is_empty() {
        let mut source = includes;
        for (i, name) in &floats {
            source.push_str(&format!(
                "const __typeof__({name}) __velt_f{i} = ({name});\n"
            ));
        }
        std::fs::write(&file, source).map_err(|e| e.to_string())?;
        let mut cc1 = super::cc1_command(zig, dir, root, SOURCE, zig_target)?;
        cc1.extend(["-emit-llvm".into(), "-o".into(), "-".into()]);
        let out = super::zig_command(zig, dir)
            .args(&cc1)
            .output()
            .map_err(|e| format!("running {}: {e}", zig.display()))?;
        let ir = String::from_utf8_lossy(&out.stdout);
        for (i, name) in &floats {
            if let (Some(bits), Some(probe)) = (float_in_ir(&ir, *i), probes.get_mut(*name)) {
                probe.value = Some(CConstantValue::Float(format!("{bits:x}")));
            }
        }
    }
    Ok(probes)
}

/// The value clang evaluated, as what it crosses as; `None` for a type no constant crosses as.
fn classify(c_type: &str, c: &CType, value: Option<i64>) -> Option<CConstantValue> {
    // An array (a string literal) reads as a pointer in `resolve`, but no value is its address.
    if c_type.contains('[') {
        return None;
    }
    let value = value?;
    Some(match c {
        CType::Int { signed: false, .. } => CConstantValue::Int((value as u64).to_string()),
        CType::Int { .. } | CType::Enum { .. } => CConstantValue::Int(value.to_string()),
        CType::Bool => CConstantValue::Bool(value != 0),
        CType::Pointer { .. } | CType::FunctionPointer => {
            CConstantValue::Pointer(format!("{:x}", value as u64))
        }
        _ => return None,
    })
}

/// Which of the probed names clang refused.
#[derive(Default, Debug, PartialEq, Eq)]
struct ProbeErrors {
    /// `__typeof__(NAME)` failed: not declared.
    undefined: HashSet<usize>,
    /// Its value is no constant.
    not_constant: HashSet<usize>,
}

/// The errors of a run on [`probe_source`] for `headers` includes and `names` names. An error
/// anywhere else is that the headers themselves don't parse.
fn probe_errors(stderr: &str, headers: usize, names: usize) -> Result<ProbeErrors, String> {
    let mut wrong = ProbeErrors::default();
    let mut other = Vec::new();
    for line in stderr.lines() {
        if !line.contains(" error: ") {
            continue;
        }
        let at = line
            .strip_prefix(SOURCE)
            .and_then(|l| l.strip_prefix(':'))
            .and_then(|l| l.split(':').next())
            .and_then(|l| l.parse::<usize>().ok())
            .filter(|&l| l > headers && l <= headers + 2 * names);
        match at {
            Some(l) => {
                let k = l - headers - 1;
                match k.is_multiple_of(2) {
                    true => wrong.undefined.insert(k / 2),
                    false => wrong.not_constant.insert(k / 2),
                };
            }
            None => other.push(line),
        }
    }
    if other.is_empty() {
        return Ok(wrong);
    }
    other.truncate(3);
    Err(format!("clang can't parse them: {}", other.join("; ")))
}

/// In clang's dump: each probe's C type and value, by its number.
fn read_dump(json: &serde_json::Value) -> (BTreeMap<usize, String>, BTreeMap<usize, i64>) {
    let mut types = BTreeMap::new();
    let mut values = BTreeMap::new();
    for node in json["inner"].as_array().into_iter().flatten() {
        match node["kind"].as_str() {
            Some("VarDecl") => {
                let Some(i) = number_of(node, "__velt_t") else {
                    continue;
                };
                let ty = &node["type"];
                let written = ty["desugaredQualType"].as_str().or(ty["qualType"].as_str());
                if let Some(written) = written {
                    types.insert(i, written.to_string());
                }
            }
            Some("EnumDecl") => {
                for e in node["inner"].as_array().into_iter().flatten() {
                    let Some(i) = number_of(e, "__velt_v") else {
                        continue;
                    };
                    if let Some(v) = constant_expr(e) {
                        values.insert(i, v);
                    }
                }
            }
            _ => {}
        }
    }
    (types, values)
}

fn number_of(node: &serde_json::Value, prefix: &str) -> Option<usize> {
    node["name"].as_str()?.strip_prefix(prefix)?.parse().ok()
}

/// The value of the `ConstantExpr` under `node`.
fn constant_expr(node: &serde_json::Value) -> Option<i64> {
    if node["kind"] == "ConstantExpr" {
        return node["value"].as_str()?.parse().ok();
    }
    node["inner"].as_array()?.iter().find_map(constant_expr)
}

/// The value of float probe `i` in LLVM IR, as `f64::to_bits`: `@__velt_f0 = constant double
/// 3.25e+00, align 8`. A `float` is printed as the `double` of its value.
fn float_in_ir(ir: &str, i: usize) -> Option<u64> {
    let head = format!("@__velt_f{i} = ");
    let line = ir.lines().find(|l| l.starts_with(&head))?;
    let (_, rest) = line.split_once(" constant ")?;
    let mut words = rest.split_whitespace();
    let ty = words.next()?;
    if ty != "double" && ty != "float" {
        return None;
    }
    let text = words.next()?.trim_end_matches(',');
    match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse::<f64>().ok().map(f64::to_bits),
    }
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

    #[test]
    fn a_probe_s_errors_name_its_lines() {
        let stderr = "constants.c:2:12: error: use of undeclared identifier 'NOPE'\nconstants.c:5:20: error: expression is not an integer constant expression\n./own.h:3:9: note: expanded from macro 'X'\n2 errors generated.\n";
        let wrong = probe_errors(stderr, 1, 2).unwrap();
        assert_eq!(wrong.undefined, HashSet::from([0]));
        assert_eq!(wrong.not_constant, HashSet::from([1]));
        // A broken header, or one that isn't there, is no probe's fault.
        let e = probe_errors("./own.h:3:9: error: expected ';'\n", 1, 2).unwrap_err();
        assert!(e.contains("own.h:3:9"), "{e}");
        let e = probe_errors(
            "constants.c:1:10: fatal error: 'x.h' file not found\n",
            1,
            2,
        );
        assert!(e.unwrap_err().contains("x.h"));
    }

    #[test]
    fn floats_are_read_from_ir() {
        let ir = "@__velt_f0 = constant double 3.250000e+00, align 8, !dbg !0\n@__velt_f1 = dso_local constant float 0x3FF8000000000000, align 4\n@__velt_f2 = constant double -0.000000e+00, align 8\n";
        assert_eq!(float_in_ir(ir, 0), Some(3.25f64.to_bits()));
        assert_eq!(float_in_ir(ir, 1), Some(1.5f64.to_bits()));
        assert_eq!(float_in_ir(ir, 2), Some((-0.0f64).to_bits()));
        assert_eq!(float_in_ir(ir, 3), None);
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
