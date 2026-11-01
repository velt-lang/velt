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
//! - a re-export: the signature where it is declared, followed through relative imports.
//!
//! The first variant written is the reference the others are compared with. The check runs where
//! [`super::LoadOptions::check_variants`] asks for it (`velt check`, `velt test`, the language
//! server), for every `#name` the program imports.

use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use velt_common::{Diagnostic, FileId, Span};
use velt_syntax::ast::{self, ExprKind, ItemKind, Lit, PatternKind};

use super::conditional::Resolved;
use super::Loader;

/// One exported name of a variant.
#[derive(Clone, Debug)]
pub(super) struct Export {
    /// Its signature, normalized for comparing.
    sig: String,
    /// Where it is exported.
    span: Span,
}

/// A variant's API: name → export.
type Api = BTreeMap<String, Export>;

/// A file's API, or why it cannot be compared; shared by every re-export that reaches it.
pub(super) type ApiResult = Rc<Result<Api, String>>;

/// What a file's API is while it is being read: a re-export cycle back to it.
const IN_PROGRESS: &str = "re-exports itself through a cycle";

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
        let mut apis: Vec<(&str, ApiResult)> = vec![];
        for v in &r.variants {
            let file = vpm::relpath::normalize(&r.base.join(v.target.trim_start_matches("./")));
            // The same file under two conditions is one API.
            if apis.iter().any(|(t, _)| *t == v.target) {
                continue;
            }
            let api = self.api(&file);
            match &*api {
                Ok(_) => apis.push((&v.target, api)),
                Err(msg) => {
                    let msg = format!("`{}`: variant `{}` {msg}", r.name, v.target);
                    self.diags.push(Diagnostic::error(msg, r.span));
                    return;
                }
            }
        }
        let Some(((first, reference), rest)) = apis.split_first() else {
            return;
        };
        let Ok(reference) = &**reference else {
            return;
        };
        for (other, api) in rest {
            let Ok(api) = &**api else {
                continue;
            };
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

    /// The API of the module `file`, read once per load: a file that re-exports from another
    /// one reached again (many names from one file, or a cycle) costs a lookup, not another
    /// parse. A file still being read when a cycle reaches it again gives [`IN_PROGRESS`].
    fn api(&mut self, file: &Path) -> ApiResult {
        let key = super::file_key(file);
        match self.variant_apis.get(&key) {
            Some(Some(done)) => return done.clone(),
            Some(None) => return Rc::new(Err(IN_PROGRESS.into())),
            None => {}
        }
        self.variant_apis.insert(key.clone(), None);
        let api = Rc::new(self.read_api(file));
        self.variant_apis.insert(key, Some(api.clone()));
        api
    }

    fn read_api(&mut self, file: &Path) -> Result<Api, String> {
        let (id, module) = self.parsed(file)?;
        let src = self.sm.get(id).src.clone();
        let locals: BTreeMap<&str, &ast::Item> = module
            .items
            .iter()
            .filter_map(|item| item_name(item).map(|n| (n, item)))
            .collect();
        let mut api = Api::new();
        for item in module.items.iter().filter(|i| i.exported) {
            match &item.kind {
                ItemKind::Import(imp) if imp.all => {
                    return Err(format!(
                        "re-exports `export * from \"{}\"`: list the names, so the variants' APIs can be compared",
                        imp.from
                    ));
                }
                ItemKind::Import(imp) if imp.from.is_empty() => {
                    for n in &imp.names {
                        let shown = n.alias.as_ref().unwrap_or(&n.name);
                        let sig = match locals.get(n.name.name.as_str()) {
                            Some(local) => signature(local, &src),
                            None => format!("`{}`", n.name.name),
                        };
                        let export = Export {
                            sig,
                            span: shown.span,
                        };
                        api.insert(shown.name.clone(), export);
                    }
                }
                ItemKind::Import(imp) => {
                    for n in &imp.names {
                        let shown = n.alias.as_ref().unwrap_or(&n.name);
                        let sig = self.reexported(file, &imp.from, &n.name.name);
                        let export = Export {
                            sig,
                            span: shown.span,
                        };
                        api.insert(shown.name.clone(), export);
                    }
                }
                _ => {
                    if let Some(name) = item_name(item) {
                        let export = Export {
                            sig: signature(item, &src),
                            span: name_span(item).unwrap_or(item.span),
                        };
                        api.insert(name.to_string(), export);
                    }
                }
            }
        }
        Ok(api)
    }

    /// The signature of `name`, which `file` re-exports from `spec`: its declaration's, through
    /// relative re-exports; for another module (std, a package), the specifier.
    fn reexported(&mut self, file: &Path, spec: &str, name: &str) -> String {
        let outside = format!("`{name}` from \"{spec}\"");
        if !(spec.starts_with("./") || spec.starts_with("../")) {
            return outside;
        }
        let dir = file.parent().unwrap_or(Path::new(""));
        let target = vpm::relpath::normalize(&dir.join(spec));
        let found = super::locate::relative_files(target)
            .into_iter()
            .flatten()
            .find(|f| vpm::probe::is_file(f));
        let Some(found) = found else {
            return outside;
        };
        match &*self.api(&found) {
            Ok(api) => api.get(name).map_or(outside, |e| e.sig.clone()),
            Err(_) => outside,
        }
    }

    /// `file` parsed: the loaded module when the program has it, else read and parsed (its
    /// syntax errors are reported: nothing else checks a variant the build does not use).
    fn parsed(&mut self, file: &Path) -> Result<(FileId, ast::Module), String> {
        if let Some(&i) = self.by_file.get(&super::file_key(file)) {
            return Ok((self.modules[i].file, self.modules[i].ast.clone()));
        }
        let src = self
            .read(file)
            .map_err(|e| format!("cannot be read: {e}"))?;
        let id = self.sm.add(file, src);
        let (module, diags) = velt_syntax::parse_file(id, &self.sm.get(id).src);
        let failed = diags.iter().any(Diagnostic::is_error);
        self.diags.extend(diags);
        if failed {
            return Err("has syntax errors".into());
        }
        Ok((id, module))
    }
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
