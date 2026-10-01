//! HTML pages: the shared layout (sidebar, search box, theme), one page per module, the index
//! page and the search index (`search.js`, a list of every item with its URL and summary).
//! Type names in signatures link to the type's documentation ([`Links`]).

use std::collections::HashMap;

use crate::extract::{DocItem, DocModule, Kind};
use crate::markdown::{escape, inline, to_html};
use crate::resolve::ModuleIndex;

/// The stylesheet shared by API docs and the docs site.
pub const STYLE: &str = include_str!("../assets/style.css");
/// Client-side search over `search.js`.
pub const SEARCH_SCRIPT: &str = include_str!("../assets/search.js");

/// A sidebar entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavLink {
    /// Shown text.
    pub title: String,
    /// Target, relative to the site root.
    pub href: String,
}

/// A whole page. `root` is the relative path from the page to the site root (`""`, `"../"`).
pub fn layout(title: &str, site: &str, nav: &[NavLink], root: &str, body: &str) -> String {
    let links: String = nav
        .iter()
        .map(|l| {
            format!(
                "<li><a href=\"{root}{}\">{}</a></li>\n",
                escape(&l.href),
                escape(&l.title)
            )
        })
        .collect();
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title} · {site}</title>\n<link rel=\"stylesheet\" href=\"{root}style.css\">\n\
         <script src=\"{root}search-index.js\" defer></script>\n\
         <script src=\"{root}search.js\" defer data-root=\"{root}\"></script>\n</head>\n<body>\n\
         <nav>\n<a class=\"brand\" href=\"{root}index.html\">{site}</a>\n\
         <input id=\"search\" type=\"search\" placeholder=\"Search…\" aria-label=\"Search\">\n\
         <ul id=\"results\"></ul>\n<ul class=\"links\">\n{links}</ul>\n</nav>\n\
         <main>\n{body}</main>\n</body>\n</html>\n",
        title = escape(title),
        site = escape(site),
    )
}

/// File name of a module's page: `std/fs` → `std.fs.html`.
pub fn module_file(name: &str) -> String {
    format!("{}.html", name.replace('/', "."))
}

/// Anchor of an item (`fn.readFile`), or of a member (`Point.len`).
fn anchor(item: &DocItem, parent: Option<&str>) -> String {
    match parent {
        Some(p) => format!("{p}.{}", item.name),
        None => format!("{}.{}", item.kind.label(), item.name),
    }
}

/// Where documented types are, for links in signatures. All module pages live in one
/// directory, so a link is `<page>#<anchor>`.
pub struct Links {
    /// Per module: exported type name → link to its documentation.
    types: Vec<HashMap<String, String>>,
    index: ModuleIndex,
}

impl Links {
    /// The types `modules` export (after [`crate::resolve`]): classes, structs, interfaces,
    /// enums and type aliases.
    pub fn new(modules: &[DocModule]) -> Links {
        let types = modules
            .iter()
            .map(|m| {
                m.items
                    .iter()
                    .filter(|it| is_type(it.kind))
                    .map(|it| (it.name.clone(), item_href(m, it)))
                    .collect()
            })
            .collect();
        Links {
            types,
            index: ModuleIndex::new(modules),
        }
    }

    /// The type names that signatures in module `m` can refer to: the prelude's, the ones it
    /// imports (`ns.T` for a namespace import), and its own (each shadowing the ones before).
    pub fn scope(&self, modules: &[DocModule], m: &DocModule) -> HashMap<String, String> {
        let mut scope = HashMap::new();
        for (i, other) in modules.iter().enumerate() {
            if other.name.starts_with("std/prelude") {
                scope.extend(self.types[i].clone());
            }
        }
        for imp in &m.imports {
            let Some(t) = self.index.resolve(&m.name, &imp.from) else {
                continue;
            };
            match &imp.name {
                Some(name) => {
                    if let Some(href) = self.types[t].get(name) {
                        scope.insert(imp.local.clone(), href.clone());
                    }
                }
                None => scope.extend(
                    self.types[t]
                        .iter()
                        .map(|(name, href)| (format!("{}.{name}", imp.local), href.clone())),
                ),
            }
        }
        if let Some(own) = self.index.resolve(&m.name, &m.name) {
            scope.extend(self.types[own].clone());
        }
        scope
    }
}

fn is_type(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Class | Kind::Struct | Kind::Interface | Kind::Enum | Kind::TypeAlias
    )
}

/// The link to an exported item: its declaration's documentation when it is re-exported.
fn item_href(m: &DocModule, item: &DocItem) -> String {
    match &item.origin {
        Some(o) => format!(
            "{}#{}.{}",
            module_file(&o.module),
            item.kind.label(),
            o.name
        ),
        None => format!("{}#{}", module_file(&m.name), anchor(item, None)),
    }
}

/// What a page needs to render signatures with links.
struct Page<'a> {
    /// This page's file (links into it become `#anchor`).
    file: String,
    scope: &'a HashMap<String, String>,
}

impl Page<'_> {
    /// `signature` as HTML, with type names linked. Names in `generics` are type parameters,
    /// never links; `self_href` (the item being declared) is not linked to itself.
    fn signature(&self, signature: &str, generics: &[&str], self_href: &str) -> String {
        let mut out = String::new();
        let chars: Vec<char> = signature.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if matches!(c, '"' | '\'' | '`') {
                let start = i;
                i += 1;
                while i < chars.len() && chars[i] != c {
                    i += if chars[i] == '\\' { 2 } else { 1 };
                }
                i = (i + 1).min(chars.len());
                out.push_str(&escape(&chars[start..i].iter().collect::<String>()));
                continue;
            }
            if !is_ident_start(c) {
                out.push_str(&escape(&c.to_string()));
                i += 1;
                continue;
            }
            // An identifier, with `.`-separated continuations (`ns.Type`).
            let start = i;
            while i < chars.len()
                && (is_ident_char(chars[i])
                    || (chars[i] == '.' && chars.get(i + 1).is_some_and(|&n| is_ident_start(n))))
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            // A parameter or field name (`name:`, `name?:`), not a type.
            let next = (i..chars.len()).find(|&j| !chars[j].is_whitespace());
            let is_name = next.is_some_and(|j| {
                chars[j] == ':' || (chars[j] == '?' && chars.get(j + 1) == Some(&':'))
            });
            let preceded_by_dot = start > 0 && chars[start - 1] == '.';
            let href = self
                .scope
                .get(&word)
                .filter(|_| !is_name && !preceded_by_dot && !generics.contains(&word.as_str()))
                .filter(|href| href.as_str() != self_href);
            match href {
                Some(href) => {
                    let href = href
                        .strip_prefix(&self.file)
                        .filter(|h| h.starts_with('#'))
                        .unwrap_or(href);
                    out.push_str(&format!(
                        "<a href=\"{}\">{}</a>",
                        escape(href),
                        escape(&word)
                    ));
                }
                None => out.push_str(&escape(&word)),
            }
        }
        out
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The body of a module's page; type names in signatures link to the types in `scope` (see
/// [`Links::scope`]).
pub fn module_body(m: &DocModule, scope: &HashMap<String, String>) -> String {
    let page = Page {
        file: module_file(&m.name),
        scope,
    };
    let mut out = format!("<h1>Module <code>{}</code></h1>\n", escape(&m.name));
    out.push_str(&to_html(&m.doc));
    if m.items.is_empty() {
        out.push_str("<p class=\"muted\">No exported items.</p>\n");
    }
    for item in &m.items {
        out.push_str(&item_html(&page, m, item, None));
    }
    out
}

fn item_html(page: &Page, m: &DocModule, item: &DocItem, parent: Option<&DocItem>) -> String {
    let id = anchor(item, parent.map(|p| p.name.as_str()));
    let generics: Vec<&str> = item
        .generics
        .iter()
        .chain(parent.iter().flat_map(|p| &p.generics))
        .map(String::as_str)
        .collect();
    let self_href = match parent {
        None => item_href(m, item),
        Some(_) => String::new(),
    };
    let signature = page.signature(&item.signature, &generics, &self_href);
    if matches!(item.kind, Kind::Field | Kind::Variant | Kind::ReExport) {
        // Data members and unresolved re-exports are one line each: the declaration and its
        // doc.
        return format!(
            "<div class=\"field\" id=\"{}\"><code>{signature}</code>{}</div>\n",
            escape(&id),
            to_html(&item.doc)
        );
    }
    let tag = if parent.is_some() { "h3" } else { "h2" };
    let mut out = format!(
        "<section class=\"item\" id=\"{}\">\n<{tag}><span class=\"kind\">{}</span> \
         <a href=\"#{0}\">{}</a></{tag}>\n<pre class=\"sig\"><code>{signature}</code></pre>\n",
        escape(&id),
        item.kind.label(),
        escape(&item.name),
    );
    if let Some(origin) = &item.origin {
        let href = format!(
            "{}#{}.{}",
            module_file(&origin.module),
            item.kind.label(),
            origin.name
        );
        let renamed = if origin.name == item.name {
            String::new()
        } else {
            format!(" (as <code>{}</code>)", escape(&origin.name))
        };
        out.push_str(&format!(
            "<p class=\"muted\">Re-exported from <a href=\"{}\"><code>{}</code></a>{renamed}.</p>\n",
            escape(&href),
            escape(&origin.module),
        ));
    }
    out.push_str(&to_html(&item.doc));
    for member in &item.members {
        out.push_str(&item_html(page, m, member, Some(item)));
    }
    out.push_str("</section>\n");
    out
}

/// The body of the index page: the modules with the first sentence of their docs.
pub fn index_body(title: &str, intro: &str, modules: &[DocModule]) -> String {
    let mut out = format!("<h1>{}</h1>\n{}", escape(title), to_html(intro));
    out.push_str("<table class=\"modules\">\n<tbody>\n");
    for m in modules {
        out.push_str(&format!(
            "<tr><td><a href=\"{}\"><code>{}</code></a></td><td>{}</td></tr>\n",
            module_file(&m.name),
            escape(&m.name),
            inline(&summary(&m.doc)),
        ));
    }
    out.push_str("</tbody>\n</table>\n");
    out
}

/// The first sentence (or line) of a doc comment.
pub fn summary(doc: &str) -> String {
    let first = doc.split("\n\n").next().unwrap_or("").replace('\n', " ");
    match first.find(". ") {
        Some(i) => first[..=i].to_string(),
        None => first,
    }
}

/// `search-index.js`: `window.VELT_SEARCH = [[name, kind, module, href, summary], …]`, with
/// `href` relative to the site root: API pages live under `prefix` (e.g. `std/`), `pages`
/// are extra `(title, href)` entries (the site's guides).
pub fn search_index(modules: &[DocModule], prefix: &str, pages: &[NavLink]) -> String {
    let mut rows: Vec<String> = pages
        .iter()
        .map(|p| row(&p.title, "page", "", &p.href, ""))
        .collect();
    for m in modules {
        let page = format!("{prefix}{}", module_file(&m.name));
        rows.push(row(&m.name, "module", &m.name, &page, &summary(&m.doc)));
        for item in m.items.iter().filter(|it| it.kind != Kind::ReExport) {
            let href = format!("{page}#{}", anchor(item, None));
            rows.push(row(
                &item.name,
                item.kind.label(),
                &m.name,
                &href,
                &summary(&item.doc),
            ));
            for member in &item.members {
                let href = format!("{page}#{}", anchor(member, Some(&item.name)));
                let name = format!("{}.{}", item.name, member.name);
                rows.push(row(
                    &name,
                    member.kind.label(),
                    &m.name,
                    &href,
                    &summary(&member.doc),
                ));
            }
        }
    }
    format!("window.VELT_SEARCH = [\n{}\n];\n", rows.join(",\n"))
}

fn row(name: &str, kind: &str, module: &str, href: &str, summary: &str) -> String {
    let fields = [name, kind, module, href, summary].map(js_string);
    format!("[{}]", fields.join(","))
}

/// A JavaScript string literal.
fn js_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '<' => out.push_str("\\u003c"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::extract;

    #[test]
    fn module_page_and_index() {
        let m = extract("std/demo", "// Demo module. More.\n\n// Adds \"x\".\nexport function add(a: i64): i64 {\n  return a;\n}\n");
        let body = module_body(&m, &HashMap::new());
        assert!(body.contains("id=\"function.add\""), "{body}");
        assert!(body.contains("function add(a: i64): i64"), "{body}");
        let page = layout("std/demo", "Velt", &[], "../", &body);
        assert!(page.contains("href=\"../style.css\""));
        let index = search_index(&[m], "std/", &[]);
        assert!(
            index.contains(
                r#"["add","function","std/demo","std/std.demo.html#function.add","Adds \"x\"."]"#
            ),
            "{index}"
        );
        assert_eq!(summary("One. Two.\nThree"), "One.");
        assert_eq!(js_string("</script>"), "\"\\u003c/script>\"");
    }

    #[test]
    fn type_names_link_to_their_docs() {
        let mut ms = vec![
            extract(
                "pkg/shapes",
                "export class Circle {}\nexport interface Shape {}\nexport type Id = i64;\n",
            ),
            extract(
                "pkg/lib",
                "import { Circle as C } from \"./shapes\";\nimport * as s from \"./shapes\";\n\
                 export { Shape } from \"./shapes\";\n\
                 export class Box<T> extends Base {\n  item: T;\n  shape(Shape: s.Id): Shape { return 0; }\n}\n\
                 export function f<Circle>(c: C, label: string = \"Shape\"): Box<Circle> {}\n",
            ),
            extract("std/prelude/core", "export class Base {}\n"),
        ];
        crate::resolve::resolve(&mut ms);
        let links = Links::new(&ms);
        let body = module_body(&ms[1], &links.scope(&ms, &ms[1]));
        // Imported (renamed and via a namespace), re-exported (to its declaration), own (on
        // this page) and prelude types; never a parameter name, a type parameter, a string,
        // or the type being declared.
        for want in [
            "class Box&lt;T&gt; extends <a href=\"std.prelude.core.html#class.Base\">Base</a>",
            "shape(Shape: <a href=\"pkg.shapes.html#type.Id\">s.Id</a>): \
             <a href=\"pkg.shapes.html#interface.Shape\">Shape</a>",
            "function f&lt;Circle&gt;(c: <a href=\"pkg.shapes.html#class.Circle\">C</a>, \
             label: string = &quot;Shape&quot;): <a href=\"#class.Box\">Box</a>&lt;Circle&gt;",
            "Re-exported from <a href=\"pkg.shapes.html#interface.Shape\"><code>pkg/shapes</code></a>",
        ] {
            assert!(body.contains(want), "{want}\n{body}");
        }
    }
}
