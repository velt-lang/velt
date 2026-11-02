//! The variants of one `#name` ([`super::conditional`]) export the same API: the same names, each
//! with the same signature, as written (whitespace and comments aside). A build loads one variant,
//! so without this check a program would compile for one target and not for another.
//!
//! Each variant is parsed, not type-checked: the variant a build does not use may call what only
//! another target has. What a name's signature is:
//! - a function (declared or not): its type parameters, parameters and return type, `async`;
//! - a class or struct: its heading (type parameters, `extends`, `implements`) and its members
//!   other than `private` ones, without bodies;
//! - a type alias, interface, enum or namespace: the whole declaration;
//! - a `const`/`let`: its type annotation (a literal's kind without one);
//! - a re-export: the signature where it is declared, followed through relative re-exports
//!   (each file read, and each (file, name) resolved, once per load); one from another package
//!   or std: that module's specifier and the name. A name that comes from `export *`, or that
//!   no file on its chain declares, is an error: it cannot be compared.
//!
//! The first variant written is the reference the others are compared with. The check runs where
//! [`super::LoadOptions::check_variants`] asks for it (`velt check`, `velt test`, the language
//! server), for every `#name` the program imports.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use velt_common::{Diagnostic, FileId, Span};
use velt_syntax::ast::{self, ItemKind};

use super::signature::{item_name, name_span, signature};

use super::conditional::Resolved;
use super::Loader;

/// The result of [`Loader::star_exports`] before it is shared: see [`StarResult`].
type StarNames = Result<BTreeMap<String, (Result<String, String>, Span)>, (String, Span)>;

/// One exported name of a variant, resolved to its signature.
#[derive(Clone, Debug)]
struct Export {
    /// Its signature, normalized for comparing.
    sig: String,
    /// Where it is exported.
    span: Span,
}

/// A variant's API: name → export.
type Api = BTreeMap<String, Export>;

/// What a file exports under one name, as the file writes it.
#[derive(Clone, Debug)]
enum Entry {
    /// Declared in the file: its signature.
    Sig(String),
    /// Re-exported from a module outside the package (std, a dependency): compared by that
    /// module's specifier and the name.
    Outside(String),
    /// Re-exported from a relative module: its file (`None` when there is none), the specifier,
    /// and the name there.
    Link {
        file: Option<PathBuf>,
        spec: String,
        name: String,
    },
    /// Brought in by `export *` from several files that disagree: why.
    Ambiguous(String),
}

/// A file's exports as written: name → entry and where it is exported, and the specifiers of
/// its `export * from …`.
#[derive(Debug)]
pub(super) struct Table {
    names: BTreeMap<String, (Entry, Span)>,
    stars: Vec<Star>,
}

/// An `export * from "<spec>"` of a file: the file it names (`None` outside the package or when
/// there is none) and where it is written.
#[derive(Debug)]
struct Star {
    spec: String,
    file: Option<PathBuf>,
    span: Span,
}

/// The names `export *` brings into a file: name → its signature (or why it has none: an
/// ambiguous name) and the star it comes through; or why they cannot be listed, at that star.
pub(super) type StarResult = Rc<StarNames>;

/// A file's [`Table`], or why it cannot be read (`reported`: a diagnostic already says why).
pub(super) type TableResult = Rc<Result<Table, (String, bool)>>;

impl Loader<'_, '_> {
    /// Compare the variants of every `#name` the program imported (none unless
    /// [`super::LoadOptions::check_variants`]).
    pub(super) fn check_variants(&mut self) {
        for resolved in std::mem::take(&mut self.variant_checks) {
            self.compare(&resolved);
        }
    }

    fn compare(&mut self, r: &Resolved) {
        // The import's own error says why the build's variant did not load.
        if !self.by_file.contains_key(&super::file_key(&r.selected)) {
            return;
        }
        let mut apis: Vec<(&str, Api)> = vec![];
        let mut failed = false;
        for v in &r.variants {
            let file = vpm::relpath::normalize(&r.base.join(v.target.trim_start_matches("./")));
            // The same file under two conditions is one API.
            if apis.iter().any(|(t, _)| *t == v.target) {
                continue;
            }
            // Every variant's problems are reported; the comparison needs them all.
            match self.api(r, &v.target, &file) {
                Some(api) => apis.push((&v.target, api)),
                None => failed = true,
            }
        }
        if failed {
            return;
        }
        let Some(((first, reference), rest)) = apis.split_first() else {
            return;
        };
        for (other, api) in rest {
            for (name, export) in reference {
                match api.get(name) {
                    None => self.diags.push(
                        Diagnostic::error(
                            format!(
                                "`{}`: `{name}` is exported by `{first}` but not by `{other}`",
                                r.name
                            ),
                            export.span,
                        )
                        .with_note(SAME_API),
                    ),
                    Some(theirs) if theirs.sig != export.sig => self.diags.push(
                        Diagnostic::error(
                            format!(
                                "`{}`: `{name}` has another signature in `{other}` than in `{first}`",
                                r.name
                            ),
                            theirs.span,
                        )
                        .with_label(export.span, format!("the signature in `{first}`"))
                        .with_note(format!("`{first}`: {}", export.sig))
                        .with_note(format!("`{other}`: {}", theirs.sig))
                        .with_note(SAME_API),
                    ),
                    Some(_) => {}
                }
            }
            for (name, export) in api.iter().filter(|(n, _)| !reference.contains_key(*n)) {
                self.diags.push(
                    Diagnostic::error(
                        format!(
                            "`{}`: `{name}` is exported by `{other}` but not by `{first}`",
                            r.name
                        ),
                        export.span,
                    )
                    .with_note(SAME_API),
                );
            }
        }
    }

    /// The API of variant `target` (its file `file`) of `r`: each name it exports with the
    /// signature where it is declared. `None` after reporting why it cannot be compared.
    fn api(&mut self, r: &Resolved, target: &str, file: &Path) -> Option<Api> {
        let table = self.table(file);
        let table = match &*table {
            Ok(t) => t,
            Err((why, reported)) => {
                if !reported {
                    let msg = format!("`{}`: variant `{target}` {why}", r.name);
                    self.diags.push(Diagnostic::error(msg, r.span));
                }
                return None;
            }
        };
        // What `export *` brings in, besides the names the variant writes itself (which win).
        let mut entries: Vec<(String, Entry, Span)> = (table.names.iter())
            .map(|(n, (e, s))| (n.clone(), e.clone(), *s))
            .collect();
        if !table.stars.is_empty() {
            match &*self.star_exports(file, &r.base) {
                Err((why, span)) => {
                    let msg = format!("`{}`: variant `{target}` cannot be compared: {why}", r.name);
                    let span = if *span == Span::DUMMY { r.span } else { *span };
                    self.diags
                        .push(Diagnostic::error(msg, span).with_note(SAME_API));
                    return None;
                }
                Ok(names) => {
                    for (name, (sig, span)) in names {
                        if !table.names.contains_key(name) {
                            let entry = match sig {
                                Ok(sig) => Entry::Sig(sig.clone()),
                                Err(why) => Entry::Ambiguous(why.clone()),
                            };
                            entries.push((name.clone(), entry, *span));
                        }
                    }
                }
            }
        }
        let mut api = Api::new();
        let mut ok = true;
        for (name, entry, span) in &entries {
            match self.entry_sig(entry, &r.base) {
                Ok(sig) => {
                    api.insert(name.clone(), Export { sig, span: *span });
                }
                Err(why) => {
                    ok = false;
                    let msg = format!(
                        "`{}`: `{name}` of `{target}` cannot be compared: {why}",
                        r.name
                    );
                    self.diags
                        .push(Diagnostic::error(msg, *span).with_note(SAME_API));
                    continue;
                }
            }
        }
        ok.then_some(api)
    }

    /// The signature `entry` stands for: its own, or the declaration a chain of relative
    /// re-exports reaches. Each (file, name) on a chain is resolved once per load, iteratively
    /// (a long chain does not deepen the stack); a chain that comes back to a (file, name) on it
    /// is a cycle that declares nothing, an error for every name on it. `base`: what file paths
    /// in messages are relative to.
    fn entry_sig(&mut self, entry: &Entry, base: &Path) -> Result<String, String> {
        let (mut file, mut name) = match entry {
            Entry::Sig(sig) | Entry::Outside(sig) => return Ok(sig.clone()),
            Entry::Ambiguous(why) => return Err(why.clone()),
            Entry::Link {
                file: None, spec, ..
            } => {
                return Err(format!("`{spec}` names no file"));
            }
            Entry::Link {
                file: Some(file),
                name,
                ..
            } => (file.clone(), name.clone()),
        };
        let mut path: Vec<(PathBuf, String)> = vec![];
        let outer = std::mem::replace(&mut self.variant_cut, false);
        let result = loop {
            let key = (super::file_key(&file), name.clone());
            if let Some(done) = self.variant_sigs.get(&key) {
                break done.clone();
            }
            // A (file, name) on this chain, or on one that an `export *` being expanded is
            // resolving: a cycle.
            if !self.variant_busy.insert(key.clone()) {
                self.variant_cut = true;
                break Err(format!(
                    "`{name}` is re-exported in a cycle through `{}` and declared nowhere on it",
                    vpm::relpath::relative(&file, base)
                ));
            }
            path.push(key);
            let table = self.table(&file);
            let shown = vpm::relpath::relative(&file, base);
            let next = match &*table {
                Err((why, _)) => break Err(format!("`{shown}` {why}")),
                Ok(t) => match t.names.get(&name) {
                    Some((Entry::Sig(sig) | Entry::Outside(sig), _)) => break Ok(sig.clone()),
                    Some((Entry::Ambiguous(why), _)) => break Err(why.clone()),
                    Some((
                        Entry::Link {
                            file: None, spec, ..
                        },
                        _,
                    )) => {
                        break Err(format!("`{spec}` (in `{shown}`) names no file"));
                    }
                    Some((
                        Entry::Link {
                            file: Some(f),
                            name: n,
                            ..
                        },
                        _,
                    )) => (f.clone(), n.clone()),
                    None if t.stars.is_empty() => {
                        break Err(format!("`{shown}` does not export `{name}`"))
                    }
                    None => match &*self.star_exports(&file, base) {
                        Err((why, _)) => break Err(why.clone()),
                        Ok(names) => match names.get(&name) {
                            Some((sig, _)) => break sig.clone(),
                            None => break Err(format!("`{shown}` does not export `{name}`")),
                        },
                    },
                },
            };
            (file, name) = next;
        };
        for key in &path {
            self.variant_busy.remove(key);
        }
        let cut = self.end_cut(outer);
        // A result a cycle cut short is this path's, not the name's: not kept.
        if !cut {
            for key in path {
                self.variant_sigs.insert(key, result.clone());
            }
        }
        result
    }

    /// Ends a resolution begun with `outer` saved from `variant_cut`: whether a cycle cut it
    /// short (the enclosing one is then cut short too).
    fn end_cut(&mut self, outer: bool) -> bool {
        let cut = self.variant_cut;
        self.variant_cut = outer || cut;
        cut
    }

    /// The names `export *` brings into `file` (see [`StarResult`]), each with its signature,
    /// once per load. Each star target contributes everything it exports (its own names, then
    /// what its stars bring in) but `default` and the names `file` exports itself. A name that
    /// two stars bring in with different signatures is ambiguous; one whose resolution came back
    /// to a file or name still being resolved (a cycle) is no candidate, as in ES modules'
    /// `resolveSet`. Results that a cycle cut short are not kept, so a later question from
    /// elsewhere in the cycle gets the whole answer.
    fn star_exports(&mut self, file: &Path, base: &Path) -> StarResult {
        let key = super::file_key(file);
        if let Some(done) = self.variant_stars.get(&key) {
            return done.clone();
        }
        if !self.variant_star_busy.insert(key.clone()) {
            self.variant_cut = true;
            return Rc::new(Ok(BTreeMap::new()));
        }
        let outer = std::mem::replace(&mut self.variant_cut, false);
        let result = Rc::new(self.read_star_exports(file, base));
        let cut = self.end_cut(outer);
        self.variant_star_busy.remove(&key);
        if !cut {
            self.variant_stars.insert(key, result.clone());
        }
        result
    }

    fn read_star_exports(&mut self, file: &Path, base: &Path) -> StarNames {
        let table = self.table(file);
        let shown = vpm::relpath::relative(file, base);
        let t = match &*table {
            Ok(t) => t,
            Err((why, _)) => return Err((format!("`{shown}` {why}"), Span::DUMMY)),
        };
        let mut out: BTreeMap<String, (Result<String, String>, Span)> = BTreeMap::new();
        for star in &t.stars {
            let Some(next) = &star.file else {
                let why = if star.spec.starts_with('.') {
                    format!(
                        "`export * from \"{}\"` in `{shown}` names no file",
                        star.spec
                    )
                } else {
                    format!(
                        "`export * from \"{}\"` in `{shown}` brings in names from outside the package, which cannot be listed: list them",
                        star.spec
                    )
                };
                return Err((why, star.span));
            };
            let theirs = self
                .all_exports(next, base)
                .map_err(|(why, _)| (why, star.span))?;
            for (name, (sig, cycle)) in theirs {
                if name == "default" || t.names.contains_key(&name) || (cycle && sig.is_err()) {
                    continue;
                }
                match out.get(&name) {
                    None => {
                        out.insert(name, (sig, star.span));
                    }
                    Some((Ok(have), _)) if sig.as_ref().is_ok_and(|s| s != have) => {
                        let why = format!(
                            "`{name}` is ambiguous: two `export *` of `{shown}` bring it in with different signatures"
                        );
                        out.insert(name, (Err(why), star.span));
                    }
                    Some(_) => {}
                }
            }
        }
        Ok(out)
    }

    /// Everything `file` exports, its own names and what its stars bring in, each with its
    /// signature and whether a cycle cut its resolution short.
    #[allow(clippy::type_complexity)]
    fn all_exports(
        &mut self,
        file: &Path,
        base: &Path,
    ) -> Result<Vec<(String, (Result<String, String>, bool))>, (String, Span)> {
        let table = self.table(file);
        let own: Vec<(String, Entry)> = match &*table {
            Ok(t) => (t.names.iter())
                .map(|(n, (e, _))| (n.clone(), e.clone()))
                .collect(),
            Err((why, _)) => {
                let shown = vpm::relpath::relative(file, base);
                return Err((format!("`{shown}` {why}"), Span::DUMMY));
            }
        };
        let mut out = vec![];
        for (name, entry) in own {
            let outer = std::mem::replace(&mut self.variant_cut, false);
            let sig = self.entry_sig(&entry, base);
            let cut = self.end_cut(outer);
            out.push((name, (sig, cut)));
        }
        let outer = std::mem::replace(&mut self.variant_cut, false);
        let stars = self.star_exports(file, base);
        let cut = self.end_cut(outer);
        match &*stars {
            Ok(names) => {
                out.extend((names.iter()).map(|(n, (sig, _))| (n.clone(), (sig.clone(), cut))))
            }
            Err(e) => return Err(e.clone()),
        }
        Ok(out)
    }

    /// `file`'s exports as written ([`Table`]), read and parsed once per load.
    fn table(&mut self, file: &Path) -> TableResult {
        let key = super::file_key(file);
        if let Some(done) = self.variant_tables.get(&key) {
            return done.clone();
        }
        let table = Rc::new(self.read_table(file));
        self.variant_tables.insert(key, table.clone());
        table
    }

    fn read_table(&mut self, file: &Path) -> Result<Table, (String, bool)> {
        let (id, module) = self.parsed(file)?;
        let src = self.sm.get(id).src.clone();
        let locals: BTreeMap<&str, &ast::Item> = module
            .items
            .iter()
            .filter_map(|item| item_name(item).map(|n| (n, item)))
            .collect();
        let dir = file.parent().unwrap_or(Path::new(""));
        // The bindings the file imports by name: local name → (specifier, name there).
        let imported: BTreeMap<&str, (&str, &str)> = (module.items.iter())
            .filter(|item| !item.exported)
            .filter_map(|item| match &item.kind {
                ItemKind::Import(imp) if !imp.from.is_empty() => Some(imp),
                _ => None,
            })
            .flat_map(|imp| {
                imp.names.iter().map(move |n| {
                    let local = n.alias.as_ref().unwrap_or(&n.name);
                    (
                        local.name.as_str(),
                        (imp.from.as_str(), n.name.name.as_str()),
                    )
                })
            })
            .collect();
        // `name` of module `spec`: a link to its file for a relative one, else the specifier.
        let link = |spec: &str, name: &str| {
            if !(spec.starts_with("./") || spec.starts_with("../")) {
                return Entry::Outside(format!("`{name}` from \"{spec}\""));
            }
            let target = vpm::relpath::normalize(&dir.join(spec));
            let file = super::locate::relative_files(target)
                .into_iter()
                .flatten()
                .find(|f| vpm::probe::is_file(f));
            Entry::Link {
                file,
                spec: spec.to_string(),
                name: name.to_string(),
            }
        };
        let mut table = Table {
            names: BTreeMap::new(),
            stars: vec![],
        };
        for item in module.items.iter().filter(|i| i.exported) {
            match &item.kind {
                ItemKind::Import(imp) if imp.all => {
                    let relative = imp.from.starts_with("./") || imp.from.starts_with("../");
                    let file = relative
                        .then(|| {
                            let target = vpm::relpath::normalize(&dir.join(&imp.from));
                            super::locate::relative_files(target)
                                .into_iter()
                                .flatten()
                                .find(|f| vpm::probe::is_file(f))
                        })
                        .flatten();
                    table.stars.push(Star {
                        spec: imp.from.clone(),
                        file,
                        span: imp.from_span,
                    });
                }
                ItemKind::Import(imp) => {
                    for n in &imp.names {
                        let shown = n.alias.as_ref().unwrap_or(&n.name);
                        let name = &n.name.name;
                        let entry = match (imp.from.is_empty(), locals.get(name.as_str())) {
                            (true, Some(item)) => Entry::Sig(signature(item, &src)),
                            // `export { a }` of a binding the file imports: where it comes from.
                            (true, None) => match imported.get(name.as_str()) {
                                Some((spec, original)) => link(spec, original),
                                None => Entry::Sig(format!("`{name}`")),
                            },
                            (false, _) => link(&imp.from, name),
                        };
                        table.names.insert(shown.name.clone(), (entry, shown.span));
                    }
                }
                _ => {
                    if let Some(name) = item_name(item) {
                        let span = name_span(item).unwrap_or(item.span);
                        let entry = Entry::Sig(signature(item, &src));
                        table.names.insert(name.to_string(), (entry, span));
                    }
                }
            }
        }
        Ok(table)
    }

    /// `file` parsed: the loaded module when the program has it, else read and parsed (its
    /// syntax errors are reported: nothing else checks a variant the build does not use;
    /// `true` in the error: a diagnostic says why).
    fn parsed(&mut self, file: &Path) -> Result<(FileId, ast::Module), (String, bool)> {
        if let Some(&i) = self.by_file.get(&super::file_key(file)) {
            return Ok((self.modules[i].file, self.modules[i].ast.clone()));
        }
        let src = self
            .read(file)
            .map_err(|e| (format!("cannot be read: {e}"), false))?;
        let id = self.sm.add(file, src);
        let (module, diags) = velt_syntax::parse_file(id, &self.sm.get(id).src);
        let failed = diags.iter().any(Diagnostic::is_error);
        self.diags.extend(diags);
        if failed {
            return Err(("has syntax errors".into(), true));
        }
        Ok((id, module))
    }
}

/// The note on every mismatch.
const SAME_API: &str = "the variants of a conditional import export the same names with the \
                        same signatures: a build uses one of them";
