//! Module loading: root file → every module the program needs, parsed, as [`SourceModule`]s for sema.
//!
//! Order of the result: the prelude (`std/prelude/*.vlt`, canonical `"std/prelude/<name>"`), then
//! the root (`"main"`), then imported modules in breadth-first discovery order. Specifiers resolve as
//! - `"./x"`, `"../x"` → `x.vlt` or the folder module `x/index.vlt` relative to the importing
//!   file ([`spec`]);
//! - `"std/x"` → `<std root>/x.vlt` or `<std root>/x/index.vlt` ([`std_root`]);
//! - a `paths` alias of the importing package (`"@app/*": "src/*"` in `package.vlt`) → the
//!   aliased file, like a relative import ([`PackageResolver::path_alias`]);
//! - `"pkg"` / `"pkg/sub"` → `src/lib.vlt` / `src/sub.vlt` (or `src/sub/index.vlt`) of a
//!   dependency of the importing package, found through a [`PackageResolver`] (vpm's installed
//!   package graph).
//!
//! Local export lists (`export { a, b };`, an import item with an empty specifier) load nothing.
//! A module containing JSX also imports its JSX runtime ([`jsx`]).
//!
//! Modules are deduplicated by canonical file path, so import cycles simply reuse the already
//! loaded module (sema handles cyclic references between functions and types).
//!
//! An optional in-memory overlay (the language server's unsaved editor buffers) takes precedence
//! over the file system for every read, including files that do not exist on disk yet.

mod jsx;
mod locate;
mod spec;
mod std_root;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use velt_common::{Diagnostic, Diagnostics, SourceMap, Span};
use velt_sema::SourceModule;
use velt_syntax::ast;

pub use locate::{Origin, PackageResolver};
pub use spec::{resolve_spec, ModuleRef};
pub use std_root::{prelude_files, std_root};

/// Inputs of [`load_program`] besides the root path.
#[derive(Default)]
pub struct LoadOptions<'a> {
    /// Standard library root (`None` → `std/` imports fail and no prelude is loaded).
    pub std_root: Option<PathBuf>,
    /// Package dependency lookup (`None` → every package import is "not a dependency").
    pub packages: Option<&'a dyn PackageResolver>,
    /// Source of the root module instead of reading the root file (the `velt test` harness).
    pub root_source: Option<String>,
    /// Unsaved file contents by path (the language server's open documents); consulted before
    /// the disk for every module, prelude included.
    pub overlay: Option<&'a HashMap<PathBuf, String>>,
}

/// Result of loading a program: the modules and the index of the root (`main`).
pub struct Loaded {
    /// All modules (prelude first).
    pub modules: Vec<SourceModule>,
    /// Index of the root module in `modules`.
    pub root: usize,
}

/// Read and parse the root file and everything it imports. Failing to read the root is returned
/// as `Err(message)`; syntax and import problems go into `diags` (the caller decides whether to stop).
pub fn load_program(
    sm: &mut SourceMap,
    root: &Path,
    opts: LoadOptions,
    diags: &mut Diagnostics,
) -> Result<Loaded, String> {
    let overlay = opts
        .overlay
        .map(|o| o.iter().map(|(p, s)| (file_key(p), s.clone())).collect())
        .unwrap_or_default();
    let mut loader = Loader {
        sm,
        diags,
        opts: &opts,
        overlay,
        modules: vec![],
        origins: vec![],
        by_file: HashMap::new(),
        std_key: opts.std_root.as_deref().map(file_key),
    };
    if let Some(std) = &opts.std_root {
        for file in prelude_files(std) {
            loader.load_prelude(&file, std);
        }
    }
    let src = match &opts.root_source {
        Some(src) => src.clone(),
        None => loader
            .read(root)
            .map_err(|e| format!("cannot read `{}`: {e}", root.display()))?,
    };
    let dir = root.parent().unwrap_or(Path::new("")).to_path_buf();
    let root_index = loader.add(root, file_key(root), src, "main".into(), Origin::Root(dir));
    let mut queue: VecDeque<usize> = (0..loader.modules.len()).collect();
    while let Some(index) = queue.pop_front() {
        loader.resolve_imports(index, &mut queue);
    }
    Ok(Loaded {
        modules: loader.modules,
        root: root_index,
    })
}

struct Loader<'a, 'o> {
    sm: &'a mut SourceMap,
    diags: &'a mut Diagnostics,
    opts: &'a LoadOptions<'o>,
    /// [`LoadOptions::overlay`] keyed by [`file_key`].
    overlay: HashMap<PathBuf, String>,
    modules: Vec<SourceModule>,
    /// Display path and origin of each module (parallel to `modules`).
    origins: Vec<(PathBuf, Origin)>,
    /// Canonical file path → module index.
    by_file: HashMap<PathBuf, usize>,
    /// [`file_key`] of the std root: std modules are exactly the files below it.
    std_key: Option<PathBuf>,
}

impl Loader<'_, '_> {
    /// Contents of `path`: the overlay's if it has the file, else the disk's.
    fn read(&self, path: &Path) -> std::io::Result<String> {
        // `file_key` costs a filesystem round trip (a few hundred µs on Windows).
        if self.overlay.is_empty() {
            return std::fs::read_to_string(path);
        }
        match self.overlay.get(&file_key(path)) {
            Some(src) => Ok(src.clone()),
            None => std::fs::read_to_string(path),
        }
    }

    /// Whether `path` names a loadable file (on disk or in the overlay).
    fn exists(&self, path: &Path) -> bool {
        path.is_file() || self.overlay.contains_key(&file_key(path))
    }

    fn load_prelude(&mut self, file: &Path, std: &Path) {
        match self.read(file) {
            Ok(src) => {
                let origin = Origin::Std(std.to_path_buf());
                let canonical = origin.canonical(file);
                self.add(file, file_key(file), src, canonical, origin);
            }
            Err(e) => self.diags.push(Diagnostic::error(
                format!("cannot read prelude `{}`: {e}", file.display()),
                Span::DUMMY,
            )),
        }
    }

    /// Parse and register a module (`key`: its [`file_key`]); returns its index.
    fn add(
        &mut self,
        path: &Path,
        key: PathBuf,
        src: String,
        canonical: String,
        origin: Origin,
    ) -> usize {
        let file = self.sm.add(path, src);
        let (ast, parse_diags) = velt_syntax::parse_file(file, &self.sm.get(file).src);
        self.diags.extend(parse_diags);
        let index = self.modules.len();
        self.by_file.insert(key, index);
        self.modules.push(SourceModule {
            path: canonical,
            is_std: matches!(origin, Origin::Std(_)),
            file,
            ast,
            imports: vec![],
            jsx_runtime: None,
        });
        self.origins.push((path.to_path_buf(), origin));
        index
    }

    /// Resolve every import of module `index`, loading new modules (queued for their own imports).
    fn resolve_imports(&mut self, index: usize, queue: &mut VecDeque<usize>) {
        let imports: Vec<(String, Span)> = self.modules[index]
            .ast
            .items
            .iter()
            .filter_map(|item| match &item.kind {
                ast::ItemKind::Import(imp) if !imp.from.is_empty() => {
                    Some((imp.from.clone(), imp.from_span))
                }
                _ => None,
            })
            .collect();
        for (spec, span) in imports {
            if self.modules[index].imports.iter().any(|(s, _)| *s == spec) {
                continue;
            }
            if let Some(target) = self.import(index, &spec, span, queue) {
                let canonical = self.modules[target].path.clone();
                self.modules[index].imports.push((spec, canonical));
            }
        }
        self.jsx_runtime(index, queue);
    }

    /// Load the JSX runtime of module `index` if it contains JSX.
    fn jsx_runtime(&mut self, index: usize, queue: &mut VecDeque<usize>) {
        let Some(span) = jsx::first_jsx(&self.modules[index].ast) else {
            return;
        };
        let (source, from) = match &self.modules[index].ast.jsx_import_source {
            Some(s) => (s.clone(), "its `// @jsxImportSource` comment"),
            None => match self
                .opts
                .packages
                .and_then(|p| p.jsx_import_source(&self.origins[index].0))
            {
                Some(s) => (s, "`jsx.importSource` in package.vlt"),
                None => (jsx::DEFAULT_IMPORT_SOURCE.to_string(), "the default"),
            },
        };
        let spec = jsx::runtime_spec(&source);
        match self.import(index, &spec, span, queue) {
            Some(target) => {
                self.modules[index].jsx_runtime = Some(self.modules[target].path.clone())
            }
            None => {
                if let Some(d) = self.diags.pop() {
                    let note =
                        format!("this module contains JSX: its runtime is `{spec}`, from {from}");
                    self.diags.push(d.with_note(note));
                }
            }
        }
    }

    /// Locate (and load if new) the module `spec` imported by module `index`.
    fn import(
        &mut self,
        index: usize,
        spec: &str,
        span: Span,
        queue: &mut VecDeque<usize>,
    ) -> Option<usize> {
        let (importer, from) = self.origins[index].clone();
        let dir = importer.parent().unwrap_or(Path::new(""));
        let alias = self.path_alias(&importer, spec);
        let aliased = alias.is_some();
        let module = match alias {
            Some(file) => ModuleRef::Relative { file },
            None => match resolve_spec(spec, dir) {
                Ok(m) => m,
                Err(msg) => return self.error(msg, vec![], span),
            },
        };
        let target = match locate::target(
            module,
            &importer,
            &from,
            self.opts.std_root.as_deref(),
            self.opts.packages,
        ) {
            Ok(t) => t,
            Err((msg, notes)) => return self.error(msg, notes, span),
        };
        let Some(file) = target.candidates.iter().find(|f| self.exists(f)).cloned() else {
            let notes = target
                .candidates
                .iter()
                .map(|f| format!("tried `{}`", f.display()))
                .collect();
            return self.error(format!("cannot find module `{spec}`"), notes, span);
        };
        let key = file_key(&file);
        if let Err(msg) = self.std_membership(&target.origin, &key, spec) {
            return self.error(msg, vec![], span);
        }
        if let Some(&existing) = self.by_file.get(&key) {
            return Some(existing);
        }
        let src = match self.read(&file) {
            Ok(s) => s,
            Err(e) => {
                return self.error(
                    format!("cannot read module `{spec}` (`{}`): {e}", file.display()),
                    vec![],
                    span,
                )
            }
        };
        let canonical = target
            .canonical
            .unwrap_or_else(|| target.origin.canonical(&file));
        // `std/…` names the standard library: a user module with such a path (a `./std/`
        // directory, a path alias into one) would collide with it.
        if !matches!(target.origin, Origin::Std(_)) && is_std_path(&canonical) {
            let fix = if aliased {
                "change the `paths` alias"
            } else {
                "rename its `std` directory"
            };
            let msg = format!(
                "module `{spec}` would have the module path `{canonical}`, which is reserved for the standard library ({fix})"
            );
            return self.error(msg, vec![], span);
        }
        if self.modules.iter().any(|m| m.path == canonical) {
            let msg = format!("module `{spec}` has the same module path `{canonical}` as another module (rename the file)");
            return self.error(msg, vec![], span);
        }
        let new = self.add(&file, key, src, canonical, target.origin);
        queue.push_back(new);
        Some(new)
    }

    /// A module is std exactly when its file lies below the std root (`key`: its [`file_key`],
    /// so symlinks and junctions are resolved): a `velt:` path must not leave the root, and user
    /// code must not load a std file under a non-std origin (deduplication would hand that copy
    /// to std's own imports).
    fn std_membership(&self, origin: &Origin, key: &Path, spec: &str) -> Result<(), String> {
        let Some(std_key) = &self.std_key else {
            return Ok(());
        };
        let inside = key.starts_with(std_key);
        match origin {
            Origin::Std(_) if !inside => Err(format!(
                "module `{spec}` resolves to a file outside the standard library"
            )),
            Origin::Std(_) => Ok(()),
            _ if inside => {
                let rel = vpm::relpath::relative(key, std_key);
                let rel = rel.strip_suffix(".vlt").unwrap_or(&rel);
                let rel = rel.strip_suffix("/index").unwrap_or(rel);
                Err(format!(
                    "module `{spec}` is a file of the standard library: import it as `velt:{rel}`"
                ))
            }
            _ => Ok(()),
        }
    }

    /// The file a `paths` alias maps `spec` to (bare specifiers only: relative and `velt:`
    /// imports are never aliased).
    fn path_alias(&self, importer: &Path, spec: &str) -> Option<PathBuf> {
        if spec.starts_with("./") || spec.starts_with("../") || spec.starts_with("velt:") {
            return None;
        }
        let mut file = self
            .opts
            .packages?
            .path_alias(importer, spec)?
            .into_os_string();
        file.push(".vlt");
        Some(vpm::relpath::normalize(Path::new(&file)))
    }

    fn error(&mut self, msg: String, notes: Vec<String>, span: Span) -> Option<usize> {
        let d = notes
            .into_iter()
            .fold(Diagnostic::error(msg, span), Diagnostic::with_note);
        self.diags.push(d);
        None
    }
}

/// Whether canonical module path `path` is in the standard library's namespace (`std`, `std/…`).
fn is_std_path(path: &str) -> bool {
    path == "std" || path.starts_with("std/")
}

/// Identity of a file for deduplication (canonical path when it exists).
fn file_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| vpm::relpath::absolute(path))
}

#[cfg(test)]
mod tests;
