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

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use velt_common::{Diagnostic, FileId, Span};
use velt_syntax::ast::{self, ExprKind, ItemKind, Lit, PatternKind};

use super::conditional::Resolved;
use super::Loader;

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

/// The names `export *` brings into a file: name → the (file, name) each comes from (more than
/// one: the name is ambiguous unless they agree), or why they cannot be listed.
pub(super) type StarResult = Rc<Result<BTreeMap<String, Vec<(PathBuf, String)>>, String>>;

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
        if let Some(star) = table.stars.first() {
            let span = star.span;
            match &*self.star_names(file, &r.base) {
                Err(why) => {
                    let msg = format!("`{}`: variant `{target}` cannot be compared: {why}", r.name);
                    self.diags
                        .push(Diagnostic::error(msg, span).with_note(SAME_API));
                    return None;
                }
                Ok(names) => {
                    for (name, sources) in names {
                        if !table.names.contains_key(name) {
                            let entry = self.star_entry(name, sources, &r.base);
                            entries.push((name.clone(), entry, span));
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
        let mut on_path: HashSet<(PathBuf, String)> = HashSet::new();
        let result = loop {
            let key = (super::file_key(&file), name.clone());
            if let Some(done) = self.variant_sigs.get(&key) {
                break done.clone();
            }
            if !on_path.insert(key.clone()) {
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
                    None => match &*self.star_names(&file, base) {
                        Err(why) => break Err(why.clone()),
                        Ok(names) => match names.get(&name).map(|s| s.as_slice()) {
                            None => break Err(format!("`{shown}` does not export `{name}`")),
                            Some([one]) => one.clone(),
                            Some(many) => {
                                let many = many.to_vec();
                                match self.star_entry(&name, &many, base) {
                                    Entry::Sig(sig) => break Ok(sig),
                                    _ => break Err(ambiguous(&name, &many, base)),
                                }
                            }
                        },
                    },
                },
            };
            (file, name) = next;
        };
        for key in path {
            self.variant_sigs.insert(key, result.clone());
        }
        result
    }

    /// The names the `export *` statements of `file` bring in (see [`StarResult`]), once per
    /// load: the files they reach, breadth first with a visited set (a cycle of stars ends), each
    /// file's own exports but `default`; a name found at a smaller depth hides the same name
    /// deeper (a file's own exports win over what its stars bring), and two sources at one depth
    /// make it ambiguous. A star from outside the package cannot be listed: an error.
    fn star_names(&mut self, file: &Path, base: &Path) -> StarResult {
        let key = super::file_key(file);
        if let Some(done) = self.variant_stars.get(&key) {
            return done.clone();
        }
        let result = Rc::new(self.read_star_names(file, base));
        self.variant_stars.insert(key, result.clone());
        result
    }

    fn read_star_names(
        &mut self,
        file: &Path,
        base: &Path,
    ) -> Result<BTreeMap<String, Vec<(PathBuf, String)>>, String> {
        let mut found: BTreeMap<String, (usize, Vec<(PathBuf, String)>)> = BTreeMap::new();
        let mut seen: HashSet<PathBuf> = HashSet::from([super::file_key(file)]);
        let mut queue: std::collections::VecDeque<(PathBuf, usize)> =
            std::collections::VecDeque::from([(file.to_path_buf(), 0)]);
        while let Some((at, depth)) = queue.pop_front() {
            let table = self.table(&at);
            let shown = vpm::relpath::relative(&at, base);
            let t = match &*table {
                Ok(t) => t,
                Err((why, _)) => return Err(format!("`{shown}` {why}")),
            };
            if depth > 0 {
                for name in t.names.keys().filter(|n| *n != "default") {
                    let (at_depth, sources) =
                        found.entry(name.clone()).or_insert_with(|| (depth, vec![]));
                    let source = (super::file_key(&at), name.clone());
                    if *at_depth == depth && !sources.contains(&source) {
                        sources.push(source);
                    }
                }
            }
            for star in &t.stars {
                let Some(next) = &star.file else {
                    return Err(format!(
                        "`export * from \"{}\"` in `{shown}` brings in names that cannot be listed here: list them",
                        star.spec
                    ));
                };
                if seen.insert(super::file_key(next)) {
                    queue.push_back((next.clone(), depth + 1));
                }
            }
        }
        Ok(found.into_iter().map(|(n, (_, s))| (n, s)).collect())
    }

    /// The entry for `name`, which `export *` brings in from `sources`: a link to the one source,
    /// the signature when several agree, an ambiguity (an unresolvable link) when they don't.
    fn star_entry(&mut self, name: &str, sources: &[(PathBuf, String)], base: &Path) -> Entry {
        let link = |(file, name): &(PathBuf, String)| Entry::Link {
            file: Some(file.clone()),
            spec: String::new(),
            name: name.clone(),
        };
        if let [one] = sources {
            return link(one);
        }
        let sigs: Vec<Result<String, String>> = (sources.iter())
            .map(|s| self.entry_sig(&link(s), base))
            .collect();
        match sigs.first() {
            Some(Ok(first)) if sigs.iter().all(|s| s.as_ref().ok() == Some(first)) => {
                Entry::Sig(first.clone())
            }
            _ => Entry::Ambiguous(ambiguous(name, sources, base)),
        }
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

/// Why `name` is ambiguous: `export *` brings it in from several files that disagree.
fn ambiguous(name: &str, sources: &[(PathBuf, String)], base: &Path) -> String {
    let files: Vec<String> = (sources.iter())
        .map(|(f, _)| format!("`{}`", vpm::relpath::relative(f, base)))
        .collect();
    format!(
        "`{name}` is ambiguous: `export *` brings it in from {} with different signatures",
        files.join(" and ")
    )
}

/// The note on every mismatch.
const SAME_API: &str = "the variants of a conditional import export the same names with the \
                        same signatures: a build uses one of them";

/// The name an item declares, if one.
fn item_name(item: &ast::Item) -> Option<&str> {
    Some(match &item.kind {
        ItemKind::Function(f) => &f.sig.name.name,
        ItemKind::ExternFn(sig) => &sig.name.name,
        ItemKind::Struct(t) | ItemKind::Class(t) => &t.name.name,
        ItemKind::Interface(i) => &i.name.name,
        ItemKind::Enum(e) => &e.name.name,
        ItemKind::TypeAlias(a) => &a.name.name,
        ItemKind::Namespace(n) => &n.name.name,
        ItemKind::Var(v) => match &v.pattern.kind {
            PatternKind::Ident(id) => &id.name,
            _ => return None,
        },
        _ => return None,
    })
}

fn name_span(item: &ast::Item) -> Option<Span> {
    Some(match &item.kind {
        ItemKind::Function(f) => f.sig.name.span,
        ItemKind::ExternFn(sig) => sig.name.span,
        ItemKind::Struct(t) | ItemKind::Class(t) => t.name.span,
        ItemKind::Interface(i) => i.name.span,
        ItemKind::Enum(e) => e.name.span,
        ItemKind::TypeAlias(a) => a.name.span,
        ItemKind::Namespace(n) => n.name.span,
        ItemKind::Var(v) => v.pattern.span,
        _ => return None,
    })
}

/// The signature of a declaration ([`variants`](self) says what that is).
fn signature(item: &ast::Item, src: &str) -> String {
    let text = |span: Span| normalize(src.get(span.lo as usize..span.hi as usize).unwrap_or(""));
    match &item.kind {
        ItemKind::Function(f) if !f.overloads.is_empty() => f
            .overloads
            .iter()
            .map(|o| fn_sig(o, src))
            .collect::<Vec<_>>()
            .join("; "),
        ItemKind::Function(f) => fn_sig(&f.sig, src),
        ItemKind::ExternFn(sig) => fn_sig(sig, src),
        ItemKind::Struct(t) | ItemKind::Class(t) => {
            let kind = if matches!(item.kind, ItemKind::Struct(_)) {
                "struct"
            } else {
                "class"
            };
            type_sig(kind, t, item, src)
        }
        ItemKind::Var(v) => {
            let ty = match (&v.ty, &v.init) {
                (Some(ty), _) => text(ty.span),
                (None, Some(e)) => match &e.kind {
                    ExprKind::Lit(Lit::Str(_)) => "string".into(),
                    ExprKind::Lit(Lit::Bool(_)) => "boolean".into(),
                    ExprKind::Lit(Lit::Int { .. } | Lit::Float { .. }) => "number".into(),
                    _ => format!("typeof ({})", text(e.span)),
                },
                (None, None) => "unknown".into(),
            };
            format!(
                "{} {}: {ty}",
                v.kind.keyword(),
                item_name(item).unwrap_or("")
            )
        }
        _ => text(item.span)
            .trim_start_matches("export")
            .trim_start()
            .to_string(),
    }
}

/// `async name<T>(a: A): R throws E`, from the name on (a `declare` function and one with a body
/// have the same signature).
fn fn_sig(sig: &ast::FnSig, src: &str) -> String {
    let text = src
        .get(sig.name.span.lo as usize..sig.span.hi as usize)
        .unwrap_or("");
    let text = normalize(text.trim_end().trim_end_matches(';'));
    let star = if sig.is_generator { "*" } else { "" };
    let async_ = if sig.is_async { "async " } else { "" };
    format!("{async_}function{star} {text}")
}

/// A class's or struct's heading and its non-private members, without bodies.
fn type_sig(kind: &str, t: &ast::TypeDecl, item: &ast::Item, src: &str) -> String {
    let rest = src
        .get(t.name.span.lo as usize..item.span.hi as usize)
        .unwrap_or("");
    let heading = normalize(&rest[..body_start(rest)]);
    let abstract_ = if t.is_abstract { "abstract " } else { "" };
    let mut members = vec![];
    if let (Some(ctor), ast::CtorVisibility::Public) = (&t.constructor, t.ctor_visibility) {
        let params: Vec<String> = ctor
            .sig
            .params
            .iter()
            .map(|p| {
                normalize(
                    src.get(p.span.lo as usize..p.span.hi as usize)
                        .unwrap_or(""),
                )
            })
            .collect();
        members.push(format!("constructor({})", params.join(", ")));
    }
    for f in t.fields.iter().filter(|f| !f.is_private) {
        let ty = normalize(
            src.get(f.ty.span.lo as usize..f.ty.span.hi as usize)
                .unwrap_or(""),
        );
        let flags = format!(
            "{}{}",
            if f.is_static { "static " } else { "" },
            if f.readonly { "readonly " } else { "" }
        );
        let opt = if f.optional { "?" } else { "" };
        members.push(format!("{flags}{}{opt}: {ty}", f.name.name));
    }
    for m in t.methods.iter().filter(|m| !m.is_private) {
        let flags = [
            (m.is_static, "static "),
            (m.is_abstract, "abstract "),
            (m.is_getter, "get "),
            (m.is_setter, "set "),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, s)| *s)
        .collect::<String>();
        let sig = fn_sig(&m.decl.sig, src);
        members.push(format!("{flags}{}", sig.trim_start_matches("function ")));
    }
    format!("{abstract_}{kind} {heading} {{ {} }}", members.join("; "))
}

/// Where a class's body starts in `text` (its heading on): the first `{` outside `<…>` and
/// `(…)`, so an object type in a bound (`<T extends { id: i64 }>`) or a type argument
/// (`extends Base<{ a: T }>`) stays part of the heading.
fn body_start(text: &str) -> usize {
    let mut depth = 0usize;
    let mut prev = ' ';
    for (i, c) in text.char_indices() {
        let arrow = c == '>' && prev == '=';
        prev = c;
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' if arrow => {}
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            '{' if depth == 0 => return i,
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    text.len()
}

/// `text` without comments, and with whitespace only where it separates two words: `a : T`,
/// `a: T` and `a:T` read the same. A separator before a closing bracket goes too (`(a, b,)`).
fn normalize(text: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
                space = true;
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
                space = true;
            }
            c if c.is_whitespace() => space = true,
            '"' | '\'' | '`' => {
                if space && out.ends_with(word) {
                    out.push(' ');
                }
                space = false;
                out.push(c);
                let mut escaped = false;
                for d in chars.by_ref() {
                    out.push(d);
                    if !escaped && d == c {
                        break;
                    }
                    escaped = !escaped && d == '\\';
                }
            }
            c => {
                if matches!(c, ')' | ']' | '}' | '>') && (out.ends_with(',') || out.ends_with(';'))
                {
                    out.pop();
                }
                if space && word(c) && out.ends_with(word) {
                    out.push(' ');
                }
                space = false;
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalize_ignores_layout_and_comments() {
        assert_eq!(
            normalize(
                "send(req : Request, /* the body */ opts ?: Options,)\n  : Promise<Response> // x"
            ),
            "send(req:Request,opts?:Options):Promise<Response>"
        );
        assert_eq!(normalize("async   function f"), "async function f");
        assert_eq!(normalize("x: \"a  //b\""), "x:\"a  //b\"");
        assert_ne!(normalize("f(a: i64)"), normalize("f(a: f64)"));
    }
}
