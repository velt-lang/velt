//! What gets documented: the exported items of a module (and `extend` blocks, which add
//! methods to types everywhere), their public members, the signatures as written in the
//! source, and the comment block right above each declaration (`///` or `//` lines; a blank
//! line ends it). A comment block at the top of the file, followed by a blank line, documents
//! the module.

use velt_common::{FileId, Span};
use velt_syntax::ast::{self, Item, ItemKind};

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
        }
    }
}

/// One documented declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocItem {
    /// What it is.
    pub kind: Kind,
    /// Its name (`Array<T>` for an extension).
    pub name: String,
    /// The declaration as written (whitespace collapsed, bodies left out).
    pub signature: String,
    /// The doc comment's Markdown (empty if none).
    pub doc: String,
    /// Public members (types and extensions only).
    pub members: Vec<DocItem>,
}

/// One documented module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocModule {
    /// The import specifier (`std/fs`) or package-relative name.
    pub name: String,
    /// The module's leading comment block.
    pub doc: String,
    /// Exported items in source order.
    pub items: Vec<DocItem>,
}

/// Extract the documentation of module `name` from its `source`. Syntax errors are tolerated:
/// whatever the parser recovered is documented.
pub fn extract(name: &str, source: &str) -> DocModule {
    let (module, _) = velt_syntax::parse_file(FileId(0), source);
    let text = Source(source);
    let items = module
        .items
        .iter()
        .filter_map(|item| text.item(item))
        .collect();
    DocModule {
        name: name.to_string(),
        doc: text.module_doc(),
        items,
    }
}

/// The source text with the helpers that slice it.
struct Source<'a>(&'a str);

impl Source<'_> {
    fn slice(&self, span: Span) -> &str {
        self.0.get(span.lo as usize..span.hi as usize).unwrap_or("")
    }

    /// `span`'s text with whitespace runs collapsed to one space.
    fn flat(&self, span: Span) -> String {
        self.slice(span)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The comment block ending on the line before byte `lo`.
    fn doc_before(&self, lo: u32) -> String {
        let before = self.0.get(..lo as usize).unwrap_or("");
        let start_of_line = before.rfind('\n').map_or(0, |i| i + 1);
        if !before[start_of_line..].trim().is_empty() {
            return String::new(); // something else precedes the item on its own line
        }
        let mut lines: Vec<&str> = before[..start_of_line]
            .lines()
            .rev()
            .map(str::trim)
            .take_while(|l| l.starts_with("//"))
            .collect();
        lines.reverse();
        strip_comments(&lines)
    }

    /// The first comment block of the file, when a blank line separates it from what follows.
    fn module_doc(&self) -> String {
        let lines: Vec<&str> = self.0.lines().map(str::trim).collect();
        let n = lines.iter().take_while(|l| l.starts_with("//")).count();
        if n == 0 || lines.get(n).is_some_and(|l| !l.is_empty()) {
            return String::new();
        }
        strip_comments(&lines[..n])
    }

    fn item(&self, item: &Item) -> Option<DocItem> {
        let doc = self.doc_before(item.span.lo);
        let simple = |kind, name: &ast::Ident| DocItem {
            kind,
            name: name.name.clone(),
            signature: self
                .flat(item.span)
                .trim_start_matches("export ")
                .to_string(),
            doc: doc.clone(),
            members: vec![],
        };
        if !item.exported && !matches!(item.kind, ItemKind::Extend(_)) {
            return None;
        }
        Some(match &item.kind {
            ItemKind::Function(f) => DocItem {
                kind: Kind::Function,
                name: f.sig.name.name.clone(),
                signature: self.function_sig(&f.sig),
                doc,
                members: vec![],
            },
            ItemKind::Struct(t) => self.type_decl(item, Kind::Struct, t, doc),
            ItemKind::Class(t) => self.type_decl(item, Kind::Class, t, doc),
            ItemKind::Interface(i) => self.interface(item, i, doc),
            ItemKind::Enum(e) => self.enumeration(item, e, doc),
            ItemKind::TypeAlias(t) => simple(Kind::TypeAlias, &t.name),
            ItemKind::Var(v) => self.constant(item, v, doc)?,
            ItemKind::Extend(e) => self.extension(item, e, doc),
            ItemKind::Import(_) | ItemKind::ExternFn(_) => return None,
        })
    }

    /// `function name<T>(a: A): R` (`async function` for async ones).
    fn function_sig(&self, sig: &ast::FnSig) -> String {
        let text = self.flat(sig.span);
        let text = text.trim_start_matches("export ");
        if text.starts_with("function") || text.starts_with("async function") {
            text.to_string()
        } else if sig.is_async {
            format!("async function {}", text.trim_start_matches("async "))
        } else {
            format!("function {text}")
        }
    }

    /// The declaration header up to its `{`.
    fn header(&self, item: &Item) -> String {
        let text = self.slice(item.span);
        let head = text.find('{').map_or(text, |i| &text[..i]);
        let flat = head.split_whitespace().collect::<Vec<_>>().join(" ");
        flat.trim_start_matches("export ").to_string()
    }

    fn type_decl(&self, item: &Item, kind: Kind, t: &ast::TypeDecl, doc: String) -> DocItem {
        let mut members: Vec<DocItem> = t
            .fields
            .iter()
            .filter(|f| !f.is_private)
            .map(|f| self.field(f))
            .collect();
        if let Some(ctor) = &t.constructor {
            members.push(self.member_fn(Kind::Constructor, &ctor.sig));
        }
        members.extend(self.methods(&t.methods));
        DocItem {
            kind,
            name: t.name.name.clone(),
            signature: self.header(item),
            doc,
            members,
        }
    }

    fn methods(&self, methods: &[ast::Method]) -> Vec<DocItem> {
        methods
            .iter()
            .filter(|m| !m.is_private)
            .map(|m| self.member_fn(Kind::Method, &m.decl.sig))
            .collect()
    }

    fn interface(&self, item: &Item, i: &ast::InterfaceDecl, doc: String) -> DocItem {
        let mut members: Vec<DocItem> = i.fields.iter().map(|f| self.field(f)).collect();
        members.extend(
            i.methods
                .iter()
                .map(|m| self.member_fn(Kind::Method, &m.sig)),
        );
        DocItem {
            kind: Kind::Interface,
            name: i.name.name.clone(),
            signature: self.header(item),
            doc,
            members,
        }
    }

    fn enumeration(&self, item: &Item, e: &ast::EnumDecl, doc: String) -> DocItem {
        let members = e
            .variants
            .iter()
            .map(|v| DocItem {
                kind: Kind::Variant,
                name: v.name.name.clone(),
                signature: self.flat(v.span),
                doc: self.doc_before(v.span.lo),
                members: vec![],
            })
            .collect();
        DocItem {
            kind: Kind::Enum,
            name: e.name.name.clone(),
            signature: self.header(item),
            doc,
            members,
        }
    }

    fn extension(&self, item: &Item, e: &ast::ExtendDecl, doc: String) -> DocItem {
        DocItem {
            kind: Kind::Extension,
            name: self.flat(e.target.span),
            signature: self.header(item),
            doc,
            members: self.methods(&e.methods),
        }
    }

    /// `const NAME: T` (the initializer only when it is short).
    fn constant(&self, item: &Item, v: &ast::VarDecl, doc: String) -> Option<DocItem> {
        let ast::PatternKind::Ident(name) = &v.pattern.kind else {
            return None;
        };
        let full = self.flat(item.span);
        let full = full.trim_start_matches("export ").trim_end_matches(';');
        let signature = match (full.len() > 80, full.split_once(" = ")) {
            (true, Some((decl, _))) if v.ty.is_some() => decl.to_string(),
            _ => full.to_string(),
        };
        Some(DocItem {
            kind: Kind::Constant,
            name: name.name.clone(),
            signature,
            doc,
            members: vec![],
        })
    }

    fn field(&self, f: &ast::Field) -> DocItem {
        DocItem {
            kind: Kind::Field,
            name: f.name.name.clone(),
            signature: self.flat(f.span).trim_end_matches([';', ',']).to_string(),
            doc: self.doc_before(f.span.lo),
            members: vec![],
        }
    }

    fn member_fn(&self, kind: Kind, sig: &ast::FnSig) -> DocItem {
        DocItem {
            kind,
            name: sig.name.name.clone(),
            signature: self.flat(sig.span),
            doc: self.doc_before(sig.span.lo),
            members: vec![],
        }
    }
}

/// Comment lines → Markdown: drop `///` or `//` and one following space.
fn strip_comments(lines: &[&str]) -> String {
    let body: Vec<&str> = lines
        .iter()
        .map(|l| {
            let l = l
                .strip_prefix("///")
                .or_else(|| l.strip_prefix("//"))
                .unwrap_or(l);
            l.strip_prefix(' ').unwrap_or(l)
        })
        .collect();
    body.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "// Module docs.\n// Second line.\n\nimport { x } from \"velt:io\";\n\n/// Adds.\n/// Twice.\nexport function add(a: i64, b: i64): i64 {\n  return a + b;\n}\n\nfunction hidden() {}\n\n// A point.\nexport class Point {\n  // X coordinate.\n  x: f64;\n  private secret: i64 = 0;\n\n  constructor(x: f64) {\n    this.x = x;\n  }\n\n  // Length.\n  len(): f64 {\n    return this.x;\n  }\n}\n\nexport enum Color {\n  Red,\n  Green = 5,\n}\n\nexport const LIMIT: i64 = 10;\n\nexport async function wait(ms: i64) {}\n";

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
}
