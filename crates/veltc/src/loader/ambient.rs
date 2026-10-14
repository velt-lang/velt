//! Build-time constants and ambient declarations.
//!
//! - **`define`** (the root package's, as Vite's `define`): while a module parses, every
//!   reference to a defined name that nothing in its scope declares is the literal
//!   (`velt_syntax::ParseOptions::defines`), so `if (__DEV__)` with `__DEV__: "false"` is
//!   `if (false)` and its branch folds, while a parameter, local or import named `__DEV__` keeps
//!   its value (as esbuild's `define`).
//! - **Ambient declarations**: `declare const NAME: T;` (or `let`/`var`, or in
//!   `declare global { … }`) in a module or in a declaration file (`.d.ts`) of the module's
//!   package. As in TypeScript, they are global when in `declare global` or in a file without
//!   imports or exports; otherwise they belong to their module (and those of a declaration
//!   file, which no one imports here, are not read). Declaration files are not modules: each
//!   package's are read once, when the first of its modules loads; a package is the nearest
//!   directory above the module with a `package.json` or `package.vlt`, and its declaration
//!   files are those in that directory and below its `src/` (outside `node_modules`, `target`,
//!   `dist`, hidden directories and nested packages). Only their ambient variables are read
//!   (anything else in them is skipped, as are their syntax errors: they may use TypeScript
//!   Velt doesn't parse).
//!
//! Velt has no global variables, so an ambient constant needs a `define`. One that a module
//! refers to without a `define` (and without declaring the name in scope,
//! `velt_syntax::scope::free_values`) is an error at its declaration ([`Ambients::check`]), and
//! so is a `define` whose literal is not of the declared type (`boolean`, `string`, `number`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use velt_common::{Diagnostic, FileId, SourceMap};
use velt_syntax::ast::{self, Lit};
use velt_syntax::Ambient;

use super::locate::PackageResolver;

/// Directories below a package that hold no declaration files of its own.
const SKIPPED: &[&str] = &["node_modules", "target", "dist"];

pub(super) struct Ambients {
    /// `define` name → its literal (values that are not literals were reported by the
    /// manifest reader and are left out).
    defines: HashMap<String, Lit>,
    /// The `define` entries as written, for messages.
    written: BTreeMap<String, String>,
    /// Every ambient variable declared so far, with the module it belongs to (`None`: a
    /// global one).
    declared: Vec<(Ambient, Option<FileId>)>,
    /// Package directories whose declaration files have been read.
    read: HashSet<PathBuf>,
}

impl Ambients {
    pub(super) fn new(packages: Option<&dyn PackageResolver>) -> Ambients {
        let written = packages.map(|p| p.defines()).unwrap_or_default();
        let defines = written
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), vpm::manifest::define_literal(v).ok()?)))
            .collect();
        Ambients {
            defines,
            written,
            declared: vec![],
            read: HashSet::new(),
        }
    }

    /// The defines to parse a module with (`None` when there are none).
    pub(super) fn defines(&self) -> Option<&HashMap<String, Lit>> {
        (!self.defines.is_empty()).then_some(&self.defines)
    }

    /// Record the ambient declarations of loaded module `file`: global when in
    /// `declare global` or when the file has no imports or exports (as in TypeScript), else
    /// the module's own.
    pub(super) fn declared(&mut self, ambient: Vec<Ambient>, file: FileId, is_module: bool) {
        for a in ambient {
            let owner = (!a.global && is_module).then_some(file);
            self.declared.push((a, owner));
        }
    }

    /// Read the declaration files of the package containing module `path`, once per package.
    pub(super) fn read_declarations(&mut self, sm: &mut SourceMap, path: &Path) {
        let Some(root) = package_dir(path) else {
            return;
        };
        if !self.read.insert(root.clone()) {
            return;
        }
        let mut files = vec![];
        declaration_files(&root, false, &mut files);
        declaration_files(&root.join("src"), true, &mut files);
        for file in files {
            let Ok(src) = std::fs::read_to_string(&file) else {
                continue;
            };
            let id = sm.add(&file, src);
            let opts = velt_syntax::ParseOptions {
                plain_ts: true,
                defines: None,
            };
            let parsed = velt_syntax::parse_with(id, &sm.get(id).src, &opts);
            // A declaration file with imports or exports is a module no one loads here: only
            // its `declare global` is global.
            let is_module = parsed.is_module;
            let global = parsed
                .ambient
                .into_iter()
                .filter(|a| a.global || !is_module);
            self.declared.extend(global.map(|a| (a, None)));
        }
    }

    /// The errors about ambient constants: used without a `define` (`mentioned(name, in)`:
    /// whether a module refers to `name` without declaring it, any module when `in` is
    /// `None`), or with a `define` of another type than declared. One per name and module it
    /// belongs to, at its first declaration.
    pub(super) fn check(
        &self,
        mentioned: &dyn Fn(&str, Option<FileId>) -> bool,
    ) -> Vec<Diagnostic> {
        let mut out = vec![];
        let mut seen = HashSet::new();
        for (decl, owner) in &self.declared {
            let name = decl.name.name.as_str();
            if !seen.insert((name, *owner)) {
                continue;
            }
            match self.defines.get(name) {
                Some(lit) => {
                    if let Some(declared) = decl.ty.as_ref().and_then(primitive) {
                        let actual = lit_type(lit);
                        if actual != declared && actual != "null" {
                            let written = self.written.get(name).map_or("", String::as_str);
                            out.push(Diagnostic::error(
                                format!(
                                    "`{name}` is declared `{declared}`, but its `define` in package.vlt is `{written}`, a {actual}"
                                ),
                                decl.span,
                            ));
                        }
                    }
                }
                None if mentioned(name, *owner) => {
                    let example = match decl.ty.as_ref().and_then(primitive) {
                        Some("string") => "\"\\\"…\\\"\"",
                        Some("number") => "\"0\"",
                        _ => "\"false\"",
                    };
                    out.push(
                        Diagnostic::error(
                            format!("`{name}` is declared here but has no value"),
                            decl.span,
                        )
                        .with_note(format!(
                            "Velt has no global variables: give it one at build time with `define: {{ {name}: {example} }}` in package.vlt (as Vite's `define`)"
                        )),
                    );
                }
                None => {}
            }
        }
        out
    }
}

/// `boolean`, `string` or `number` when `ty` is one of them.
fn primitive(ty: &ast::TypeExpr) -> Option<&'static str> {
    let ast::TypeExprKind::Named { path, args } = &ty.kind else {
        return None;
    };
    match (path.as_slice(), args.is_empty()) {
        ([id], true) => match id.name.as_str() {
            "boolean" | "bool" => Some("boolean"),
            "string" => Some("string"),
            "number" => Some("number"),
            _ => None,
        },
        _ => None,
    }
}

fn lit_type(lit: &Lit) -> &'static str {
    match lit {
        Lit::Bool(_) => "boolean",
        Lit::Str(_) => "string",
        Lit::Int { .. } | Lit::Float { .. } => "number",
        Lit::Null => "null",
    }
}

/// The package directory of module `path`: the nearest directory above it with a
/// `package.json` or `package.vlt`.
fn package_dir(path: &Path) -> Option<PathBuf> {
    let abs = vpm::relpath::absolute(path);
    abs.ancestors().skip(1).find_map(|dir| {
        (dir.join("package.json").is_file() || dir.join(vpm::manifest::MANIFEST_FILE).is_file())
            .then(|| dir.to_path_buf())
    })
}

/// The `.d.ts` files in `dir`, and below it when `deep` (see the module docs), sorted.
fn declaration_files(dir: &Path, deep: bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let link = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
        if path.is_dir() {
            let nested = path.join("package.json").is_file()
                || path.join(vpm::manifest::MANIFEST_FILE).is_file();
            if deep && !link && !nested && !name.starts_with('.') && !SKIPPED.contains(&name) {
                declaration_files(&path, true, out);
            }
        } else if name.ends_with(".d.ts") {
            out.push(path);
        }
    }
}
