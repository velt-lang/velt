//! Linting a file the way `velt check --ts-compat` does, for the rule fixtures (cases.rs) and
//! the `tsc` oracle (oracle.rs).

use std::path::{Path, PathBuf};

use velt_common::FileId;
use velt_syntax::ast;
use velt_tscompat::{lint, Finding, LintModule};

/// `crates/velt_tscompat/tests/cases`.
pub fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases")
}

/// Parse `src`: a `.ts` file reads `<T>x` as a type assertion, like the loader.
pub fn parse(path: &Path, src: &str) -> ast::Module {
    let (module, diags) = if path.extension().is_some_and(|e| e == "ts") {
        velt_syntax::parse_ts_file(FileId(0), src)
    } else {
        velt_syntax::parse_file(FileId(0), src)
    };
    assert!(diags.is_empty(), "{}: {diags:?}", path.display());
    module
}

/// The relative imports of `module`, resolved next to `path` like the loader does.
pub fn imports(path: &Path, module: &ast::Module) -> Vec<(String, PathBuf)> {
    let dir = path.parent().expect("a case has a directory");
    let mut out = vec![];
    for item in &module.items {
        let ast::ItemKind::Import(import) = &item.kind else {
            continue;
        };
        let found = ["vlt", "ts", "tsx"]
            .iter()
            .map(|ext| dir.join(format!("{}.{ext}", import.from)))
            .find(|f| f.is_file());
        if let Some(file) = found {
            let file = file.canonicalize().expect("canonical import");
            out.push((import.from.clone(), file));
        }
    }
    out
}

/// Lint `module` (parsed from `src`) as the only file in scope, with the default JSX provider
/// unless it names one.
pub fn lint_module(path: &Path, src: &str, module: &ast::Module) -> Vec<Finding> {
    let canonical = path.canonicalize().unwrap_or(path.to_path_buf());
    let lint_module = LintModule {
        path: &canonical,
        src,
        imports: imports(path, module),
        default_jsx_provider: module.jsx_import_source.is_none(),
        ast: module,
    };
    lint(&[lint_module], &[canonical.as_path()])
}

/// Lint `src` as the only file in scope.
pub fn lint_source(path: &Path, src: &str) -> Vec<Finding> {
    lint_module(path, src, &parse(path, src))
}

/// The 1-based line of byte offset `at` in `src`.
pub fn line_of(src: &str, at: u32) -> usize {
    src[..at as usize].matches('\n').count() + 1
}

/// `src` with every fix of `findings` applied (fixes never overlap in the cases and samples).
pub fn apply_fixes(src: &str, findings: &[Finding]) -> String {
    let mut fixes: Vec<_> = findings.iter().filter_map(|f| f.fix.as_ref()).collect();
    fixes.sort_by_key(|f| std::cmp::Reverse(f.span.lo));
    let mut out = src.to_string();
    for fix in fixes {
        out.replace_range(fix.span.lo as usize..fix.span.hi as usize, &fix.replacement);
    }
    out
}
