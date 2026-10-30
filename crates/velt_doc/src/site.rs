//! The docs website: the Markdown documents listed in `docs/site/pages.txt` rendered to HTML
//! with a shared sidebar, plus the standard library's API reference under `std/`, and one search
//! index over both. Output is a static directory (GitHub Pages, any web server).
//!
//! `pages.txt`: one `Title = path/to/page.md` per line (relative to the file; `#` comments).
//! `* = path/to/dir/` stands for every page in that folder and below that no other line names,
//! sorted by path (`net.md`, then `net/dns.md`) and titled by their first `# ` heading: the
//! standard library's pages follow its tree that way.
//! The first page becomes the landing page `index.html`. Every other page keeps its path as
//! written, without leading `../` (`../book/tour.md` → `book/tour.html`), so the site mirrors
//! the documentation tree and relative links between pages keep working.

use std::path::{Path, PathBuf};

use crate::html::{self, NavLink};
use crate::{extract_all, inputs_from_dir, module_nav, write, write_assets, write_module_pages};

/// One page of the site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    /// Sidebar title.
    pub title: String,
    /// The Markdown source.
    pub source: PathBuf,
    /// Output path relative to the site root (`book/tour.html`).
    pub output: String,
}

impl Page {
    /// Output path relative to the site root (`../book/tour.md` → `book/tour.html`).
    pub fn file(&self) -> String {
        self.output.clone()
    }

    /// The relative path from this page back to the site root (`""`, `"../"`, …).
    fn root(&self) -> String {
        "../".repeat(self.output.matches('/').count())
    }
}

/// The output path of a page written as `path` in `pages.txt`: leading `../` and `./` dropped,
/// `.md` replaced by `.html`.
fn output_path(path: &str) -> String {
    let mut rest = path.replace('\\', "/");
    while let Some(r) = rest.strip_prefix("../").or_else(|| rest.strip_prefix("./")) {
        rest = r.to_string();
    }
    match rest.strip_suffix(".md") {
        Some(stem) => format!("{stem}.html"),
        None => format!("{rest}.html"),
    }
}

/// Parse `pages.txt` (paths relative to `base`).
pub fn parse_pages(text: &str, base: &Path) -> Result<Vec<Page>, String> {
    let mut lines = vec![];
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (title, path) = line
            .split_once('=')
            .ok_or_else(|| format!("pages.txt:{}: expected `Title = path.md`", n + 1))?;
        lines.push((title.trim(), path.trim()));
    }
    let named: Vec<String> = lines
        .iter()
        .filter(|(title, _)| *title != "*")
        .map(|(_, path)| path.replace('\\', "/"))
        .collect();
    let mut pages = vec![];
    for (title, path) in lines {
        let entries = match title {
            "*" => folder_pages(base, path, &named)?,
            _ => vec![(title.to_string(), path.to_string())],
        };
        for (title, path) in entries {
            let output = if pages.is_empty() {
                "index.html".to_string()
            } else {
                output_path(&path)
            };
            pages.push(Page {
                title,
                source: base.join(&path),
                output,
            });
        }
    }
    Ok(pages)
}

/// The pages of `* = dir/`: `(title, path as pages.txt would write it)` for every `.md` file
/// in `dir` (relative to `base`) and below, except those `named` lists, sorted by path without
/// the extension, each titled by its first `# ` heading.
fn folder_pages(base: &Path, dir: &str, named: &[String]) -> Result<Vec<(String, String)>, String> {
    let dir = dir.replace('\\', "/");
    let dir = dir.trim_end_matches('/');
    let mut found = vec![];
    collect_md(&base.join(dir), "", &mut found)?;
    found.sort_by(|a, b| a.trim_end_matches(".md").cmp(b.trim_end_matches(".md")));
    let mut out = vec![];
    for rel in found {
        let path = format!("{dir}/{rel}");
        if named.contains(&path) {
            continue;
        }
        let file = base.join(&path);
        let text = std::fs::read_to_string(&file)
            .map_err(|e| format!("cannot read `{}`: {e}", file.display()))?;
        let title = text
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .map(|t| t.trim().to_string())
            .ok_or_else(|| format!("`{}` has no `# ` heading for its title", file.display()))?;
        out.push((title, path));
    }
    Ok(out)
}

/// The `.md` files below `dir`, as paths relative to it (`/`-separated, `prefix` first).
fn collect_md(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read `{}`: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = format!("{prefix}{name}");
        if entry.path().is_dir() {
            collect_md(&entry.path(), &format!("{rel}/"), out)?;
        } else if name.ends_with(".md") {
            out.push(rel);
        }
    }
    Ok(())
}

/// Build the site from `pages_file` and the std library at `std_dir` into `out`.
pub fn build_site(pages_file: &Path, std_dir: &Path, out: &Path) -> Result<usize, String> {
    let text = std::fs::read_to_string(pages_file)
        .map_err(|e| format!("cannot read `{}`: {e}", pages_file.display()))?;
    let base = pages_file.parent().unwrap_or(Path::new("."));
    let pages = parse_pages(&text, base)?;
    let modules = extract_all(&inputs_from_dir(std_dir, "std")?);
    let mut nav: Vec<NavLink> = pages
        .iter()
        .map(|p| NavLink {
            title: p.title.clone(),
            href: p.file(),
        })
        .collect();
    nav.push(NavLink {
        title: "Standard library".into(),
        href: "std/index.html".into(),
    });
    for page in &pages {
        let md = std::fs::read_to_string(&page.source)
            .map_err(|e| format!("cannot read `{}`: {e}", page.source.display()))?;
        let body = crate::markdown::to_html(&md);
        write(
            &out.join(page.file()),
            &html::layout(&page.title, SITE, &nav, &page.root(), &body),
        )?;
    }
    let std_nav: Vec<NavLink> = nav
        .iter()
        .cloned()
        .chain(module_nav(&modules, "std/"))
        .collect();
    write_module_pages(SITE, &modules, &std_nav, &out.join("std"), "../")?;
    let intro =
        "The modules under `std/` are imported with `import { … } from \"velt/<module>\"`; \
                 `std/prelude/*` is available everywhere without an import.";
    let index = html::index_body("Standard library", intro, &modules);
    write(
        &out.join("std/index.html"),
        &html::layout("Standard library", SITE, &std_nav, "../", &index),
    )?;
    write_assets(out, &html::search_index(&modules, "std/", &nav))?;
    Ok(pages.len() + modules.len() + 1)
}

/// The site's name in titles and the sidebar.
const SITE: &str = "Velt";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_pages_and_std_reference() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = tmp.path().join("docs");
        std::fs::create_dir_all(docs.join("site")).unwrap();
        std::fs::write(
            docs.join("site/index.md"),
            "# Welcome\n\nSee [the guide](../guide.md).\n",
        )
        .unwrap();
        std::fs::write(docs.join("guide.md"), "# Guide\n\nText.\n").unwrap();
        std::fs::write(
            docs.join("site/pages.txt"),
            "# nav\nHome = index.md\nGuide = ../guide.md\n",
        )
        .unwrap();
        let std_dir = tmp.path().join("std");
        std::fs::create_dir_all(&std_dir).unwrap();
        std::fs::write(
            std_dir.join("fs.vlt"),
            "// Files.\n\nexport function read(): string {\n  return \"\";\n}\n",
        )
        .unwrap();
        let out = tmp.path().join("site");
        let n = build_site(&docs.join("site/pages.txt"), &std_dir, &out).unwrap();
        assert_eq!(n, 4);
        let index = std::fs::read_to_string(out.join("index.html")).unwrap();
        assert!(
            index.contains("href=\"../guide.html\"") || index.contains("guide.html"),
            "{index}"
        );
        assert!(out.join("guide.html").is_file());
        let fs = std::fs::read_to_string(out.join("std/std.fs.html")).unwrap();
        assert!(fs.contains("function read(): string") && fs.contains("href=\"../style.css\""));
        let search = std::fs::read_to_string(out.join("search-index.js")).unwrap();
        assert!(search.contains("std/std.fs.html#function.read") && search.contains("\"Guide\""));
        assert!(parse_pages("bad line", tmp.path()).is_err());
    }

    #[test]
    fn a_folder_of_pages_follows_its_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = tmp.path();
        for (path, text) in [
            ("std/README.md", "# Guide\n"),
            ("std/net.md", "# velt/net\n"),
            ("std/net/http.md", "Intro.\n\n# velt/net/http: fetch\n"),
            ("std/net/http/h2.md", "# velt/net/http/h2\n"),
            ("std/fs.md", "# velt/fs\n"),
            ("std/netx.md", "# velt/netx\n"),
        ] {
            std::fs::create_dir_all(docs.join(path).parent().unwrap()).unwrap();
            std::fs::write(docs.join(path), text).unwrap();
        }
        let text = "Home = std/README.md\n* = std/\n";
        let pages = parse_pages(text, docs).unwrap();
        let got: Vec<(&str, String)> = pages.iter().map(|p| (p.title.as_str(), p.file())).collect();
        assert_eq!(
            got,
            [
                ("Home", "index.html".to_string()),
                ("velt/fs", "std/fs.html".to_string()),
                ("velt/net", "std/net.html".to_string()),
                ("velt/net/http: fetch", "std/net/http.html".to_string()),
                ("velt/net/http/h2", "std/net/http/h2.html".to_string()),
                ("velt/netx", "std/netx.html".to_string()),
            ]
        );
        std::fs::write(docs.join("std/fs.md"), "No heading.\n").unwrap();
        assert!(parse_pages(text, docs)
            .unwrap_err()
            .contains("no `# ` heading"));
    }

    #[test]
    fn pages_keep_their_paths_below_the_docs() {
        let text = "Home = ../README.md\nTour = ../book/tour.md\nPrelude = ../std/prelude.md\n";
        let pages = parse_pages(text, Path::new("docs/site")).unwrap();
        let files: Vec<String> = pages.iter().map(Page::file).collect();
        assert_eq!(files, ["index.html", "book/tour.html", "std/prelude.html"]);
        assert_eq!(pages[0].root(), "");
        assert_eq!(pages[1].root(), "../");
    }
}
