//! HTML pages: the shared layout (sidebar, search box, theme), one page per module, the index
//! page and the search index (`search.js`, a list of every item with its URL and summary).

use crate::extract::{DocItem, DocModule, Kind};
use crate::markdown::{escape, inline, to_html};

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

/// The body of a module's page.
pub fn module_body(m: &DocModule) -> String {
    let mut out = format!("<h1>Module <code>{}</code></h1>\n", escape(&m.name));
    out.push_str(&to_html(&m.doc));
    if m.items.is_empty() {
        out.push_str("<p class=\"muted\">No exported items.</p>\n");
    }
    for item in &m.items {
        out.push_str(&item_html(item, None));
    }
    out
}

fn item_html(item: &DocItem, parent: Option<&str>) -> String {
    let id = anchor(item, parent);
    if matches!(item.kind, Kind::Field | Kind::Variant) {
        // Data members are one line each: the declaration and its doc.
        return format!(
            "<div class=\"field\" id=\"{}\"><code>{}</code>{}</div>\n",
            escape(&id),
            escape(&item.signature),
            to_html(&item.doc)
        );
    }
    let tag = if parent.is_some() { "h3" } else { "h2" };
    let mut out = format!(
        "<section class=\"item\" id=\"{}\">\n<{tag}><span class=\"kind\">{}</span> \
         <a href=\"#{0}\">{}</a></{tag}>\n<pre class=\"sig\"><code>{}</code></pre>\n",
        escape(&id),
        item.kind.label(),
        escape(&item.name),
        escape(&item.signature),
    );
    out.push_str(&to_html(&item.doc));
    for member in &item.members {
        out.push_str(&item_html(member, Some(&item.name)));
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
        for item in &m.items {
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
        let body = module_body(&m);
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
}
