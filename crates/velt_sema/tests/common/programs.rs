//! Load real `.velt` programs for tests: the std prelude (`std/prelude/*.velt`), the root file
//! and every module it imports (`./relative` and `std/x` specifiers), parsed with the real parser.
//! A module with a `// @jsxImportSource <source>` comment also loads `<source>/jsx-runtime` as
//! its JSX runtime (like the driver's loader; tests always name the source), and a non-std module
//! that mentions a global loaded on demand (`std/prelude/global/*.velt`) loads its module.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use velt_common::{Diagnostics, FileId, SourceMap};
use velt_sema::{check, hir, SourceModule};
use velt_syntax::ast;

thread_local! {
    /// Parse without packing number arrays (`velt_syntax::parse_file_unpacked`).
    static UNPACKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `f` with every module it loads parsed without packing large array literals of numbers into
/// `ast::ExprKind::NumArray` (as before #2421): what a packed array must check exactly like.
/// The flag goes back to what it was when `f` returns or panics, so nested calls and a failed
/// test leave the thread as they found it.
pub fn unpacked<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            UNPACKED.with(|u| u.set(self.0));
        }
    }
    let _restore = Restore(UNPACKED.with(|u| u.replace(true)));
    f()
}

/// Whether this thread parses without packing now (inside [`unpacked`]).
pub fn is_unpacked() -> bool {
    UNPACKED.with(|u| u.get())
}

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
        let (ast, diags) = match UNPACKED.with(|u| u.get()) {
            true => velt_syntax::parse_file_unpacked(id, &src),
            false => velt_syntax::parse_file(id, &src),
        };
        let msgs: Vec<String> = diags.iter().map(|d| d.render(&self.sm)).collect();
        assert!(
            self.lenient || diags.is_empty(),
            "parse errors in {}:\n{}",
            file.display(),
            msgs.join("\n")
        );
        self.modules.push(SourceModule {
            path: canonical.to_string(),
            is_std: canonical.starts_with("std/"),
            is_dependency: false,
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
            self.globals(m, &mut queue);
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
                // A std module's `#name`: the variant std/imports.velt maps it to for every
                // build (`default`), as the driver's loader picks it for a native target.
                let target = match spec.strip_prefix('#') {
                    Some(_) => std_default_variant(&std_dir, &spec).map(|p| format!("velt/{p}")),
                    None => None,
                };
                let (canonical, file) =
                    if let Some(rest) = target.as_deref().unwrap_or(&spec).strip_prefix("velt/") {
                        let file = std_dir.join(format!("{rest}.velt"));
                        // A folder module: `velt/fs` is `std/fs/index.velt`.
                        let file = if file.is_file() {
                            file
                        } else {
                            std_dir.join(rest).join("index.velt")
                        };
                        (format!("std/{rest}"), file)
                    } else {
                        let file =
                            normalize(&dir.join(format!("{}.velt", spec.trim_start_matches("./"))));
                        // A std file's relative import names a std module (`./number` in
                        // std/lang/hooks/index.velt is `std/lang/hooks/number`).
                        let rel = match file.strip_prefix(normalize(&std_dir)) {
                            Ok(in_std) => Path::new("std").join(in_std),
                            Err(_) => file.strip_prefix(root_dir).unwrap_or(&file).to_path_buf(),
                        };
                        let rel = rel.with_extension("").to_string_lossy().replace('\\', "/");
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

/// The `default` variant of std's conditional import `name` (`#http-transport`) as a path below
/// std/ without `.velt`, read from std/imports.velt, whose entries are written
/// `"#name": { …, default: "./path.velt" }`.
fn std_default_variant(std_dir: &Path, name: &str) -> Option<String> {
    let src = std::fs::read_to_string(std_dir.join("imports.velt")).ok()?;
    let entry = &src[src.find(&format!("\"{name}\":"))?..];
    let entry = &entry[..entry.find('}')?];
    let after = &entry[entry.find("default:")? + "default:".len()..];
    let path = after.split('"').nth(1)?;
    Some(
        path.trim_start_matches("./")
            .trim_end_matches(".velt")
            .to_string(),
    )
}

impl Loader {
    /// Load the global modules (`std/prelude/global/*.velt`) whose re-exported names non-std
    /// module `m` mentions as whole words, as the driver's loader does.
    fn globals(&mut self, m: usize, queue: &mut VecDeque<usize>) {
        if self.modules[m].is_std {
            return;
        }
        let src = self.sm.get(self.modules[m].file).src.clone();
        let dir = repo_root().join("std/prelude/global");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        for f in files {
            let name = f.file_stem().unwrap().to_string_lossy().to_string();
            let canonical = format!("std/prelude/global/{name}");
            if self.modules.iter().any(|x| x.path == canonical) {
                continue;
            }
            let text = std::fs::read_to_string(&f).unwrap();
            let own = bound(&self.modules[m].ast);
            // The float printer's global (`// load for: a conversion operand may be a float`),
            // approximated as the driver's loader would see this test's sources: a template with
            // a float literal, `/` or `Math.` somewhere in the source.
            let float_text = text.contains("// load for: a conversion operand may be a float")
                && src.contains("${")
                && (src.contains('/')
                    || src.contains("Math.")
                    || src
                        .as_bytes()
                        .windows(3)
                        .any(|w| w[0].is_ascii_digit() && w[1] == b'.' && w[2].is_ascii_digit()));
            // A tuple's array methods: one of the global's words, each `.name` (`// load when`).
            let tuple_method = name == "tuple_elements"
                && text
                    .lines()
                    .find_map(|l| l.split_once(" uses: ").map(|(_, w)| w))
                    .is_some_and(|words| words.split_whitespace().any(|w| src.contains(w)));
            if float_text
                || tuple_method
                || reexports(&text)
                    .iter()
                    .any(|n| !own.contains(n) && mentions(&src, n))
            {
                let i = self.add(&canonical, &f, text);
                queue.push_back(i);
            }
        }
    }
}

/// The names module `m` imports or declares at its top level (they hide a global), as the
/// driver's loader counts them (`velt_loader`'s `globals::bound_names`).
fn bound(m: &ast::Module) -> Vec<String> {
    let mut names = vec![];
    for item in &m.items {
        match &item.kind {
            ast::ItemKind::Import(imp) => {
                if !item.exported || imp.from.is_empty() {
                    let local =
                        |n: &ast::ImportName| n.alias.as_ref().unwrap_or(&n.name).name.clone();
                    names.extend(imp.names.iter().map(local));
                }
                names.extend(imp.namespace.iter().map(|n| n.name.clone()));
            }
            ast::ItemKind::Class(t) | ast::ItemKind::Struct(t) => names.push(t.name.name.clone()),
            ast::ItemKind::Function(f) => names.push(f.sig.name.name.clone()),
            ast::ItemKind::ExternFn(f) => names.push(f.name.name.clone()),
            ast::ItemKind::Interface(i) => names.push(i.name.name.clone()),
            ast::ItemKind::Enum(e) => names.push(e.name.name.clone()),
            ast::ItemKind::TypeAlias(a) => names.push(a.name.name.clone()),
            ast::ItemKind::Var(v) => {
                if let ast::PatternKind::Ident(id) = &v.pattern.kind {
                    names.push(id.name.clone());
                }
            }
            ast::ItemKind::Extend(_) | ast::ItemKind::Augment(_) | ast::ItemKind::Namespace(_) => {}
        }
    }
    names
}

/// The names `export { … } from "…"` items of `src` re-export.
fn reexports(src: &str) -> Vec<String> {
    let mut sm = SourceMap::new();
    let id = sm.add("global.velt".to_string(), src.to_string());
    let (module, _) = velt_syntax::parse_file(id, src);
    let mut names = vec![];
    for item in &module.items {
        if let ast::ItemKind::Import(imp) = &item.kind {
            if item.exported {
                names.extend(
                    imp.names
                        .iter()
                        .map(|n| n.alias.as_ref().unwrap_or(&n.name).name.clone()),
                );
            }
        }
    }
    names
}

/// Whether `src` contains `name` as a whole identifier.
fn mentions(src: &str, name: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    src.match_indices(name).any(|(at, _)| {
        !src[..at].chars().next_back().is_some_and(ident)
            && !src[at + name.len()..].chars().next().is_some_and(ident)
    })
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
    // In the order std/prelude/index.velt lists the files, as veltc's loader reads them.
    let dir = repo_root().join("std/prelude");
    let index = std::fs::read_to_string(dir.join("index.velt")).expect("std/prelude/index.velt");
    let order = velt_common::std_modules::prelude_order(&index).expect("std/prelude/index.velt");
    for name in order {
        let f = dir.join(format!("{name}.velt"));
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
    load_with(&repo_root().join("tests/inline/main.velt"), src, true)
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
    // What a check can demand is loaded up front here: this loader has no load callback.
    for path in velt_common::std_hooks::ON_DEMAND_MODULES {
        let rel = path.strip_prefix("std/").expect("a std path");
        let f = repo_root().join(format!("std/{rel}.velt"));
        let src = std::fs::read_to_string(&f).expect("a hook module");
        l.add(path, &f, src);
    }
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
    load_src_at(&repo_root().join("tests/inline/main.velt"), src)
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
