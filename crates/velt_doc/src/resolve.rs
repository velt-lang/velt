//! Across modules: re-exports become items of the re-exporting module, and module specifiers
//! resolve to the documented modules they name (for re-exports and for links in signatures).
//!
//! `export { x as y } from "m"` documents `m`'s `x` under the name `y`; `export * from "m"`
//! documents every export of `m` except names the module exports itself or lists by name (the
//! language's rule). Chains are followed and cycles cut. A re-export of a module that is not
//! documented alongside (another package, std while documenting a package) stays a one-line
//! [`Kind::ReExport`] entry.

use std::collections::{HashMap, HashSet};

use crate::extract::{DocItem, DocModule, Kind, Origin, ReExport};

/// Module names → their index in a module list.
pub struct ModuleIndex(HashMap<String, usize>);

impl ModuleIndex {
    /// Index `modules` by name.
    pub fn new(modules: &[DocModule]) -> ModuleIndex {
        ModuleIndex(
            modules
                .iter()
                .enumerate()
                .map(|(i, m)| (m.name.clone(), i))
                .collect(),
        )
    }

    /// The module that `spec`, written in module `from`, names: `velt:x` is `std/x`; `./x` and
    /// `../x` are relative to `from`'s directory (or to `from` itself, when it is a directory's
    /// `index.vlt`); anything else is a module name (`pkg`, `pkg/sub`). `None` when that module
    /// is not documented.
    pub fn resolve(&self, from: &str, spec: &str) -> Option<usize> {
        if let Some(std) = spec.strip_prefix("velt:") {
            return self.0.get(&format!("std/{std}")).copied();
        }
        if spec.starts_with("./") || spec.starts_with("../") {
            let parent = from.rsplit_once('/').map_or("", |(dir, _)| dir);
            return [parent, from]
                .iter()
                .filter_map(|base| join(base, spec))
                .find_map(|name| self.0.get(&name).copied());
        }
        self.0.get(spec).copied()
    }
}

/// `base/rel` with `.` and `..` segments applied (and a trailing `/index` dropped); `None` when
/// `..` leaves the tree.
fn join(base: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            seg => parts.push(seg),
        }
    }
    if parts.len() > 1 && parts.last() == Some(&"index") {
        parts.pop();
    }
    Some(parts.join("/"))
}

/// Replace every module's re-exports with the items they name.
pub fn resolve(modules: &mut [DocModule]) {
    let index = ModuleIndex::new(modules);
    let mut state = State {
        modules,
        index: &index,
        done: HashMap::new(),
        visiting: HashSet::new(),
        cuts: 0,
    };
    let all: Vec<Vec<DocItem>> = (0..state.modules.len()).map(|i| state.exports(i)).collect();
    for (m, items) in modules.iter_mut().zip(all) {
        m.items = items;
        m.reexports.clear();
    }
}

struct State<'a> {
    modules: &'a [DocModule],
    index: &'a ModuleIndex,
    done: HashMap<usize, Vec<DocItem>>,
    /// Modules whose exports are being computed (a re-export cycle stops there).
    visiting: HashSet<usize>,
    /// How many times a cycle was cut: a result computed across a cut is incomplete for the
    /// modules on the cycle, so it is not kept.
    cuts: usize,
}

impl State<'_> {
    /// Everything module `i` exports: its own items, then the re-exported ones.
    fn exports(&mut self, i: usize) -> Vec<DocItem> {
        if let Some(done) = self.done.get(&i) {
            return done.clone();
        }
        if !self.visiting.insert(i) {
            self.cuts += 1;
            return vec![];
        }
        let cuts = self.cuts;
        let m = &self.modules[i];
        let mut items = m.items.clone();
        // A name the module declares or lists by name wins over `export *`.
        let mut taken: HashSet<String> = items
            .iter()
            .filter(|it| it.kind != Kind::Extension)
            .map(|it| it.name.clone())
            .collect();
        taken.extend(
            m.reexports
                .iter()
                .flat_map(|r| r.names.iter().map(|(_, alias)| alias.clone())),
        );
        for r in &m.reexports {
            let target = self.index.resolve(&m.name, &r.from);
            let theirs = match target {
                Some(t) if t != i => self.exports(t),
                _ => {
                    items.extend(stubs(r));
                    continue;
                }
            };
            let origin_module = &self.modules[target.unwrap_or(i)].name;
            let reexported = |it: &DocItem, name: &str| DocItem {
                name: name.to_string(),
                origin: Some(it.origin.clone().unwrap_or_else(|| Origin {
                    module: origin_module.clone(),
                    name: it.name.clone(),
                })),
                ..it.clone()
            };
            let exportable = theirs.iter().filter(|it| it.kind != Kind::Extension);
            if r.all {
                for it in exportable {
                    if !taken.contains(&it.name) {
                        taken.insert(it.name.clone());
                        items.push(reexported(it, &it.name));
                    }
                }
                continue;
            }
            for (name, alias) in &r.names {
                match theirs.iter().find(|it| &it.name == name) {
                    Some(it) => items.push(reexported(it, alias)),
                    None => items.extend(stubs(&ReExport {
                        from: r.from.clone(),
                        names: vec![(name.clone(), alias.clone())],
                        all: false,
                    })),
                }
            }
        }
        self.visiting.remove(&i);
        if self.cuts == cuts {
            self.done.insert(i, items.clone());
        }
        items
    }
}

/// One-line entries for a re-export whose module is not documented.
fn stubs(r: &ReExport) -> Vec<DocItem> {
    let stub = |name: &str, signature: String| DocItem {
        kind: Kind::ReExport,
        name: name.to_string(),
        signature,
        doc: String::new(),
        members: vec![],
        generics: vec![],
        origin: None,
    };
    if r.all {
        return vec![stub("*", format!("export * from \"{}\"", r.from))];
    }
    r.names
        .iter()
        .map(|(name, alias)| {
            let list = if name == alias {
                name.clone()
            } else {
                format!("{name} as {alias}")
            };
            stub(alias, format!("export {{ {list} }} from \"{}\"", r.from))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::extract;

    fn names(m: &DocModule) -> Vec<(&str, Option<&str>)> {
        m.items
            .iter()
            .map(|i| {
                (
                    i.name.as_str(),
                    i.origin.as_ref().map(|o| o.module.as_str()),
                )
            })
            .collect()
    }

    #[test]
    fn specifiers() {
        let ms: Vec<DocModule> = [
            "std/fs",
            "pkg",
            "pkg/shapes",
            "pkg/shapes/circle",
            "pkg/util",
        ]
        .iter()
        .map(|n| extract(n, ""))
        .collect();
        let idx = ModuleIndex::new(&ms);
        assert_eq!(idx.resolve("pkg", "velt:fs"), Some(0));
        assert_eq!(idx.resolve("pkg", "./util"), Some(4));
        assert_eq!(idx.resolve("pkg/shapes/circle", "../util"), Some(4));
        // `pkg/shapes` is shapes/index.vlt: `./circle` is next to it.
        assert_eq!(idx.resolve("pkg/shapes", "./circle"), Some(3));
        assert_eq!(idx.resolve("pkg/util", "./shapes/index"), Some(2));
        assert_eq!(idx.resolve("pkg", "pkg/util"), Some(4));
        assert_eq!(idx.resolve("pkg", "../../x"), None);
        assert_eq!(idx.resolve("pkg", "other"), None);
    }

    #[test]
    fn reexports_named_star_chained_and_unresolved() {
        let mut ms = vec![
            extract(
                "pkg/lib",
                "export { Circle, area as circleArea } from \"./circle\";\nexport * from \"./square\";\n\
                 export { readFile } from \"velt:fs\";\nexport * from \"other\";\n\
                 import { helper } from \"./square\";\nexport { helper as help, local };\n\
                 // Local.\nfunction local() {}\nexport function side(): i64 { return 1; }\n",
            ),
            extract(
                "pkg/circle",
                "// A circle.\nexport class Circle {}\nexport function area(c: Circle): f64 { return 0.0; }\n",
            ),
            extract(
                "pkg/square",
                "export * from \"./lib\";\nexport { Circle as Round } from \"./circle\";\n\
                 export function side(): i64 { return 2; }\nexport function helper() {}\n\
                 extend i64 { twice(): i64 { return 0; } }\n",
            ),
        ];
        resolve(&mut ms);
        assert_eq!(
            names(&ms[0]),
            [
                ("local", None),
                ("side", None),
                ("Circle", Some("pkg/circle")),
                ("circleArea", Some("pkg/circle")),
                // `side` is the module's own; the extension is not re-exported.
                ("helper", Some("pkg/square")),
                ("Round", Some("pkg/circle")),
                ("readFile", None),
                ("*", None),
                ("help", Some("pkg/square")),
            ]
        );
        let lib = &ms[0];
        assert_eq!(lib.items[0].doc, "Local.");
        assert_eq!(lib.items[3].origin.as_ref().unwrap().name, "area");
        assert_eq!(lib.items[2].doc, "A circle.");
        assert_eq!(
            (lib.items[6].kind, lib.items[6].signature.as_str()),
            (Kind::ReExport, "export { readFile } from \"velt:fs\"")
        );
        assert_eq!(lib.items[7].signature, "export * from \"other\"");
        // The cycle square → lib → square is cut; square still gets lib's own items.
        let square: Vec<&str> = ms[2].items.iter().map(|i| i.name.as_str()).collect();
        assert!(
            square.contains(&"Round") && square.contains(&"local"),
            "{square:?}"
        );
    }
}
