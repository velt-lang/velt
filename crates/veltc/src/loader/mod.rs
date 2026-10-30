//! Module loading: root file → every module the program needs, parsed, as [`SourceModule`]s for sema.
//!
//! Order of the result: the prelude (`std/prelude/*.vlt` in the order `std/prelude/index.vlt`
//! lists them, canonical `"std/prelude/<name>"`), then
//! the root (`"main"`), then imported modules in breadth-first discovery order, then any extra
//! roots ([`load_with_roots`]) not loaded yet, each followed by what it imports. A global loaded on
//! demand (`std/prelude/global/*.vlt`, [`globals`]) joins that order where a module first
//! mentions one of its names. Specifiers resolve as
//! - `"./x"`, `"../x"` → `x.vlt`, `x.ts` or `x.tsx`, else the folder module `x/index.vlt`,
//!   `x/index.ts` or `x/index.tsx`, relative to the importing file (two of a kind existing at once
//!   is an ambiguity error); `"./x.ts"` (any source extension) names exactly that file, and
//!   `"./x.js"` / `"./x.jsx"` the `.ts` / `.tsx` file, as in TypeScript ([`spec`], [`locate`]);
//! - `"std/x"` → `<std root>/x.vlt` or `<std root>/x/index.vlt` ([`std_root`]);
//! - a `paths` alias of the importing package (`"@app/*": "src/*"` in `package.vlt`) → the
//!   aliased file, like a relative import ([`PackageResolver::path_alias`]);
//! - `"pkg"` / `"pkg/sub"` → `src/lib.vlt` / `src/sub.vlt` (or `src/sub/index.vlt`; `.ts` and
//!   `.tsx` as for relative imports) of a dependency of the importing package, found through a
//!   [`PackageResolver`] (vpm's installed package graph);
//! - any other bare specifier (`"@sigx/reactivity"`, `"sigx/internals"`) → the TypeScript source
//!   of an npm package in `node_modules` or the workspace, as its `package.json` `exports` name
//!   it ([`vpm::npm`]).
//!
//! Non-std modules parse with the root package's `define` constants, and ambient
//! `declare const`s come from modules and their packages' `.d.ts` files ([`ambient`]).
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
//!
//! The loader looks at the file system only through [`vpm::probe`] (as the npm resolver does),
//! so a build can record what decided how its imports resolved (the build stamp).

mod ambient;
mod case;
mod globals;
mod import_meta;
mod jsx;
mod locate;
mod pick;
mod resolve;
mod spec;
mod std_root;
mod visibility;
mod wanted;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use velt_common::{std_modules, Diagnostic, Diagnostics, SourceMap, Span};
use velt_sema::SourceModule;
use velt_syntax::ast;

pub use locate::{module_path, Origin, PackageResolver};
pub use resolve::resolve_module;
pub use spec::{resolve_spec, ModuleRef};
pub use std_root::{in_std, prelude_files, std_root};

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
    /// A root inside [`LoadOptions::std_root`] is loaded as the std module it is (`velt/fs`), not
    /// as `main`: the language server, for a std file opened in the editor, so it is checked like
    /// std code (runtime declarations, std's own types) instead of as a user program.
    pub std_root_as_std: bool,
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
        globals: vec![],
        npm: vpm::npm::NpmResolver::default(),
        ambient: ambient::Ambients::new(opts.packages),
        global_vars: vec![],
        wanted: wanted::WantedByModule::default(),
        resolved: std::collections::HashSet::new(),
        effects: HashMap::new(),
    };
    if let Some(std) = &opts.std_root {
        match prelude_files(std) {
            Ok(files) => {
                for file in files {
                    loader.load_prelude(&file, std);
                }
            }
            Err(e) => loader.diags.push(Diagnostic::error(e, Span::DUMMY)),
        }
        loader.globals = globals::global_modules(std);
    }
    let src = match &opts.root_source {
        Some(src) => src.clone(),
        None => loader
            .read(root)
            .map_err(|e| format!("cannot read `{}`: {e}", root.display()))?,
    };
    let dir = root.parent().unwrap_or(Path::new("")).to_path_buf();
    let origin = Origin::Root(dir);
    let key = file_key(root);
    let std_origin = match (&opts.std_root, &loader.std_key) {
        (Some(std), Some(std_key)) if opts.std_root_as_std && key.starts_with(std_key) => {
            Some(Origin::Std(std.clone()))
        }
        _ => None,
    };
    let root_index = match std_origin {
        // A prelude file opened in the editor is already loaded.
        Some(_) if loader.by_file.contains_key(&key) => loader.by_file[&key],
        Some(std) => {
            let canonical = std.canonical(root);
            loader.add(root, key, src, canonical, std)
        }
        None => loader.add(root, key, src, "main".into(), origin.clone()),
    };
    let mut queue: VecDeque<usize> = (0..loader.modules.len()).collect();
    loader.resolve_all(&mut queue);
    loader.extra_phase = true;
    for file in extra_roots {
        loader.add_extra_root(file, &origin, &mut queue);
        loader.resolve_all(&mut queue);
    }
    loader.check_ambients();
    loader.declare_global_types();
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
    /// The globals loaded on demand ([`globals`]) not loaded yet.
    globals: Vec<globals::Global>,
    /// npm packages found so far ([`vpm::npm`]).
    npm: vpm::npm::NpmResolver,
    /// Build-time constants and ambient declarations ([`ambient`]).
    ambient: ambient::Ambients,
    /// The typed `var`s of each module's `declare global { … }`, by module index
    /// ([`ambient::global_vars`]).
    global_vars: Vec<(usize, Vec<velt_syntax::Ambient>)>,
    /// What importers ask of each module, so a std module's re-exports load only for the names
    /// a program imports ([`wanted`]).
    wanted: wanted::WantedByModule,
    /// The modules whose imports were resolved (a std module asked for more names later is
    /// resolved again, for the re-exports that adds).
    resolved: std::collections::HashSet<usize>,
    /// Per std file not loaded: whether it has effects beyond its exports ([`wanted::has_effects`]).
    effects: HashMap<PathBuf, bool>,
}

impl Loader<'_, '_> {
    /// Contents of `path`: the overlay's if it has the file, else the disk's.
    fn read(&self, path: &Path) -> std::io::Result<String> {
        // `file_key` costs a filesystem round trip (a few hundred µs on Windows).
        if self.overlay.is_empty() {
            return vpm::probe::read_to_string(path);
        }
        match self.overlay.get(&file_key(path)) {
            Some(src) => Ok(src.clone()),
            None => vpm::probe::read_to_string(path),
        }
    }

    /// Load prelude module `file`; returns its index (`None` after reporting a read error).
    fn load_prelude(&mut self, file: &Path, std: &Path) -> Option<usize> {
        match self.read(file) {
            Ok(src) => {
                let origin = Origin::Std(std.to_path_buf());
                let canonical = origin.canonical(file);
                Some(self.add(file, file_key(file), src, canonical, origin))
            }
            Err(e) => {
                self.diags.push(Diagnostic::error(
                    format!("cannot read prelude `{}`: {e}", file.display()),
                    Span::DUMMY,
                ));
                None
            }
        }
    }

    /// Load the globals ([`globals`]) that non-std module `index` mentions.
    fn load_globals(&mut self, index: usize, queue: &mut VecDeque<usize>) {
        if self.modules[index].is_std || self.globals.is_empty() {
            return;
        }
        let src = &self.sm.get(self.modules[index].file).src;
        let ast = &self.modules[index].ast;
        let own = globals::bound_names(ast);
        let (wanted, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.globals)
            .into_iter()
            .partition(|g| {
                g.names
                    .iter()
                    .any(|n| !own.contains(n.as_str()) && globals::names_global(src, ast, n))
            });
        self.globals = rest;
        let Some(std) = self.opts.std_root.clone() else {
            return;
        };
        for g in wanted {
            if let Some(new) = self.load_prelude(&g.file, &std) {
                queue.push_back(new);
            }
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
        let is_std = matches!(origin, Origin::Std(_));
        let plain_ts = vpm::sources::is_plain_ts(path);
        let mut ast = if is_std {
            let src = &self.sm.get(file).src;
            let (ast, parse_diags) = if plain_ts {
                velt_syntax::parse_ts_file(file, src)
            } else {
                velt_syntax::parse_file(file, src)
            };
            self.diags.extend(parse_diags);
            ast
        } else {
            self.ambient.read_declarations(self.sm, path);
            let opts = velt_syntax::ParseOptions {
                plain_ts,
                defines: self.ambient.defines(),
            };
            let parsed = velt_syntax::parse_with(file, &self.sm.get(file).src, &opts);
            self.diags.extend(parsed.diags);
            let typed = ambient::global_vars(&parsed.ambient);
            if !typed.is_empty() {
                self.global_vars.push((self.modules.len(), typed));
            }
            self.ambient
                .declared(parsed.ambient, file, parsed.is_module);
            parsed.module
        };
        import_meta::fill(&mut ast, path, canonical == "main");
        let index = self.modules.len();
        self.by_file.insert(key, index);
        let is_dependency = matches!(origin, Origin::Package { .. });
        self.modules.push(SourceModule {
            path: canonical,
            is_std,
            is_dependency,
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
        loop {
            while let Some(index) = queue.pop_front() {
                self.resolve_imports(index, queue);
            }
            // Conditional globals once everything else is loaded: what they add may load more.
            if !self.load_conditional_globals(queue) {
                break;
            }
        }
    }

    /// Load the conditional globals ([`globals`]) whose std module is loaded and whose words a
    /// non-std module mentions; whether any was.
    fn load_conditional_globals(&mut self, queue: &mut VecDeque<usize>) -> bool {
        let Some(std) = self.opts.std_root.clone() else {
            return false;
        };
        let (modules, sm) = (&self.modules, &*self.sm);
        let due = |g: &globals::Global| {
            let Some((module, words)) = &g.when else {
                return false;
            };
            modules.iter().any(|m| m.is_std && m.path == *module)
                && modules.iter().filter(|m| !m.is_std).any(|m| {
                    let src = &sm.get(m.file).src;
                    words.iter().any(|w| globals::mentions_word(src, &m.ast, w))
                })
        };
        let (wanted, rest): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.globals).into_iter().partition(due);
        self.globals = rest;
        let mut loaded = false;
        for g in wanted {
            if let Some(new) = self.load_prelude(&g.file, &std) {
                queue.push_back(new);
                loaded = true;
            }
        }
        loaded
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

    /// Give the typed `var`s of every module's `declare global` to the rewrite of `globalThis`
    /// ([`ambient::declare_global_types`]), when a module of the program names it (a program
    /// that doesn't has no use for them).
    fn declare_global_types(&mut self) {
        let vars = std::mem::take(&mut self.global_vars);
        let (modules, sm) = (&self.modules, &*self.sm);
        let names = |m: &SourceModule| {
            let src = &sm.get(m.file).src;
            globals::mentions(src, "globalThis")
                || src.contains("global.")
                || src.contains("global[")
        };
        if vars.is_empty() || !modules.iter().any(|m| !m.is_std && names(m)) {
            return;
        }
        for (i, vars) in vars {
            ambient::declare_global_types(&mut self.modules[i].ast, &vars);
        }
    }

    /// Report the ambient constants that modules use but nothing gives a value
    /// ([`ambient::Ambients::check`]).
    fn check_ambients(&mut self) {
        let (modules, sm) = (&self.modules, &*self.sm);
        let mentioned = |name: &str, owner: Option<velt_common::FileId>| {
            modules
                .iter()
                .filter(|m| !m.is_std && owner.is_none_or(|f| f == m.file))
                .any(|m| {
                    globals::mentions(&sm.get(m.file).src, name)
                        && !velt_syntax::scope::free_values(&m.ast, &|n| n == name).is_empty()
                })
        };
        let found = self.ambient.check(&mentioned);
        self.diags.extend(found);
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
    /// A std module's re-exports are followed only for the names asked of it ([`wanted`]).
    fn resolve_imports(&mut self, index: usize, queue: &mut VecDeque<usize>) {
        let is_std = self.modules[index].is_std;
        let items: Vec<(ast::Import, bool)> = (self.modules[index].ast.items.iter())
            .filter_map(|item| match &item.kind {
                ast::ItemKind::Import(imp) if !imp.from.is_empty() => {
                    Some((imp.clone(), item.exported))
                }
                // `declare module "spec" { … }` augments that module: it is loaded as an import.
                ast::ItemKind::Augment(ast::Augment {
                    module: Some((spec, span)),
                    ..
                }) => Some((wanted::augmented(spec, *span), false)),
                _ => None,
            })
            .collect();
        let mut imports: Vec<(String, Span, Option<Vec<String>>, bool)> = vec![];
        for (imp, exported) in items {
            let skip = exported && is_std && !self.wanted.needs(index, &imp);
            if skip && self.pure(index, &imp.from) {
                continue;
            }
            let written = !velt_syntax::is_synthesized_import(&imp);
            imports.push((
                imp.from.clone(),
                imp.from_span,
                wanted::names_taken(&imp),
                written,
            ));
        }
        let first = self.resolved.insert(index);
        for (spec, span, names, written) in imports {
            let known = self.modules[index].imports.iter().find(|(s, _)| *s == spec);
            let target = match known {
                Some((_, path)) => self.modules.iter().position(|m| m.path == *path),
                None => {
                    let target = self.import(index, &spec, span, written, queue);
                    if let Some(t) = target {
                        let canonical = self.modules[t].path.clone();
                        self.modules[index].imports.push((spec, canonical));
                    }
                    target
                }
            };
            if let Some(t) = target {
                if self.wanted.request(t, names) && self.resolved.contains(&t) && t != index {
                    queue.push_back(t);
                }
            }
        }
        if first {
            self.jsx_runtime(index, queue);
            self.load_globals(index, queue);
        }
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
        match self.import(index, &spec, span, true, queue) {
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

    /// Locate (and load if new) the module `spec` imported by module `index`; `written`: the
    /// import is the source's, so std's visibility rule applies ([`visibility`]).
    fn import(
        &mut self,
        index: usize,
        spec: &str,
        span: Span,
        written: bool,
        queue: &mut VecDeque<usize>,
    ) -> Option<usize> {
        let (importer, from) = self.origins[index].clone();
        let dir = importer.parent().unwrap_or(Path::new(""));
        let alias = self.path_alias(&importer, spec);
        let mut aliased = alias.is_some();
        // No npm package was found for the specifier: an error that follows says why the
        // workspace walk may have missed it.
        let mut npm_missed = false;
        let module = match alias {
            Some(file) => ModuleRef::Relative { file },
            None => {
                let classified = resolve_spec(spec, dir);
                match npm_module(
                    &mut self.npm,
                    self.opts.packages,
                    &importer,
                    spec,
                    &classified,
                ) {
                    Some(Ok(file)) => {
                        aliased = true;
                        ModuleRef::Relative { file }
                    }
                    Some(Err((msg, notes))) => return self.error(msg, notes, span),
                    None => {
                        npm_missed = matches!(classified, Ok(ModuleRef::Package { .. }));
                        match classified {
                            Ok(m) => m,
                            Err(msg) => {
                                let notes = self.npm.workspace_note(&importer);
                                return self.error(msg, notes.into_iter().collect(), span);
                            }
                        }
                    }
                }
            }
        };
        let target = match locate::target(
            module,
            &importer,
            &from,
            self.opts.std_root.as_deref(),
            self.opts.packages,
        ) {
            Ok(t) => t,
            Err((msg, mut notes)) => {
                if npm_missed {
                    notes.extend(self.npm.workspace_note(&importer));
                }
                return self.error(msg, notes, span);
            }
        };
        let shown = pick::Shown {
            dir,
            relative: aliased || spec.starts_with('.'),
        };
        let file = self.pick_file(spec, span, &target, &shown)?;
        let key = file_key(&file);
        let target = self.std_test_origin(target, &file, &key);
        if written {
            if let Err((msg, notes)) = self.visible(&from, &target, &key, spec) {
                return self.error(msg, notes, span);
            }
        }
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

    /// The path a `paths` alias maps `spec` to, resolved like a relative import's (bare
    /// specifiers only: relative and `velt/` imports are never aliased).
    fn path_alias(&self, importer: &Path, spec: &str) -> Option<PathBuf> {
        if spec.starts_with("./")
            || spec.starts_with("../")
            || spec.starts_with(std_modules::SPECIFIER_PREFIX)
        {
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

/// The npm package module bare specifier `spec` names ([`vpm::npm`]), unless the importing
/// package has a Velt dependency of that name (which wins) or `spec` is not bare
/// (`classified`: what [`resolve_spec`] made of it). `None`: no npm package of that name.
fn npm_module(
    npm: &mut vpm::npm::NpmResolver,
    packages: Option<&dyn PackageResolver>,
    importer: &Path,
    spec: &str,
    classified: &Result<ModuleRef, String>,
) -> Option<Result<PathBuf, vpm::npm::NpmError>> {
    let bare = match classified {
        Ok(ModuleRef::Package { name, .. }) => {
            packages.is_none_or(|p| p.dependency_root(importer, name).is_err())
        }
        Ok(_) => false,
        Err(_) => {
            spec != "std"
                && !std_modules::is_std(spec)
                && !spec.starts_with(std_modules::SPECIFIER_PREFIX)
        }
    };
    if !bare {
        return None;
    }
    // A package a git dependency provides, before Node's search of `node_modules`.
    let provided = vpm::npm::split_specifier(spec)
        .and_then(|(name, _)| packages.and_then(|p| p.npm_package(importer, name)));
    if let Some(dir) = provided {
        return npm.resolve_in(&dir, spec);
    }
    npm.resolve(importer, spec)
}

/// Whether canonical module path `path` is in the standard library's namespace (`std`, `std/…`).
fn is_std_path(path: &str) -> bool {
    path == "std" || std_modules::is_std(path)
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
mod globals_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod ts_tests;
#[cfg(test)]
mod visibility_tests;
