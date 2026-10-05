//! Module loading: root file → every module the program needs, parsed, as [`SourceModule`]s for sema.
//!
//! Order of the result: the prelude (`std/prelude/*.vlt`, canonical `"std/prelude/<name>"`), then
//! the root (`"main"`), then imported modules in breadth-first discovery order, then any extra
//! roots ([`load_with_roots`]) not loaded yet, each followed by what it imports. Specifiers resolve as
//! - `"./x"`, `"../x"` → `x.vlt`, `x.ts` or `x.tsx`, else the folder module `x/index.vlt`,
//!   `x/index.ts` or `x/index.tsx`, relative to the importing file (two of a kind existing at once
//!   is an ambiguity error); `"./x.ts"` (any source extension) names exactly that file, and
//!   `"./x.js"` / `"./x.jsx"` the `.ts` / `.tsx` file, as in TypeScript ([`spec`], [`locate`]);
//! - `"std/x"` → `<std root>/x.vlt` or `<std root>/x/index.vlt` ([`std_root`]);
//! - a `paths` alias of the importing package (`"@app/*": "src/*"` in `package.vlt`) → the
//!   aliased file, like a relative import ([`PackageResolver::path_alias`]);
//! - `"pkg"` / `"pkg/sub"` → `src/lib.vlt` / `src/sub.vlt` (or `src/sub/index.vlt`; `.ts` and
//!   `.tsx` as for relative imports) of a dependency of the importing package, found through a
//!   [`PackageResolver`] (vpm's installed package graph).
//!
//! The names an import spells must match the files on disk in case, on every OS ([`case`]); a
//! file module hiding a folder module of another extension gets a warning ([`pick`]).
//!
//! Local export lists (`export { a, b };`, an import item with an empty specifier) load nothing.
//! A module containing JSX also imports its JSX runtime ([`jsx`]); JSX in a `.ts` file is an
//! error (TypeScript allows it only in `.tsx` files).
//!
//! Modules are deduplicated by canonical file path, so import cycles simply reuse the already
//! loaded module (sema handles cyclic references between functions and types).
//!
//! An optional in-memory overlay (the language server's unsaved editor buffers) takes precedence
//! over the file system for every read, including files that do not exist on disk yet.

mod case;
mod jsx;
mod locate;
mod pick;
mod resolve;
mod spec;
mod std_root;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use velt_common::{Diagnostic, Diagnostics, SourceMap, Span};
use velt_sema::SourceModule;
use velt_syntax::ast;

pub use locate::{module_path, Origin, PackageResolver};
pub use resolve::resolve_module;
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
    load_with_roots(sm, root, &[], opts, diags)
}

/// [`load_program`] plus `extra_roots`: further files loaded as if the root imported them
/// (`velt check` in a package loads every module under `src/` and `tests/`), so modules they
/// share with the root and with each other are loaded once. Their canonical paths are relative
/// to the root's directory, like the root's own relative imports. A module loaded for an extra
/// root whose module path is taken or reserved gets a fallback path no import can name
/// ([`Loader::fallback_name`]): those paths differ from the ones `velt build` and `velt test`
/// give the module, so a clash here is not one there. An extra root that cannot be read is
/// reported in `diags`, located in that file.
pub fn load_with_roots(
    sm: &mut SourceMap,
    root: &Path,
    extra_roots: &[PathBuf],
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
        extra_phase: false,
        dir_names: case::DirNames::default(),
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
    let origin = Origin::Root(dir);
    let root_index = loader.add(root, file_key(root), src, "main".into(), origin.clone());
    let mut queue: VecDeque<usize> = (0..loader.modules.len()).collect();
    loader.resolve_all(&mut queue);
    loader.extra_phase = true;
    for file in extra_roots {
        loader.add_extra_root(file, &origin, &mut queue);
        loader.resolve_all(&mut queue);
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
    /// Loading extra roots: a taken or reserved module path gets a fallback instead of an error.
    extra_phase: bool,
    /// Directory listings, to match file names in case ([`case`]).
    dir_names: case::DirNames,
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
        let src = &self.sm.get(file).src;
        let (ast, parse_diags) = if vpm::sources::is_plain_ts(path) {
            velt_syntax::parse_ts_file(file, src)
        } else {
            velt_syntax::parse_file(file, src)
        };
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

    /// Resolve the imports of every queued module, and of the modules that loads.
    fn resolve_all(&mut self, queue: &mut VecDeque<usize>) {
        while let Some(index) = queue.pop_front() {
            self.resolve_imports(index, queue);
        }
    }

    /// Load `file` as an extra root with `origin` (the root's), unless it is already loaded.
    fn add_extra_root(&mut self, file: &Path, origin: &Origin, queue: &mut VecDeque<usize>) {
        let key = file_key(file);
        if self.by_file.contains_key(&key) {
            return;
        }
        let natural = origin.canonical(file);
        let canonical = if is_std_path(&natural) || self.is_taken(&natural) {
            self.fallback_name(&natural)
        } else {
            natural
        };
        match self.read(file) {
            Ok(src) => {
                let index = self.add(file, key, src, canonical, origin.clone());
                queue.push_back(index);
            }
            Err(e) => {
                // Located in the file itself (registered empty), so the location names it.
                let shown = shown_in_package(file);
                let span = Span::new(self.sm.add(file, String::new()), 0, 0);
                self.error(format!("cannot read `{shown}`: {e}"), vec![], span);
            }
        }
    }

    /// Whether a loaded module has module path `path`.
    fn is_taken(&self, path: &str) -> bool {
        self.modules.iter().any(|m| m.path == path)
    }

    /// A unique module path for a module whose natural path `natural` is taken or reserved:
    /// `#natural` (then `#natural#2`, …). Imports resolve to files, never to these names, and
    /// diagnostics show the file's path, so the name appears nowhere a user writes or reads it.
    fn fallback_name(&self, natural: &str) -> String {
        let base = format!("#{natural}");
        let mut name = base.clone();
        let mut n = 2;
        while self.is_taken(&name) {
            name = format!("{base}#{n}");
            n += 1;
        }
        name
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
        let file = &self.origins[index].0;
        if vpm::sources::is_plain_ts(file) {
            let tsx = file.with_extension("tsx");
            let note = format!(
                "TypeScript allows JSX only in `.tsx` files: rename it to `{}`",
                tsx.file_name().unwrap_or_default().to_string_lossy()
            );
            self.error(
                "JSX is not allowed in a `.ts` file".to_string(),
                vec![note],
                span,
            );
        }
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
        let shown = pick::Shown {
            dir,
            relative: aliased || spec.starts_with('.'),
        };
        let file = self.pick_file(spec, span, &target, &shown)?;
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
        let reserved = !matches!(target.origin, Origin::Std(_)) && is_std_path(&canonical);
        let canonical = if self.extra_phase && (reserved || self.is_taken(&canonical)) {
            self.fallback_name(&canonical)
        } else {
            canonical
        };
        if reserved && !self.extra_phase {
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
        if self.is_taken(&canonical) {
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
                let rel = module_path(&rel);
                Err(format!(
                    "module `{spec}` is a file of the standard library: import it as `velt:{rel}`"
                ))
            }
            _ => Ok(()),
        }
    }

    /// The path a `paths` alias maps `spec` to, resolved like a relative import's (bare
    /// specifiers only: relative and `velt:` imports are never aliased).
    fn path_alias(&self, importer: &Path, spec: &str) -> Option<PathBuf> {
        if spec.starts_with("./") || spec.starts_with("../") || spec.starts_with("velt:") {
            return None;
        }
        let file = self.opts.packages?.path_alias(importer, spec)?;
        Some(vpm::relpath::normalize(&file))
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

/// `file` as shown in a message: relative to its package's root (the directory with
/// `package.vlt`), or as given outside a package.
fn shown_in_package(file: &Path) -> String {
    let dir = file.parent().unwrap_or(Path::new(""));
    match vpm::manifest::find_package_root(dir) {
        Some(root) => vpm::relpath::relative(&vpm::relpath::absolute(file), &root),
        None => file.display().to_string(),
    }
}

/// Identity of a file for deduplication (canonical path when it exists).
fn file_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| vpm::relpath::absolute(path))
}

/// How a message names `file`, a candidate for an import in directory `dir`: relative to `dir`
/// for relative (and path alias) imports (`dup.ts`, `../lib/x.vlt`), the full path otherwise.
fn shown_path(file: &Path, dir: &Path, relative: bool) -> String {
    if relative {
        vpm::relpath::relative(file, dir)
    } else {
        file.display().to_string()
    }
}

#[cfg(test)]
mod case_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod ts_tests;
