//! Rule fixtures: every `tests/cases/*.{vlt,ts,tsx}` (except helpers starting with `_`) is
//! linted on its own, and its findings must match the `//~ code…` annotations (`//~^` for the
//! line above): the codes found on each line, in order. A case with fixes has `<name>.fixed`,
//! the case with every fix applied (`VELT_BLESS=1` rewrites them). Every case also passes
//! `velt check` (crates/veltc/tests/ts_compat.rs), so the rules only ever see valid Velt.

mod common;

use std::path::{Path, PathBuf};

use common::{cases_dir, imports, lint_source, parse};
use velt_common::{FileId, Span};
use velt_tscompat::{lint, Finding, LintModule};

const ANNOTATION: &str = "//~";

/// The cases, sorted.
fn cases() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(cases_dir())
        .expect("tests/cases")
        .map(|e| e.expect("entry").path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("_");
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            !name.starts_with('_') && ["vlt", "ts", "tsx"].contains(&ext)
        })
        .collect();
    files.sort();
    files
}

/// `(line, codes)` for every line with findings (1-based lines).
fn by_line(src: &str, findings: &[Finding]) -> Vec<(usize, Vec<String>)> {
    let mut lines: Vec<(usize, Vec<String>)> = vec![];
    for f in findings {
        let line = common::line_of(src, f.span.lo);
        match lines.last_mut() {
            Some((l, codes)) if *l == line => codes.push(f.code.to_string()),
            _ => lines.push((line, vec![f.code.to_string()])),
        }
    }
    lines
}

/// The annotated `(line, codes)`: `//~ codes` for its own line, `//~^ codes` for the line above
/// (where `velt fmt` moves a comment that followed a `{`).
fn expected(src: &str) -> Vec<(usize, Vec<String>)> {
    src.lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let (_, codes) = line.split_once(ANNOTATION)?;
            let (line, codes) = match codes.strip_prefix('^') {
                Some(codes) => (i, codes),
                None => (i + 1, codes),
            };
            Some((line, codes.split_whitespace().map(String::from).collect()))
        })
        .collect()
}

/// `src` with every fix applied (fixes never overlap in the cases).
fn apply_fixes(src: &str, findings: &[Finding]) -> String {
    let mut fixes: Vec<_> = findings.iter().filter_map(|f| f.fix.as_ref()).collect();
    fixes.sort_by_key(|f| std::cmp::Reverse(f.span.lo));
    let mut out = src.to_string();
    for fix in fixes {
        out.replace_range(fix.span.lo as usize..fix.span.hi as usize, &fix.replacement);
    }
    out
}

fn check_fixed(path: &Path, src: &str, findings: &[Finding]) -> Result<(), String> {
    let fixed_path = path.with_extension("fixed");
    let has_fixes = findings.iter().any(|f| f.fix.is_some());
    let fixed = apply_fixes(src, findings);
    if std::env::var_os("VELT_BLESS").is_some() {
        if has_fixes {
            std::fs::write(&fixed_path, &fixed).map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    match std::fs::read_to_string(&fixed_path) {
        Ok(want) if want == fixed => Ok(()),
        Ok(_) => Err(format!("{}: fixes differ (VELT_BLESS=1)", path.display())),
        Err(_) if has_fixes => Err(format!("{}: no .fixed file", path.display())),
        Err(_) => Ok(()),
    }
}

#[test]
fn every_case_reports_its_annotated_findings() {
    let mut failures = vec![];
    for path in cases() {
        let src = std::fs::read_to_string(&path).expect("read case");
        let findings = lint_source(&path, &src);
        let (got, want) = (by_line(&src, &findings), expected(&src));
        if got != want {
            failures.push(format!(
                "{}:\n  got  {got:?}\n  want {want:?}",
                path.display()
            ));
        }
        if let Err(e) = check_fixed(&path, &src, &findings) {
            failures.push(e);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_finding_explains_itself() {
    for path in cases() {
        let src = std::fs::read_to_string(&path).expect("read case");
        for f in lint_source(&path, &src) {
            assert!(
                f.span.hi > f.span.lo,
                "{}: empty span: {f:?}",
                path.display()
            );
            assert!(f.notes.len() >= 2, "why and what to write: {f:?}");
        }
    }
}

/// `declare function` is valid Velt only in a package with a native library, so it has no case
/// file (cases must pass `velt check` on their own).
#[test]
fn declare_function_is_reported() {
    let src = "declare function add(a: number, b: number): number;\n";
    let findings = lint_source(Path::new("declare.vlt"), src);
    let codes: Vec<&str> = findings.iter().map(|f| f.code).collect();
    assert_eq!(codes, ["declare-fn"]);
    assert_eq!(findings[0].span, Span::new(FileId(0), 0, 7));
}

#[test]
fn jsx_with_a_named_provider_is_not_reported() {
    let src = "// @jsxImportSource some-provider\n\
               export function A(): JSX.Element { return <a />; }\n";
    assert!(lint_source(Path::new("named.tsx"), src).is_empty());
}

#[test]
fn imports_of_linted_files_are_inside() {
    let dir = cases_dir();
    let (case, helper) = (
        dir.join("outside_import.vlt"),
        dir.join("_outside_helper.vlt"),
    );
    let src = std::fs::read_to_string(&case).expect("read case");
    let helper_src = std::fs::read_to_string(&helper).expect("read helper");
    let (module, helper_module) = (parse(&case, &src), parse(&helper, &helper_src));
    let (case, helper) = (case.canonicalize().unwrap(), helper.canonicalize().unwrap());
    let modules = [
        LintModule {
            path: &case,
            src: &src,
            imports: imports(&case, &module),
            default_jsx_provider: false,
            ast: &module,
        },
        LintModule {
            path: &helper,
            src: &helper_src,
            imports: vec![],
            default_jsx_provider: false,
            ast: &helper_module,
        },
    ];
    assert_eq!(lint(&modules, &[case.as_path(), helper.as_path()]), vec![]);
}

/// Defaults in `for…of` and `catch` patterns parse but `velt check` rejects them today, so they
/// have no case file; the lint still looks at them.
#[test]
fn defaults_in_for_of_and_catch_patterns_are_linted() {
    let src = "function f(ps: { x: number | null }[]) {\n\
               \x20 for (const { x = 1.5f64 } of ps) {}\n\
               \x20 try {} catch ({ message = 2.5f64 }) {}\n\
               }\n";
    let codes: Vec<&str> = lint_source(Path::new("patterns.vlt"), src)
        .iter()
        .map(|f| f.code)
        .collect();
    assert_eq!(codes, ["number-suffix", "number-suffix"]);
}

#[test]
fn a_large_integer_literal_has_no_fix_and_says_why() {
    let findings = lint_source(Path::new("big.vlt"), "const big = 9007199254740993i64;\n");
    assert_eq!(findings.len(), 1);
    assert!(findings[0].fix.is_none());
    assert!(findings[0].notes[1].contains("`i64`"), "{:?}", findings[0]);
    let findings = lint_source(Path::new("edge.vlt"), "const edge = 9007199254740992i64;\n");
    assert!(findings[0].fix.is_some());
}
