//! One `.d.ts` file of the type package being assembled ([`DeclFile`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use velt_common::std_modules;

use super::globals::TS_GLOBALS;
use super::{rel_of, DeclAt, Package};
use crate::extract::Kind;
use crate::ts::FIRST_PARAM;

/// Where an extension's target interface is.
enum Place {
    /// A type of the prelude (declared by globals.d.ts).
    Global,
    /// Declared in the file being written.
    Here { exported: bool },
    /// Declared by the public module with this specifier.
    Module(String),
}

/// Declarations `text` with the declared name `from` (after the keyword on the lines that start
/// a declaration) changed to `to`.
fn renamed(text: &str, from: &str, to: &str) -> String {
    if from == to {
        return text.to_string();
    }
    let mut out = String::new();
    for line in text.lines() {
        let starts = !line.is_empty() && !line.starts_with([' ', '}']) && !line.starts_with("/**");
        let at = line.find(&format!(" {from}")).filter(|&i| {
            let rest = &line[i + 1 + from.len()..];
            !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '$')
        });
        match (starts, at) {
            (true, Some(i)) => {
                out.push_str(&line[..=i]);
                out.push_str(to);
                out.push_str(&line[i + 1 + from.len()..]);
            }
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// `name` or `name as alias`.
pub(super) fn rename(name: &str, alias: &str) -> String {
    match name == alias {
        true => name.to_string(),
        false => format!("{name} as {alias}"),
    }
}

/// One `.d.ts` file being assembled: the public module `module`'s, or `globals.d.ts`.
pub(super) struct DeclFile<'p, 'a> {
    package: &'p Package<'a>,
    module: Option<usize>,
    /// `import type { … } from "velt/…"` lines by specifier: `(name there, local name)`.
    imports: BTreeMap<String, BTreeSet<(String, String)>>,
    pub(super) reexports: Vec<String>,
    /// Declarations, in order.
    decls: Vec<String>,
    /// Declarations made or imported, and the local name they have here.
    local: HashMap<DeclAt, String>,
    /// The declarations made here by name, and whether each is exported.
    declared: HashMap<String, bool>,
    /// Extensions of other modules' types: `declare module "velt/…"` blocks by specifier.
    augments: BTreeMap<String, Vec<String>>,
}

impl<'p, 'a> DeclFile<'p, 'a> {
    pub(super) fn new(package: &'p Package<'a>, module: Option<usize>) -> DeclFile<'p, 'a> {
        DeclFile {
            package,
            module,
            imports: BTreeMap::new(),
            reexports: vec![],
            decls: vec![],
            local: HashMap::new(),
            declared: HashMap::new(),
            augments: BTreeMap::new(),
        }
    }

    /// Declare what is at `at` as `name`: exported (`export`), or not (in `globals.d.ts`: a
    /// global).
    pub(super) fn declare(&mut self, at: &DeclAt, name: &str, export: bool) {
        let p = self.package;
        let Some(it) = p.own[at.0].get(&at.1) else {
            return;
        };
        if let Some(local) = self.local.get(at).cloned() {
            // Declared already (privately, for a declaration that refers to it).
            if export {
                self.reexports
                    .push(format!("export {{ {} }};", rename(&local, name)));
            } else if local != name {
                self.alias(name, &local, &it.generics);
            }
            return;
        }
        if self.declared.contains_key(name) {
            return;
        }
        self.local.insert(at.clone(), name.to_string());
        self.declared.insert(name.to_string(), export);
        let keyword = if export {
            "export declare "
        } else {
            "declare "
        };
        let text = with_keyword(&renamed(&it.ts.text, &it.ts.name, name), keyword);
        let slot = self.decls.len();
        self.decls.push(String::new());
        for (r, arity) in &it.ts.refs {
            self.reference(at.0, r, *arity);
        }
        self.decls[slot] = text;
    }

    /// `type name<A, B> = to<A, B>;` (`generics`: the type parameters' names).
    fn alias(&mut self, name: &str, to: &str, generics: &[String]) {
        if self.declared.contains_key(name) {
            return;
        }
        self.declared.insert(name.to_string(), false);
        let params = match generics.is_empty() {
            true => String::new(),
            false => format!("<{}>", generics.join(", ")),
        };
        self.decls
            .push(format!("declare type {name}{params} = {to}{params};"));
    }

    /// Make type name `name`, written in module `from` with `arity` type arguments, resolve
    /// here. A type that is not part of the API (a file's own class in a constructor that
    /// only std calls) is opaque: `unknown`.
    fn reference(&mut self, from: usize, name: &str, arity: usize) {
        let p = self.package;
        let Some(at) = p.resolve_ref(from, name) else {
            if !TS_GLOBALS.contains(&name) && !self.declared.contains_key(name) {
                self.declared.insert(name.to_string(), false);
                let params: Vec<String> = (0..arity).map(|i| format!("T{i} = unknown")).collect();
                let params = match params.is_empty() {
                    true => String::new(),
                    false => format!("<{}>", params.join(", ")),
                };
                self.decls.push(format!(
                    "/** Internal to the Velt standard library. */\ndeclare type {name}{params} = unknown;"
                ));
            }
            return;
        };
        let generics = p.own[at.0]
            .get(&at.1)
            .map(|d| d.generics.clone())
            .unwrap_or_default();
        if let Some(local) = self.local.get(&at).cloned() {
            if local != name {
                self.alias(name, &local, &generics);
            }
            return;
        }
        // The prelude's: TypeScript's own, or declared by globals.d.ts (and imported from it).
        if p.global_files.contains(&at.0) {
            if self.module.is_some() && !TS_GLOBALS.contains(&at.1.as_str()) {
                self.imports
                    .entry(self.globals_spec())
                    .or_default()
                    .insert((at.1.clone(), name.to_string()));
                self.declared.insert(name.to_string(), false);
                self.local.insert(at, name.to_string());
            }
            return;
        }
        match p.home(&at) {
            Some((home, exported)) if Some(home) != self.module => {
                let spec = std_modules::specifier(&p.modules[home].name).unwrap_or_default();
                if !self.declared.contains_key(name) {
                    self.imports
                        .entry(spec)
                        .or_default()
                        .insert((exported, name.to_string()));
                    self.declared.insert(name.to_string(), false);
                    self.local.insert(at, name.to_string());
                }
            }
            // Exported here (declared before or after this): under another name, an alias.
            Some((_, exported)) => {
                if exported != name {
                    self.alias(name, &exported, &generics);
                }
            }
            None => self.declare(&at, name, false),
        }
    }

    /// The extensions in the files `wanted` picks, added to their targets' interfaces: merged
    /// with the declaration made here, or in a `declare module` block for another module's type
    /// (globals.d.ts's for the prelude's). An extension of TypeScript's own types (`Array`,
    /// `string`) is left out: declaring it would change the client's globals, which a type-only
    /// import must not. So is one of a type that is not an interface or class in TypeScript.
    pub(super) fn extensions(&mut self, wanted: impl Fn(usize) -> bool) {
        let p = self.package;
        for f in (0..p.modules.len()).filter(|&f| wanted(f)) {
            for it in p.modules[f]
                .items
                .iter()
                .filter(|it| it.kind == Kind::Extension)
            {
                let Some(ext) = &it.ts.ext else {
                    continue;
                };
                let Some((place, name, params)) = self.target(f, &ext.target) else {
                    continue;
                };
                let direct = ext.args == params;
                let members: String = ext
                    .members
                    .iter()
                    .filter_map(|m| match direct {
                        true => Some(m.direct.clone()),
                        false => m.this_form.clone(),
                    })
                    .collect();
                let members = match params.first() {
                    Some(first) => members.replace(FIRST_PARAM, first),
                    None => members,
                };
                if members.is_empty() {
                    continue;
                }
                for (r, arity) in it.ts.refs.iter().filter(|(r, _)| *r != ext.target) {
                    self.reference(f, r, *arity);
                }
                let params = match params.is_empty() {
                    true => String::new(),
                    false => format!("<{}>", params.join(", ")),
                };
                let text = format!("interface {name}{params} {{\n{members}}}");
                let place = match place {
                    Place::Global if self.module.is_none() => Place::Here { exported: true },
                    Place::Global => Place::Module(self.globals_spec()),
                    place => place,
                };
                match place {
                    Place::Global => {}
                    Place::Here { exported: true } => self.decls.push(format!("export {text}")),
                    Place::Here { exported: false } => self.decls.push(text),
                    Place::Module(spec) => self.augments.entry(spec).or_default().push(text),
                }
            }
        }
    }

    /// Where the interface of extension target `target` (written in module `f`) is, its name
    /// there and its type parameters' names; `None` for TypeScript's own types.
    fn target(&self, f: usize, target: &str) -> Option<(Place, String, Vec<String>)> {
        let p = self.package;
        // A Velt declaration the module sees first (`Request` of `velt/net/http`), then
        // TypeScript's globals (whose parameters a merge must repeat).
        let at = p
            .resolve_ref(f, target)
            .filter(|at| !p.global_files.contains(&at.0));
        let Some(at) = at else {
            if TS_GLOBALS.contains(&target) {
                return None;
            }
            let at = p.resolve_ref(f, target)?;
            let decl = p.own[at.0].get(&at.1)?;
            return decl
                .ts
                .merges
                .then(|| (Place::Global, at.1.clone(), decl.generics.clone()));
        };
        let decl = p.own[at.0].get(&at.1)?;
        if !decl.ts.merges {
            return None;
        }
        let params = decl.generics.clone();
        if let Some(&exported) = self
            .declared
            .get(&at.1)
            .filter(|_| self.local.contains_key(&at))
        {
            return Some((Place::Here { exported }, at.1, params));
        }
        let (home, name) = p.home(&at)?;
        let spec = std_modules::specifier(&p.modules[home].name)?;
        Some((Place::Module(spec), name, params))
    }

    /// The specifier of globals.d.ts from this file (`../globals.js`).
    fn globals_spec(&self) -> String {
        let depth = self.module.map_or(0, |m| {
            rel_of(&self.package.modules[m].name).split('/').count()
        });
        match depth {
            0 => "./globals.js".to_string(),
            n => format!("{}globals.js", "../".repeat(n)),
        }
    }

    pub(super) fn finish(self) -> String {
        let mut out = String::from("// Generated by `velt doc --types`: do not edit.\n");
        for (spec, names) in &self.imports {
            let names: Vec<String> = names.iter().map(|(n, l)| rename(n, l)).collect();
            out.push_str(&format!(
                "import type {{ {} }} from \"{spec}\";\n",
                names.join(", ")
            ));
        }
        out.push('\n');
        for d in self.decls.iter().filter(|d| !d.is_empty()) {
            out.push_str(d);
            out.push_str("\n\n");
        }
        for (spec, exts) in &self.augments {
            out.push_str(&format!("declare module \"{spec}\" {{\n"));
            for e in exts {
                out.push_str(&indent(e));
            }
            out.push_str("}\n\n");
        }
        for r in &self.reexports {
            out.push_str(r);
            out.push('\n');
        }
        out.push_str("export {};\n");
        out
    }
}

/// `text` (declarations, each with its doc comment above) with `keyword` before each
/// declaration: the lines that start one, not indented and not a comment or a closing brace.
fn with_keyword(text: &str, keyword: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let starts = !line.is_empty() && !line.starts_with([' ', '}']) && !line.starts_with("/**");
        if starts {
            out.push_str(keyword);
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// `text` indented by two spaces.
fn indent(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        out.push_str(&format!("  {line}\n"));
    }
    out
}
