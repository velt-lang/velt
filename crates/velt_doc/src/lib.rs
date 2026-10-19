//! API documentation for Velt code, and the docs website.
//!
//! - [`comment`]: doc comments (`/** … */`, `///`) and their JSDoc tags;
//! - [`extract`]: exported items, public members, signatures and doc comments of a module;
//! - [`resolve`]: re-exports across modules (`export { x } from`, `export * from`);
//! - `sig`: signatures in one canonical form, however the source is laid out;
//! - [`markdown`]: the Markdown subset doc comments and the docs are written in;
//! - [`html`]: page layout, module pages (type names in signatures link to their types),
//!   index and the client-side search index;
//! - [`site`]: the website (the pages in `docs/site/pages.txt` + std API reference) built by
//!   `velt-site`.
//!
//! [`write_api_docs`] is what `velt doc` runs: one page per module, an index with a search box.

pub mod comment;
pub mod extract;
pub mod html;
pub mod markdown;
pub mod resolve;
mod sig;
pub mod site;

use std::path::{Path, PathBuf};

pub use extract::{extract, DocModule};

/// A module to document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    /// Its import name (`std/fs`, `mypkg/util`).
    pub module: String,
    /// Its source text.
    pub source: String,
}

/// Every source file under `dir` (`.vlt`, `.ts`, `.tsx` but not `.d.ts`; sorted; hidden
/// directories, `target/` and `node_modules/` skipped) as a module named
/// `<prefix>/<relative path without the extension>` (`x/index.vlt` → `<prefix>/x`).
pub fn inputs_from_dir(dir: &Path, prefix: &str) -> Result<Vec<Input>, String> {
    let mut files = vec![];
    collect(dir, &mut files)?;
    files.sort();
    let mut inputs = vec![];
    for file in files {
        let rel = file.strip_prefix(dir).unwrap_or(&file).with_extension("");
        let mut parts: Vec<String> = rel
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        if parts.len() > 1 && parts.last().is_some_and(|p| p == "index") {
            parts.pop();
        }
        let module = std::iter::once(prefix.to_string())
            .chain(parts)
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("/");
        let source = std::fs::read_to_string(&file)
            .map_err(|e| format!("cannot read `{}`: {e}", file.display()))?;
        inputs.push(Input { module, source });
    }
    Ok(inputs)
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !name.starts_with('.') && name != "target" && name != "node_modules" {
                collect(&path, out)?;
            }
        } else if is_source_name(&name) {
            out.push(path);
        }
    }
    Ok(())
}

/// A Velt source module's file name: `.vlt`, `.ts` or `.tsx`, but not a `.d.ts` declaration
/// file. A copy of `vpm::sources::is_source_name` (crates/vpm/src/sources.rs), which this crate
/// does not depend on: keep the two in step.
fn is_source_name(name: &str) -> bool {
    [".vlt", ".ts", ".tsx"]
        .iter()
        .any(|ext| name.len() > ext.len() && name.ends_with(ext))
        && !name.ends_with(".d.ts")
}

/// Write API docs for `inputs` into `out`: `index.html` (titled `title`, introduced by the
/// Markdown `intro`), a page per module, `style.css`, `search.js`, `search-index.js`.
/// Returns the path of `index.html`.
pub fn write_api_docs(
    title: &str,
    intro: &str,
    inputs: &[Input],
    out: &Path,
) -> Result<PathBuf, String> {
    let modules = extract_all(inputs);
    let nav = module_nav(&modules, "");
    write_module_pages(title, &modules, &nav, out, "")?;
    let index = html::layout(
        title,
        title,
        &nav,
        "",
        &html::index_body(title, intro, &modules),
    );
    write(&out.join("index.html"), &index)?;
    write_assets(out, &html::search_index(&modules, "", &[]))?;
    Ok(out.join("index.html"))
}

/// The documentation of `inputs`, with re-exports resolved among them.
pub fn extract_all(inputs: &[Input]) -> Vec<DocModule> {
    let mut modules: Vec<DocModule> = inputs
        .iter()
        .map(|i| extract(&i.module, &i.source))
        .collect();
    resolve::resolve(&mut modules);
    modules
}

/// Sidebar links to every module page (`prefix`: the pages' directory relative to the root).
pub fn module_nav(modules: &[DocModule], prefix: &str) -> Vec<html::NavLink> {
    modules
        .iter()
        .map(|m| html::NavLink {
            title: m.name.clone(),
            href: format!("{prefix}{}", html::module_file(&m.name)),
        })
        .collect()
}

/// One page per module in `dir`; `root` leads from there to the site root.
pub fn write_module_pages(
    site: &str,
    modules: &[DocModule],
    nav: &[html::NavLink],
    dir: &Path,
    root: &str,
) -> Result<(), String> {
    let links = html::Links::new(modules);
    for m in modules {
        let body = html::module_body(m, &links.scope(modules, m));
        let page = html::layout(&m.name, site, nav, root, &body);
        write(&dir.join(html::module_file(&m.name)), &page)?;
    }
    Ok(())
}

/// The stylesheet and search files at a site root.
pub fn write_assets(root: &Path, search_index: &str) -> Result<(), String> {
    write(&root.join("style.css"), html::STYLE)?;
    write(&root.join("search.js"), html::SEARCH_SCRIPT)?;
    write(&root.join("search-index.js"), search_index)
}

/// Write `text` to `path`, creating its directory.
pub fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create `{}`: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("util")).unwrap();
        std::fs::write(src.join("lib.vlt"), "// Lib.\n\nexport function f() {}\n").unwrap();
        std::fs::write(src.join("util/index.vlt"), "export const X: i64 = 1;\n").unwrap();
        std::fs::write(src.join("card.tsx"), "export function Card() {}\n").unwrap();
        std::fs::write(src.join("model.ts"), "export const Y: i64 = 2;\n").unwrap();
        std::fs::write(src.join("globals.d.ts"), "declare const Z: number;\n").unwrap();
        let inputs = inputs_from_dir(&src, "pkg").unwrap();
        let names: Vec<&str> = inputs.iter().map(|i| i.module.as_str()).collect();
        assert_eq!(names, ["pkg/card", "pkg/lib", "pkg/model", "pkg/util"]);
        let out = tmp.path().join("doc");
        let index = write_api_docs("pkg", "API", &inputs, &out).unwrap();
        let html = std::fs::read_to_string(index).unwrap();
        assert!(
            html.contains("pkg.lib.html") && html.contains("Lib."),
            "{html}"
        );
        for f in [
            "pkg.lib.html",
            "pkg.util.html",
            "style.css",
            "search.js",
            "search-index.js",
        ] {
            assert!(out.join(f).is_file(), "{f}");
        }
    }
}
