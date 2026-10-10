//! What gets documented: the exported items of a module (and `extend` blocks, which add
//! methods to types everywhere; names listed in `export { … }`), their public members, their
//! signatures in canonical form ([`crate::sig`]), and the doc comment right above each
//! declaration (`/** … */` or `///` lines; see [`crate::comment`]). A comment block at the top
//! of the file, followed by a blank line, documents the module. Re-exports (`export { x } from`,
//! `export * from`) are recorded here and resolved across modules by [`crate::resolve`].

use std::cell::RefCell;
use std::ops::Range;

use velt_common::FileId;
use velt_syntax::ast::{self, Item, ItemKind};

use crate::comment::{self, DocComment};
use crate::sig::Printer;

/// The kind of a documented declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `function`.
    Function,
    /// `class`.
    Class,
    /// `struct`.
    Struct,
    /// `interface`.
    Interface,
    /// `enum`.
    Enum,
    /// `type X = …`.
    TypeAlias,
    /// Top-level `const` / `let`.
    Constant,
    /// `extend<T> Target { … }`.
    Extension,
    /// A field of a class, struct or interface.
    Field,
    /// A method (or accessor) of a class, struct, interface or extension.
    Method,
    /// A constructor.
    Constructor,
    /// An enum member.
    Variant,
    /// A re-export of a module that is not documented alongside (`export { x } from "pkg"`).
    ReExport,
}

impl Kind {
    /// The lowercase word used in the pages and the search index.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Function => "function",
            Kind::Class => "class",
            Kind::Struct => "struct",
            Kind::Interface => "interface",
            Kind::Enum => "enum",
            Kind::TypeAlias => "type",
            Kind::Constant => "const",
            Kind::Extension => "extend",
            Kind::Field => "field",
            Kind::Method => "method",
            Kind::Constructor => "constructor",
            Kind::Variant => "member",
            Kind::ReExport => "re-export",
        }
    }
}

/// One documented declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocItem {
    /// What it is.
    pub kind: Kind,
    /// Its name (`Array<T>` for an extension; the exported name for a re-export).
    pub name: String,
    /// The declaration in canonical form (bodies left out; see [`crate::sig`]).
    pub signature: String,
    /// The doc comment's Markdown (empty if none; [`DocComment::render_markdown`]).
    pub doc: String,
    /// `@deprecated`: `Some` (with its text, possibly empty) when deprecated.
    pub deprecated: Option<String>,
    /// Public members (types and extensions only).
    pub members: Vec<DocItem>,
    /// Its generic parameters' names (they are never links to types of the same name).
    pub generics: Vec<String>,
    /// For an item another module declares and this one re-exports: where it is declared.
    pub origin: Option<Origin>,
}

/// Where a re-exported item is declared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    /// The declaring module.
    pub module: String,
    /// The item's name there.
    pub name: String,
}

/// One documented module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocModule {
    /// The import specifier (`std/fs`) or package-relative name.
    pub name: String,
    /// The module's leading comment block.
    pub doc: String,
    /// Exported items in source order (after [`crate::resolve`], followed by re-exported ones).
    pub items: Vec<DocItem>,
    /// Names imported from other modules (for links in signatures).
    pub imports: Vec<Import>,
    /// `export { … } from` / `export * from` (and exported imports), in source order; resolved
    /// into `items` by [`crate::resolve`].
    pub reexports: Vec<ReExport>,
}

/// A name bound by an import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    /// The name in this module.
    pub local: String,
    /// The module specifier as written.
    pub from: String,
    /// The exported name; `None` for a namespace (`import * as local`).
    pub name: Option<String>,
}

/// A re-export statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReExport {
    /// The module specifier as written.
    pub from: String,
    /// `(name in that module, exported name)` pairs.
    pub names: Vec<(String, String)>,
    /// `export * from`.
    pub all: bool,
}

/// Extract the documentation of module `name` from its `source`. Syntax errors are tolerated:
/// whatever the parser recovered is documented. Re-exports are recorded, not yet resolved.
pub fn extract(name: &str, source: &str) -> DocModule {
    extract_with(name, source).0
}

/// The declarations that [`extract`] documents without a doc comment although a plain comment
/// ends on the line right above them: the byte offsets where they start. Before `/** */` became
/// the doc comment syntax, such a comment was the documentation; std's tests use this to keep
/// plain comments from hiding docs.
pub fn plain_comments_above_docs(source: &str) -> Vec<u32> {
    extract_with("", source).1
}

fn extract_with(name: &str, source: &str) -> (DocModule, Vec<u32>) {
    let (module, _) = velt_syntax::parse_file(FileId(0), source);
    let text = Source {
        printer: Printer { src: source },
        comments: velt_syntax::comment_ranges(source),
        plain_above: RefCell::default(),
    };
    let mut imports = vec![];
    for item in &module.items {
        if let ItemKind::Import(i) = &item.kind {
            if !item.exported && !i.from.is_empty() {
                imports.extend(imported_names(i));
            }
        }
    }
    let mut items = vec![];
    let mut reexports = vec![];
    for item in &module.items {
        match &item.kind {
            ItemKind::Import(i) if item.exported && !i.from.is_empty() => {
                reexports.push(ReExport {
                    from: i.from.clone(),
                    names: i
                        .names
                        .iter()
                        .map(|n| (n.name.name.clone(), exported_name(n)))
                        .collect(),
                    all: i.all,
                });
            }
            // `export { a, b as c };`: local declarations, or names imported above.
            ItemKind::Import(i) if item.exported => {
                for n in &i.names {
                    let exported = exported_name(n);
                    let local = module
                        .items
                        .iter()
                        .find(|d| declared_name(d) == Some(&n.name.name));
                    if let Some(mut doc) = local.and_then(|d| text.item(d, true)) {
                        doc.name = exported;
                        items.push(doc);
                    } else if let Some(imp) = imports.iter().find(|imp| imp.local == n.name.name) {
                        if let Some(orig) = &imp.name {
                            reexports.push(ReExport {
                                from: imp.from.clone(),
                                names: vec![(orig.clone(), exported)],
                                all: false,
                            });
                        }
                    }
                }
            }
            _ => items.extend(text.item(item, false)),
        }
    }
    let module = DocModule {
        name: name.to_string(),
        doc: text.module_doc(),
        items,
        imports,
        reexports,
    };
    (module, text.plain_above.into_inner())
}

fn exported_name(n: &ast::ImportName) -> String {
    n.alias.as_ref().unwrap_or(&n.name).name.clone()
}

fn imported_names(i: &ast::Import) -> Vec<Import> {
    if let Some(ns) = &i.namespace {
        return vec![Import {
            local: ns.name.clone(),
            from: i.from.clone(),
            name: None,
        }];
    }
    i.names
        .iter()
        .map(|n| Import {
            local: exported_name(n),
            from: i.from.clone(),
            name: Some(n.name.name.clone()),
        })
        .collect()
}

/// The name a top-level declaration binds.
fn declared_name(item: &Item) -> Option<&String> {
    Some(match &item.kind {
        ItemKind::Function(f) => &f.sig.name.name,
        ItemKind::Struct(t) | ItemKind::Class(t) => &t.name.name,
        ItemKind::Interface(i) => &i.name.name,
        ItemKind::Enum(e) => &e.name.name,
        ItemKind::TypeAlias(t) => &t.name.name,
        ItemKind::Var(v) => match &v.pattern.kind {
            ast::PatternKind::Ident(name) => &name.name,
            _ => return None,
        },
        ItemKind::Extend(_) | ItemKind::Import(_) | ItemKind::ExternFn(_) => return None,
    })
}

fn generic_names(generics: &[ast::GenericParam]) -> Vec<String> {
    generics.iter().map(|g| g.name.name.clone()).collect()
}

/// The source text with the helpers that slice and print it.
struct Source<'a> {
    printer: Printer<'a>,
    /// Where the comments are ([`velt_syntax::comment_ranges`]).
    comments: Vec<Range<u32>>,
    /// Documented declarations without a doc comment and with a plain comment right above.
    plain_above: RefCell<Vec<u32>>,
}

impl Source<'_> {
    /// The doc comment of the declaration starting at byte `lo`.
    fn doc_before(&self, lo: u32) -> DocComment {
        let src = self.printer.src;
        let doc = comment::doc_before_in(src, &self.comments, lo);
        if doc.is_none() && comment::plain_comment_before(src, &self.comments, lo) {
            self.plain_above.borrow_mut().push(lo);
        }
        doc.unwrap_or_default()
    }

    /// The first comment block of the file, when a blank line separates it from what follows.
    fn module_doc(&self) -> String {
        comment::module_doc(self.printer.src, &self.comments)
            .map(|d| d.render_markdown())
            .unwrap_or_default()
    }

    /// The documentation of a top-level item: exported ones and extensions, or any
    /// declaration with `any` (one named in an `export { … }` list).
    fn item(&self, item: &Item, any: bool) -> Option<DocItem> {
        if !(item.exported || any) && !matches!(item.kind, ItemKind::Extend(_)) {
            return None;
        }
        let doc = self.doc_before(item.span.lo);
        let p = &self.printer;
        Some(match &item.kind {
            ItemKind::Function(f) => {
                // An overloaded function is called through its signatures, one per line; the
                // implementation's own signature is not callable.
                let sigs = match f.overloads.is_empty() {
                    true => std::slice::from_ref(&f.sig),
                    false => &f.overloads[..],
                };
                let signature = sigs
                    .iter()
                    .map(|s| {
                        let asyncness = if s.is_async { "async " } else { "" };
                        format!("{asyncness}function {}", p.fn_sig(s))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                DocItem {
                    signature,
                    ..leaf(Kind::Function, &f.sig.name.name, &f.sig.generics, doc)
                }
            }
            ItemKind::Struct(t) => self.type_decl(Kind::Struct, t, doc),
            ItemKind::Class(t) => self.type_decl(Kind::Class, t, doc),
            ItemKind::Interface(i) => self.interface(i, doc),
            ItemKind::Enum(e) => self.enumeration(e, doc),
            ItemKind::TypeAlias(t) => DocItem {
                signature: format!(
                    "type {}{} = {}",
                    t.name.name,
                    p.generics(&t.generics),
                    p.ty(&t.ty)
                ),
                ..leaf(Kind::TypeAlias, &t.name.name, &t.generics, doc)
            },
            ItemKind::Var(v) => self.constant(v, doc)?,
            ItemKind::Extend(e) => {
                let target = p.ty(&e.target);
                DocItem {
                    signature: format!("extend{} {target}", p.generics(&e.generics)),
                    members: self.methods(&e.methods),
                    ..leaf(Kind::Extension, &target, &e.generics, doc)
                }
            }
            ItemKind::Import(_) | ItemKind::ExternFn(_) => return None,
        })
    }

    fn type_decl(&self, kind: Kind, t: &ast::TypeDecl, doc: DocComment) -> DocItem {
        let mut members: Vec<DocItem> = t
            .fields
            .iter()
            .filter(|f| !f.is_private)
            .map(|f| self.field(f))
            .collect();
        // A private constructor is not part of the API, like private methods.
        let ctor = t
            .constructor
            .as_ref()
            .filter(|_| t.ctor_visibility != ast::CtorVisibility::Private);
        if let Some(ctor) = ctor {
            members.push(self.member_fn(Kind::Constructor, &ctor.sig));
        }
        members.extend(self.methods(&t.methods));
        let keyword = if kind == Kind::Struct {
            "struct"
        } else {
            "class"
        };
        DocItem {
            signature: self.printer.type_header(
                keyword,
                &t.name.name,
                &t.generics,
                t.extends.as_slice(),
                &t.implements,
            ),
            members,
            ..leaf(kind, &t.name.name, &t.generics, doc)
        }
    }

    fn methods(&self, methods: &[ast::Method]) -> Vec<DocItem> {
        methods
            .iter()
            .filter(|m| !m.is_private)
            .map(|m| match m.decl.overloads.split_first() {
                None => self.member_fn(Kind::Method, &m.decl.sig),
                // Overloads: the signatures, one per line, with the first one's doc comment.
                Some((first, rest)) => {
                    let mut item = self.member_fn(Kind::Method, first);
                    for s in rest {
                        item.signature.push('\n');
                        item.signature += &self.member_fn(Kind::Method, s).signature;
                    }
                    item
                }
            })
            .collect()
    }

    fn interface(&self, i: &ast::InterfaceDecl, doc: DocComment) -> DocItem {
        let mut members: Vec<DocItem> = i.fields.iter().map(|f| self.field(f)).collect();
        members.extend(
            i.methods
                .iter()
                .map(|m| self.member_fn(Kind::Method, &m.sig)),
        );
        DocItem {
            signature: self.printer.type_header(
                "interface",
                &i.name.name,
                &i.generics,
                &i.extends,
                &[],
            ),
            members,
            ..leaf(Kind::Interface, &i.name.name, &i.generics, doc)
        }
    }

    fn enumeration(&self, e: &ast::EnumDecl, doc: DocComment) -> DocItem {
        let members = e
            .variants
            .iter()
            .map(|v| DocItem {
                signature: self.printer.flat(v.span).trim_end_matches(',').to_string(),
                ..leaf(Kind::Variant, &v.name.name, &[], self.doc_before(v.span.lo))
            })
            .collect();
        DocItem {
            signature: format!("enum {}", e.name.name),
            members,
            ..leaf(Kind::Enum, &e.name.name, &[], doc)
        }
    }

    /// `const NAME: T = init` (the initializer only when the whole fits in 80 characters, or
    /// there is no type to show instead).
    fn constant(&self, v: &ast::VarDecl, doc: DocComment) -> Option<DocItem> {
        let ast::PatternKind::Ident(name) = &v.pattern.kind else {
            return None;
        };
        let mut signature = format!("{} {}", v.kind.keyword(), name.name);
        if let Some(ty) = &v.ty {
            signature.push_str(": ");
            signature.push_str(&self.printer.ty(ty));
        }
        if let Some(init) = &v.init {
            let full = format!("{signature} = {}", self.printer.flat(init.span));
            if full.len() <= 80 || v.ty.is_none() {
                signature = full;
            }
        }
        Some(DocItem {
            signature,
            ..leaf(Kind::Constant, &name.name, &[], doc)
        })
    }

    fn field(&self, f: &ast::Field) -> DocItem {
        DocItem {
            signature: self.printer.field(f),
            ..leaf(Kind::Field, &f.name.name, &[], self.doc_before(f.span.lo))
        }
    }

    /// A method, accessor or constructor: its modifiers as written (`static`, `async`, `get`,
    /// `mut`, …), then the signature.
    fn member_fn(&self, kind: Kind, sig: &ast::FnSig) -> DocItem {
        let mods: String = self
            .printer
            .src
            .get(sig.span.lo as usize..sig.name.span.lo.max(sig.span.lo) as usize)
            .unwrap_or("")
            .split_whitespace()
            .filter(|w| !matches!(*w, "export" | "function" | "override"))
            .map(|w| format!("{w} "))
            .collect();
        DocItem {
            signature: format!("{mods}{}", self.printer.fn_sig(sig)),
            ..leaf(
                kind,
                &sig.name.name,
                &sig.generics,
                self.doc_before(sig.span.lo),
            )
        }
    }
}

/// An item without members, signature or origin (filled in by the caller).
fn leaf(kind: Kind, name: &str, generics: &[ast::GenericParam], doc: DocComment) -> DocItem {
    DocItem {
        kind,
        name: name.to_string(),
        signature: String::new(),
        doc: doc.render_markdown(),
        deprecated: doc.deprecated,
        members: vec![],
        generics: generic_names(generics),
        origin: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "// Module docs.\n// Second line.\n\nimport { x } from \"velt:io\";\n\n/// Adds.\n/// Twice.\nexport function add(a: i64, b: i64): i64 {\n  return a + b;\n}\n\nfunction hidden() {}\n\n/** A point. */\nexport class Point {\n  /** X coordinate. */\n  x: f64;\n  private secret: i64 = 0;\n\n  constructor(x: f64) {\n    this.x = x;\n  }\n\n  /**\n   * Length.\n   */\n  len(): f64 {\n    return this.x;\n  }\n}\n\nexport enum Color {\n  Red,\n  Green = 5,\n}\n\nexport const LIMIT: i64 = 10;\n\nexport async function wait(ms: i64) {}\n";

    #[test]
    fn exported_items_with_docs() {
        let m = extract("demo", SRC);
        assert_eq!(m.doc, "Module docs.\nSecond line.");
        let names: Vec<&str> = m.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["add", "Point", "Color", "LIMIT", "wait"]);
        let add = &m.items[0];
        assert_eq!(add.signature, "function add(a: i64, b: i64): i64");
        assert_eq!(add.doc, "Adds.\nTwice.");
        let point = &m.items[1];
        assert_eq!(
            (point.kind, point.signature.as_str()),
            (Kind::Class, "class Point")
        );
        let members: Vec<(&str, &str, &str)> = point
            .members
            .iter()
            .map(|m| (m.name.as_str(), m.signature.as_str(), m.doc.as_str()))
            .collect();
        assert_eq!(
            members,
            [
                ("x", "x: f64", "X coordinate."),
                ("constructor", "constructor(x: f64)", ""),
                ("len", "len(): f64", "Length.")
            ]
        );
        assert_eq!(m.items[2].members.len(), 2);
        assert_eq!(m.items[3].signature, "const LIMIT: i64 = 10");
        assert_eq!(m.items[4].signature, "async function wait(ms: i64)");
    }

    #[test]
    fn tags_and_deprecation() {
        let src = "/**\n * Old.\n * @param a - the value\n * @deprecated Use `g`.\n */\nexport function f(a: i64) {}\n\nexport enum E {\n  /** @deprecated */\n  A,\n  // Plain.\n  B,\n}\n";
        let m = extract("demo", src);
        let f = &m.items[0];
        assert_eq!(
            f.doc,
            "Old.\n\n**Parameters**\n\n- `a`: the value\n\n**Deprecated:** Use `g`."
        );
        assert_eq!(f.deprecated.as_deref(), Some("Use `g`."));
        let e = &m.items[1].members;
        assert_eq!(e[0].deprecated.as_deref(), Some(""));
        assert_eq!((e[1].doc.as_str(), e[1].deprecated.as_ref()), ("", None));
    }

    #[test]
    fn overloads_show_their_signatures() {
        let src = "/** Twice. */\nexport function f(x: string): string;\nexport async function f(x: number): Promise<number>;\nexport async function f(x: string | number): Promise<string | number> {\n  return x;\n}\n\nexport class C {\n  /** Scales. */\n  m(a: f64): f64;\n  m(a: f64, b: f64): f64;\n  m(a: f64, b?: f64): f64 {\n    return a;\n  }\n  static m2(): C;\n  static m2(): C {\n    return new C();\n  }\n}\n";
        let m = extract("demo", src);
        let f = &m.items[0];
        assert_eq!(
            f.signature,
            "function f(x: string): string\nasync function f(x: number): Promise<number>"
        );
        assert_eq!(f.doc, "Twice.");
        let members: Vec<&str> = m.items[1]
            .members
            .iter()
            .map(|m| m.signature.as_str())
            .collect();
        assert_eq!(
            members,
            ["m(a: f64): f64\nm(a: f64, b: f64): f64", "static m2(): C"]
        );
        assert_eq!(m.items[1].members[0].doc, "Scales.");
    }

    #[test]
    fn plain_comments_and_template_text_are_not_docs() {
        let src = "// Module.\n\n// Not a doc.\nexport function f() {}\n\nconst t = `\n/// inside a template\n`;\nexport function g() {}\n";
        let m = extract("demo", src);
        assert_eq!(m.doc, "Module.");
        assert_eq!((m.items[0].doc.as_str(), m.items[1].doc.as_str()), ("", ""));
    }

    #[test]
    fn plain_comments_above_documented_declarations() {
        let src = "// Module.

// Was a doc.
export function f() {}

/** Doc. */
export function g() {}

// Section.

export function h() {}

export class A {
  // Was a doc.
  x: i64;
  // Private.
  private y: i64;
}

// Not exported.
function k() {}
";
        let at = |needle: &str| src.find(needle).unwrap() as u32;
        assert_eq!(
            plain_comments_above_docs(src),
            [at("export function f"), at("x: i64")]
        );
    }

    #[test]
    fn private_constructors_are_hidden() {
        let src = "export class A {\n  private constructor() {}\n}\n\nexport class B {\n  protected constructor(x: i64) {}\n}\n";
        let m = extract("demo", src);
        assert!(m.items[0].members.is_empty());
        let b: Vec<&str> = m.items[1]
            .members
            .iter()
            .map(|m| m.signature.as_str())
            .collect();
        assert_eq!(b, ["protected constructor(x: i64)"]);
    }
}
