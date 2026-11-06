//! Asking clang about the names of `declare c const`s (`constants`): each in a file of its own
//! making, `__typeof__(NAME)` says its C type and that it exists, and an enumerator initialized
//! from it holds its value, which the JSON dump prints. A float can't initialize an enumerator
//! exactly: those are read from the LLVM IR of a constant initialized from them, in a second
//! run.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use vpm::c_headers::{CConstantValue, Probe};

use super::ctype::{CType, Headers};
use super::LibraryHeaders;

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

/// What clang says about `names` in a library's headers (of the package at `root`), with its
/// `defines`, on `zig_target`, run in `dir`, and every file it read.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_clang(
    zig: &Path,
    dir: &Path,
    root: &Path,
    read_with: LibraryHeaders,
    names: &[&str],
    zig_target: &str,
    long_bits: u32,
) -> Result<(BTreeMap<String, Probe>, Vec<PathBuf>), String> {
    let (headers, defines) = (read_with.headers, read_with.defines);
    let includes = super::includes(root, headers)?;
    let file = dir.join(SOURCE);
    std::fs::write(&file, probe_source(&includes, names)).map_err(|e| e.to_string())?;
    let mut cc1 = super::cc1_command(zig, dir, root, SOURCE, defines, zig_target)?;
    cc1.extend(["-ferror-limit".into(), "0".into(), "-ast-dump=json".into()]);
    cc1.extend(
        [
            "-dependency-file",
            super::DEPENDENCIES,
            "-MT",
            "deps",
            "-sys-header-deps",
        ]
        .map(str::to_string),
    );
    let out = super::zig_command(zig, dir)
        .args(&cc1)
        .output()
        .map_err(|e| format!("running {}: {e}", zig.display()))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let wrong = probe_errors(&stderr, headers.len(), names.len())?;
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("clang's AST is not JSON: {e}"))?;
    let dependencies = std::fs::read_to_string(dir.join(super::DEPENDENCIES))
        .map_err(|e| format!("clang wrote no list of the files it read: {e}"))?;
    let read = super::files_read(dir, &dependencies);
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
        let mut cc1 = super::cc1_command(zig, dir, root, SOURCE, defines, zig_target)?;
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
    Ok((probes, read))
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
    use super::*;

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
}
