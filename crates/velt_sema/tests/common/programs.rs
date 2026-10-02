//! Load real `.vlt` programs for tests: the std prelude (`std/prelude/*.vlt`), the root file
//! and every module it imports (`./relative` and `std/x` specifiers), parsed with the real parser.
//! A module with a `// @jsxImportSource <source>` comment also loads `<source>/jsx-runtime` as
//! its JSX runtime (like the driver's loader; tests always name the source).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use velt_common::{Diagnostics, FileId, SourceMap};
use velt_sema::{check, hir, SourceModule};
use velt_syntax::ast;

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub struct Loaded {
    pub modules: Vec<SourceModule>,
    pub root: usize,
    pub sm: SourceMap,
}

impl Loaded {
    pub fn check(&self) -> (Option<hir::Program>, Diagnostics) {
        check(&self.modules, self.root)
    }

    /// All diagnostics rendered like the driver does (`file:line:col: error: ...`).
    pub fn render(&self, d: &Diagnostics) -> String {
        d.iter().map(|d| d.render(&self.sm) + "\n").collect()
    }
}

struct Loader {
    modules: Vec<SourceModule>,
    sm: SourceMap,
    files: Vec<PathBuf>,
    /// Accept parse errors (the parser's recovered AST is checked anyway).
    lenient: bool,
}

impl Loader {
    fn add(&mut self, canonical: &str, file: &Path, src: String) -> usize {
        let src = src.replace("\r\n", "\n");
        let name = file.file_name().unwrap().to_string_lossy().to_string();
        let id = self.sm.add(name, src.clone());
        let (ast, diags) = velt_syntax::parse_file(id, &src);
        let msgs: Vec<String> = diags.iter().map(|d| d.render(&self.sm)).collect();
        assert!(
            self.lenient || diags.is_empty(),
            "parse errors in {}:\n{}",
            file.display(),
            msgs.join("\n")
        );
        self.modules.push(SourceModule {
            path: canonical.to_string(),
            file: FileId(id.0),
            ast,
            imports: vec![],
            jsx_runtime: None,
        });
        self.files.push(file.to_path_buf());
        self.modules.len() - 1
    }

    /// Resolve every module's imports, loading new modules breadth-first.
    fn resolve(&mut self, root_dir: &Path) {
        let std_dir = repo_root().join("std");
        let mut queue: VecDeque<usize> = (0..self.modules.len()).collect();
        while let Some(m) = queue.pop_front() {
            let mut specs: Vec<String> = self.modules[m]
                .ast
                .items
                .iter()
                .filter_map(|i| match &i.kind {
                    ast::ItemKind::Import(imp) => Some(imp.from.clone()),
                    _ => None,
                })
                .collect();
            let runtime = (self.modules[m].ast.jsx_import_source.as_ref())
                .map(|s| format!("{s}/jsx-runtime"));
            specs.extend(runtime.clone());
            let dir = self.files[m].parent().unwrap().to_path_buf();
            for spec in specs {
                let (canonical, file) = if let Some(rest) = spec.strip_prefix("velt:") {
                    (format!("std/{rest}"), std_dir.join(format!("{rest}.vlt")))
                } else {
                    let file =
                        normalize(&dir.join(format!("{}.vlt", spec.trim_start_matches("./"))));
                    let rel = file
                        .strip_prefix(root_dir)
                        .unwrap_or(&file)
                        .with_extension("")
                        .to_string_lossy()
                        .replace('\\', "/");
                    (rel, file)
                };
                if !self.modules.iter().any(|x| x.path == canonical) {
                    // A missing module stays unresolved: sema reports the import.
                    let Ok(src) = std::fs::read_to_string(&file) else {
                        continue;
                    };
                    let i = self.add(&canonical, &file, src);
                    queue.push_back(i);
                }
                if runtime.as_ref() == Some(&spec) {
                    self.modules[m].jsx_runtime = Some(canonical.clone());
                }
                self.modules[m].imports.push((spec.clone(), canonical));
            }
        }
    }
}

/// `p` without `.` / `..` components (so `a/../b` and `b` are the same module).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn prelude(l: &mut Loader) {
    let dir = repo_root().join("std/prelude");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.retain(|f| f.extension().is_some_and(|e| e == "vlt"));
    files.sort();
    for f in files {
        let name = f.file_stem().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&f).unwrap();
        l.add(&format!("std/prelude/{name}"), &f, src);
    }
}

/// Load the program rooted at `file` (with the prelude).
pub fn load_file(file: &Path) -> Loaded {
    let src =
        std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
    load_src_at(file, &src)
}

/// Load a program whose root source is `src`, as if it were the file `file`.
pub fn load_src_at(file: &Path, src: &str) -> Loaded {
    load_with(file, src, false)
}

/// Like [`load_src`] but the root may have syntax errors.
pub fn load_src_lenient(src: &str) -> Loaded {
    load_with(&repo_root().join("tests/inline/main.vlt"), src, true)
}

/// [`load_src_lenient`] for a source placed at `file` (so relative imports resolve from there).
pub fn load_src_lenient_at(file: &Path, src: &str) -> Loaded {
    load_with(file, src, true)
}

fn load_with(file: &Path, src: &str, lenient: bool) -> Loaded {
    let mut l = Loader {
        modules: vec![],
        sm: SourceMap::new(),
        files: vec![],
        lenient: false,
    };
    prelude(&mut l);
    l.lenient = lenient;
    let root = l.add("main", file, src.to_string());
    let root_dir = normalize(file.parent().unwrap());
    l.resolve(&root_dir);
    Loaded {
        modules: l.modules,
        root,
        sm: l.sm,
    }
}

/// Load an inline test program (it may import `std/...` modules).
pub fn load_src(src: &str) -> Loaded {
    load_src_at(&repo_root().join("tests/inline/main.vlt"), src)
}

/// Check an inline program and require success.
pub fn ok_src(src: &str) -> hir::Program {
    let l = load_src(src);
    let (p, d) = l.check();
    assert!(
        p.is_some() && d.iter().all(|d| !d.is_error()),
        "unexpected diagnostics:\n{}",
        l.render(&d)
    );
    p.unwrap()
}

/// Check an inline program and require failure; returns the rendered diagnostics.
pub fn err_src(src: &str) -> String {
    let l = load_src(src);
    let (p, d) = l.check();
    let r = l.render(&d);
    assert!(p.is_none(), "expected errors, program was accepted:\n{r}");
    r
}
