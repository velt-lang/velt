//! Linting a file the way `velt check --ts-compat` does, for the rule fixtures (cases.rs) and
//! the `tsc` oracle (oracle.rs): the file is loaded with the prelude and what it imports (the
//! standard library's `velt:` modules, files next to it, its JSX runtime), checked for the
//! editor (`velt_sema::ide::check_for_ide`) and linted as the only file in scope
//! (`velt_tscompat::lint_program`).

#![allow(dead_code)] // Each test binary uses part of it.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use velt_common::{FileId, SourceMap};
use velt_sema::SourceModule;
use velt_syntax::ast;
use velt_syntax::visit::{self, Visit};
use velt_tscompat::{canonical, lint_program, Finding};

/// `crates/velt_tscompat/tests/cases`.
pub fn cases_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases")
}

fn std_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std")
}

/// Parse `src`: a `.ts` file reads `<T>x` as a type assertion, like the loader.
pub fn parse(path: &Path, src: &str) -> ast::Module {
    parse_as(path, FileId(0), src)
}

fn parse_as(path: &Path, file: FileId, src: &str) -> ast::Module {
    let (module, diags) = if path.extension().is_some_and(|e| e == "ts") {
        velt_syntax::parse_ts_file(file, src)
    } else {
        velt_syntax::parse_file(file, src)
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
        if let Some(file) = relative(dir, &import.from) {
            out.push((import.from.clone(), file));
        }
    }
    out
}

/// The file a relative specifier names from `dir`, canonical.
fn relative(dir: &Path, spec: &str) -> Option<PathBuf> {
    if !spec.starts_with("./") && !spec.starts_with("../") {
        return None;
    }
    let found = ["vlt", "ts", "tsx"]
        .iter()
        .map(|ext| dir.join(format!("{spec}.{ext}")))
        .find(|f| f.is_file())?;
    Some(found.canonicalize().expect("canonical import"))
}

/// A loaded program: every module, the root's index and the sources.
pub struct Loaded {
    pub modules: Vec<SourceModule>,
    pub root: usize,
    pub sm: SourceMap,
}

/// Load `src` as the file `path`, with the prelude and everything it imports.
pub fn load(path: &Path, src: &str) -> Loaded {
    let mut l = Loader {
        modules: vec![],
        sm: SourceMap::new(),
        files: vec![],
    };
    let mut prelude: Vec<PathBuf> = std::fs::read_dir(std_dir().join("prelude"))
        .expect("std/prelude")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "vlt"))
        .collect();
    prelude.sort();
    for file in prelude {
        let stem = file.file_stem().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&file).expect("read the prelude");
        l.add(format!("std/prelude/{stem}"), true, &file, text);
    }
    let file = canonical(path);
    let root = l.add(
        file.to_string_lossy().into_owned(),
        false,
        &file,
        src.to_string(),
    );
    l.resolve();
    Loaded {
        modules: l.modules,
        root,
        sm: l.sm,
    }
}

struct Loader {
    modules: Vec<SourceModule>,
    sm: SourceMap,
    files: Vec<PathBuf>,
}

impl Loader {
    fn add(&mut self, path: String, is_std: bool, file: &Path, src: String) -> usize {
        let id = self.sm.add(file, src.clone());
        let ast = parse_as(file, id, &src);
        self.modules.push(SourceModule {
            path,
            is_std,
            file: id,
            ast,
            imports: vec![],
            jsx_runtime: None,
        });
        self.files.push(file.to_path_buf());
        self.modules.len() - 1
    }

    /// The module `spec` names from module `from`, loaded if new.
    fn import(&mut self, from: usize, spec: &str) -> Option<usize> {
        let (path, is_std, file) = match spec.strip_prefix("velt:") {
            Some(rest) => (
                format!("std/{rest}"),
                true,
                std_dir().join(format!("{rest}.vlt")),
            ),
            None => {
                let file = relative(self.files[from].parent()?, spec)?;
                (file.to_string_lossy().into_owned(), false, file)
            }
        };
        if let Some(i) = self.modules.iter().position(|m| m.path == path) {
            return Some(i);
        }
        let src = std::fs::read_to_string(&file).ok()?;
        Some(self.add(path, is_std, &file, src))
    }

    /// Resolve every module's imports and JSX runtime, loading new modules breadth-first.
    fn resolve(&mut self) {
        let mut queue: VecDeque<usize> = (0..self.modules.len()).collect();
        while let Some(m) = queue.pop_front() {
            let specs: Vec<String> = self.modules[m]
                .ast
                .items
                .iter()
                .filter_map(|i| match &i.kind {
                    ast::ItemKind::Import(import) => Some(import.from.clone()),
                    _ => None,
                })
                .collect();
            let before = self.modules.len();
            for spec in specs {
                if let Some(target) = self.import(m, &spec) {
                    let path = self.modules[target].path.clone();
                    self.modules[m].imports.push((spec, path));
                }
            }
            if has_jsx(&self.modules[m].ast) {
                let source = self.modules[m]
                    .ast
                    .jsx_import_source
                    .clone()
                    .unwrap_or_else(|| "velt:jsx".into());
                let spec = format!("{}/jsx-runtime", source.trim_end_matches('/'));
                if let Some(target) = self.import(m, &spec) {
                    let path = self.modules[target].path.clone();
                    self.modules[m].imports.push((spec, path.clone()));
                    self.modules[m].jsx_runtime = Some(path);
                }
            }
            queue.extend(before..self.modules.len());
        }
    }
}

fn has_jsx(module: &ast::Module) -> bool {
    struct Find(bool);
    impl Visit<'_> for Find {
        fn expr(&mut self, e: &ast::Expr) {
            self.0 |= matches!(e.kind, ast::ExprKind::Jsx(_));
        }
    }
    let mut find = Find(false);
    visit::walk_module(module, &mut find);
    find.0
}

/// Lint `src` (as the file `path`) as the only file in scope, after checking it with what it
/// imports. Its own check must pass: the lint only sees valid Velt.
pub fn lint_source(path: &Path, src: &str) -> Vec<Finding> {
    lint_checked(path, src)
        .unwrap_or_else(|errors| panic!("{} must pass `velt check`:\n{errors}", path.display()))
}

/// [`lint_source`], or [`lint_syntax`] for a sample that isn't valid Velt on its own (a
/// `declare function` without a native library, a JSX provider that doesn't exist).
pub fn lint_any(path: &Path, src: &str) -> Vec<Finding> {
    lint_checked(path, src).unwrap_or_else(|_| lint_syntax(path, src))
}

/// The findings, or the check's errors.
fn lint_checked(path: &Path, src: &str) -> Result<Vec<Finding>, String> {
    let loaded = load(path, src);
    let analysis = velt_sema::ide::check_for_ide(&loaded.modules, loaded.root);
    let root_file = loaded.modules[loaded.root].file;
    let errors: Vec<String> = analysis
        .diagnostics()
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.render(&loaded.sm))
        .collect();
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    let file = canonical(path);
    Ok(lint_program(
        &loaded.modules,
        &loaded.sm,
        analysis.diagnostics(),
        Some(&analysis),
        &|p| p == file,
        &|f| f == root_file,
    ))
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

/// Lint `src` (as the file `path`) with the syntax rules only, without checking it: for code
/// the lint must handle that `velt check` rejects on its own (`declare function` outside a
/// package with a native library).
pub fn lint_syntax(path: &Path, src: &str) -> Vec<Finding> {
    let module = parse(path, src);
    let file = canonical(path);
    let lint_module = velt_tscompat::LintModule {
        path: &file,
        src,
        imports: imports(path, &module),
        default_jsx_provider: module.jsx_import_source.is_none(),
        ast: &module,
    };
    velt_tscompat::lint(&[lint_module], &[file.as_path()], None)
}
