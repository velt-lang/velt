//! Reading `package.vlt`, the manifest written in Velt
//! (docs/internals/design/package-manifest.md):
//!
//! ```ts ignore
//! import type { Package } from "velt/package";
//!
//! export const pkg: Package = {
//!   name: "hello",
//!   version: "0.1.0",
//!   dependencies: { json: "1.2", util: { path: "../util" } },
//!   paths: { "@app/*": "src/*" },
//!   jsx: { importSource: "sigx" },
//!   native: { targets: ["x86_64-unknown-linux-gnu"] },
//!   tsCompat: ["src/models"],
//!   build: { cpu: "x86-64-v3" },
//! };
//! ```
//!
//! The file is parsed, never compiled or run: the reader accepts only a type import from
//! `velt/package` and one `export const pkg: Package = <data>`, where the data is made of string,
//! number and boolean literals, arrays and object literals. It is the only definition of a valid
//! manifest; the `Package` type only gives editors completion and errors. Manifests come from
//! uploaded archives too, so the input's size, nesting and number of values are bounded.

use std::collections::{BTreeMap, HashMap};

use velt_common::{Diagnostic, Diagnostics, FileId, Span};
use velt_syntax::ast::{self, ExprKind, ItemKind, Lit, ObjectProp, PatternKind, TypeExprKind};

use super::{
    check_cpu, check_dependency, check_dependency_name, check_description, check_entry,
    check_import_source, check_keyword, check_name, check_registry, check_toolchain, check_version,
    default_entry, schema, BuildConfig, Dependency, DetailedDependency, JsxConfig, Manifest,
    Package, MAX_KEYWORDS,
};

/// File name of the manifest written in Velt.
pub const PACKAGE_FILE: &str = super::MANIFEST_FILE;
/// Largest manifest the reader parses (checked before parsing).
pub const MAX_BYTES: usize = 64 * 1024;
/// Most values (scalars, arrays, objects) one manifest may contain.
pub const MAX_VALUES: usize = 10_000;

const TYPES_MODULE: &str = "velt/package";

/// What a data file declares: `package.vlt`'s `pkg`, or `velt.workspace.vlt`'s `workspace`.
struct Shape {
    file: &'static str,
    binding: &'static str,
    type_name: &'static str,
    /// The message for a file of another shape.
    message: &'static str,
}

const PACKAGE_SHAPE: Shape = Shape {
    file: PACKAGE_FILE,
    binding: "pkg",
    type_name: "Package",
    message: "the manifest must be one `export const pkg: Package = { … }`",
};

impl Manifest {
    /// Read and validate the text of a `package.vlt`. Every problem is a diagnostic in `file`.
    pub fn read(file: FileId, src: &str) -> Result<Manifest, Diagnostics> {
        read_with_value(file, src).map(|(manifest, _)| manifest)
    }
}

/// [`Manifest::read`], also returning the data the manifest was decoded from (for the checks
/// that point at its values, like [`missing_ts_compat_dirs`]), from one parse.
fn read_with_value(file: FileId, src: &str) -> Result<(Manifest, Value), Diagnostics> {
    read_data(file, src, &PACKAGE_SHAPE, |r, v| r.manifest(v))
}

/// Parse `src`, check that it is one declaration of `shape` made of data, and decode the data
/// with `decode`, which reports problems on the reader.
fn read_data<T>(
    file: FileId,
    src: &str,
    shape: &'static Shape,
    decode: impl FnOnce(&mut Reader, &Value) -> T,
) -> Result<(T, Value), Diagnostics> {
    if src.len() > MAX_BYTES {
        return Err(vec![Diagnostic::error(
            format!("the manifest is larger than {} KiB", MAX_BYTES / 1024),
            Span::new(file, 0, 0),
        )]);
    }
    let (module, diags) = velt_syntax::parse_file(file, src);
    if diags.iter().any(Diagnostic::is_error) {
        return Err(diags);
    }
    let mut reader = Reader {
        src,
        shape,
        diags: Vec::new(),
        values: 0,
        too_many: false,
    };
    let init = reader.declaration(&module);
    let value = init.and_then(|e| reader.value(e));
    if let Some(value) = value {
        let decoded = decode(&mut reader, &value);
        if reader.diags.is_empty() {
            return Ok((decoded, value));
        }
    }
    if reader.diags.is_empty() {
        reader
            .diags
            .push(Diagnostic::error(shape.message, module.span));
    }
    Err(reader.diags)
}

/// A data value of the manifest, with its location.
struct Value {
    kind: ValueKind,
    span: Span,
}

enum ValueKind {
    Str(String),
    Num,
    Bool(bool),
    Array(Vec<Value>),
    Object(Vec<(ast::Ident, Value)>),
}

impl ValueKind {
    fn describe(&self) -> &'static str {
        match self {
            ValueKind::Str(_) => "a string",
            ValueKind::Num => "a number",
            ValueKind::Bool(_) => "a boolean",
            ValueKind::Array(_) => "an array",
            ValueKind::Object(_) => "an object",
        }
    }
}

struct Reader<'s> {
    src: &'s str,
    shape: &'static Shape,
    diags: Diagnostics,
    values: usize,
    too_many: bool,
}

impl Reader<'_> {
    fn error(&mut self, message: impl Into<String>, span: Span) {
        self.diags.push(Diagnostic::error(message, span));
    }

    /// Check the file's items and return the initializer of `export const pkg: Package` (or the
    /// declaration of another [`Shape`]).
    fn declaration<'m>(&mut self, module: &'m ast::Module) -> Option<&'m ast::Expr> {
        let mut seen_import = false;
        let mut init = None;
        for item in &module.items {
            match &item.kind {
                ItemKind::Import(import) if !seen_import && !item.exported => {
                    seen_import = true;
                    let types_only = import.from == TYPES_MODULE
                        && import.namespace.is_none()
                        && !import.all
                        && !import.names.is_empty()
                        && import.names.iter().all(|n| n.type_only);
                    if !types_only {
                        self.error(
                            format!(
                                "`{}` may only import types from `{TYPES_MODULE}`",
                                self.shape.file
                            ),
                            item.span,
                        );
                    }
                }
                ItemKind::Import(_) if !item.exported => {
                    self.error(
                        format!("`{}` may have at most one import", self.shape.file),
                        item.span,
                    );
                }
                ItemKind::Var(var) if init.is_none() && is_declaration(item, var, self.shape) => {
                    init = var.init.as_ref();
                }
                _ => self.error(self.shape.message, item.span),
            }
        }
        if init.is_none() && self.diags.is_empty() {
            self.error(self.shape.message, module.span);
        }
        init
    }

    /// The data subset: everything else is reported where it is written.
    fn value(&mut self, expr: &ast::Expr) -> Option<Value> {
        if self.too_many {
            return None;
        }
        self.values += 1;
        if self.values > MAX_VALUES {
            self.too_many = true;
            self.error(
                format!("the manifest has more than {MAX_VALUES} values"),
                expr.span,
            );
            return None;
        }
        let kind = match &expr.kind {
            ExprKind::Lit(Lit::Str(s)) => ValueKind::Str(s.clone()),
            ExprKind::Lit(Lit::Int { .. } | Lit::Float { .. }) => ValueKind::Num,
            ExprKind::Lit(Lit::Bool(b)) => ValueKind::Bool(*b),
            ExprKind::Lit(Lit::Null) => {
                self.error("`null` is not allowed; leave the key out", expr.span);
                return None;
            }
            ExprKind::Array(elems) => {
                let mut values = Vec::with_capacity(elems.len());
                let mut ok = true;
                for elem in elems {
                    match self.value(elem) {
                        Some(v) => values.push(v),
                        None => ok = false,
                    }
                }
                if !ok {
                    return None;
                }
                ValueKind::Array(values)
            }
            ExprKind::Object(props) => ValueKind::Object(self.props(props)?),
            other => {
                self.error(
                    format!(
                        "the manifest is data only: {} not allowed",
                        self.not_data(other, expr.span)
                    ),
                    expr.span,
                );
                return None;
            }
        };
        Some(Value {
            kind,
            span: expr.span,
        })
    }

    fn props(&mut self, props: &[ObjectProp]) -> Option<Vec<(ast::Ident, Value)>> {
        let mut out: Vec<(ast::Ident, Value)> = Vec::new();
        let mut seen: HashMap<&str, Span> = HashMap::new();
        let mut ok = true;
        for prop in props {
            match prop {
                ObjectProp::KeyValue(key, expr) => {
                    if let Some(&first) = seen.get(key.name.as_str()) {
                        self.diags.push(
                            Diagnostic::error(format!("duplicate key `{}`", key.name), key.span)
                                .with_label(first, "first written here"),
                        );
                        ok = false;
                    }
                    seen.entry(&key.name).or_insert(key.span);
                    match self.value(expr) {
                        Some(v) => out.push((key.clone(), v)),
                        None => ok = false,
                    }
                }
                ObjectProp::Shorthand(key) => {
                    self.error(
                        format!(
                            "the manifest is data only: shorthand properties are not allowed (write `{0}: …`)",
                            key.name
                        ),
                        key.span,
                    );
                    ok = false;
                }
                ObjectProp::Spread(expr) => {
                    self.error(
                        "the manifest is data only: spreads are not allowed",
                        expr.span,
                    );
                    ok = false;
                }
                ObjectProp::Method(f) => {
                    self.error(
                        "the manifest is data only: methods are not allowed",
                        f.sig.name.span,
                    );
                    ok = false;
                }
            }
        }
        ok.then_some(out)
    }

    /// Decode the data into a [`Manifest`], checking every field where it is written.
    fn manifest(&mut self, value: &Value) -> Manifest {
        let mut manifest = Manifest {
            registry: None,
            package: Package {
                name: String::new(),
                version: String::new(),
                description: None,
                keywords: vec![],
                entry: default_entry(),
            },
            toolchain: None,
            dependencies: BTreeMap::new(),
            paths: BTreeMap::new(),
            native: None,
            jsx: None,
            define: BTreeMap::new(),
            ts_compat: vec![],
            build: None,
        };
        let Some(fields) = self.object(value, "the manifest") else {
            return manifest;
        };
        let (mut name, mut version) = (false, false);
        for (key, v) in fields {
            match key.name.as_str() {
                "name" => {
                    name = true;
                    if let Some(s) = self.string(v, "name") {
                        self.check(check_name(s), v.span);
                        manifest.package.name = s.to_string();
                    }
                }
                "version" => {
                    version = true;
                    if let Some(s) = self.string(v, "version") {
                        self.check(check_version(s).map_err(|e| format!("version {e}")), v.span);
                        manifest.package.version = s.to_string();
                    }
                }
                "velt" => {
                    if let Some(s) = self.string(v, "velt") {
                        self.check(check_toolchain(s).map_err(|e| format!("velt {e}")), v.span);
                        manifest.toolchain = Some(s.to_string());
                    }
                }
                "description" => {
                    if let Some(s) = self.string(v, "description") {
                        self.check(check_description(s), v.span);
                        manifest.package.description = Some(s.to_string());
                    }
                }
                "keywords" => manifest.package.keywords = self.keywords(v),
                "entry" => {
                    if let Some(s) = self.string(v, "entry") {
                        self.check(check_entry(s), v.span);
                        manifest.package.entry = s.to_string();
                    }
                }
                "registry" => {
                    if let Some(s) = self.string(v, "registry") {
                        self.check(check_registry(s), v.span);
                        manifest.registry = Some(s.to_string());
                    }
                }
                "dependencies" => manifest.dependencies = self.dependencies(v),
                "paths" => manifest.paths = self.paths(v),
                "jsx" => manifest.jsx = self.jsx(v),
                "define" => manifest.define = self.define(v),
                "native" => manifest.native = self.native(v),
                "tsCompat" => manifest.ts_compat = self.ts_compat(v),
                "build" => manifest.build = self.build(v),
                _ => self.unknown_key(key, schema::PACKAGE),
            }
        }
        for (present, key) in [(name, "name"), (version, "version")] {
            if !present {
                self.error(format!("the manifest is missing `{key}`"), value.span);
            }
        }
        if manifest.native.is_some() && super::split_scoped(&manifest.package.name).is_some() {
            self.error(
                "a package with a scoped name (`@scope/name`) cannot have `native` code yet",
                value.span,
            );
        }
        manifest
    }

    /// `keywords`: at most [`MAX_KEYWORDS`] valid, distinct words, in the order written.
    fn keywords(&mut self, value: &Value) -> Vec<String> {
        let ValueKind::Array(elems) = &value.kind else {
            self.error(
                format!("`keywords` must be an array, not {}", value.kind.describe()),
                value.span,
            );
            return vec![];
        };
        if elems.is_empty() {
            self.error("`keywords` is empty; remove the field instead", value.span);
        } else if elems.len() > MAX_KEYWORDS {
            self.error(
                format!(
                    "`keywords` has {} entries; at most {MAX_KEYWORDS} are allowed",
                    elems.len()
                ),
                value.span,
            );
        }
        let mut words: Vec<String> = vec![];
        for elem in elems {
            let ValueKind::Str(word) = &elem.kind else {
                self.error(
                    format!(
                        "`keywords` entries must be strings, not {}",
                        elem.kind.describe()
                    ),
                    elem.span,
                );
                continue;
            };
            if words.contains(word) {
                self.error(format!("duplicate keyword `{word}`"), elem.span);
                continue;
            }
            self.check(check_keyword(word), elem.span);
            words.push(word.clone());
        }
        words
    }

    fn dependencies(&mut self, value: &Value) -> BTreeMap<String, Dependency> {
        let mut out = BTreeMap::new();
        for (key, v) in self.object(value, "`dependencies`").unwrap_or_default() {
            self.check(check_dependency_name(&key.name), key.span);
            let dep = match &v.kind {
                ValueKind::Str(req) => Dependency::Version(req.clone()),
                ValueKind::Object(fields) => {
                    let mut detailed = DetailedDependency::default();
                    for (k, fv) in fields {
                        let slot = match k.name.as_str() {
                            "version" => &mut detailed.version,
                            "path" => &mut detailed.path,
                            "git" => &mut detailed.git,
                            "branch" => &mut detailed.branch,
                            "tag" => &mut detailed.tag,
                            "rev" => &mut detailed.rev,
                            _ => {
                                self.unknown_key(k, schema::DEPENDENCY);
                                continue;
                            }
                        };
                        *slot = self.string(fv, &k.name).map(str::to_string);
                    }
                    Dependency::Detailed(detailed)
                }
                other => {
                    self.error(
                        format!(
                            "dependency `{}` must be a version requirement or an object, not {}",
                            key.name,
                            other.describe()
                        ),
                        v.span,
                    );
                    continue;
                }
            };
            self.check(check_dependency(&key.name, &dep), v.span);
            out.insert(key.name.clone(), dep);
        }
        out
    }

    fn paths(&mut self, value: &Value) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (key, v) in self.object(value, "`paths`").unwrap_or_default() {
            let Some(target) = self.string(v, &key.name) else {
                continue;
            };
            if let Err(why) = crate::paths::check_alias(&key.name, target) {
                self.error(format!("alias `{}`: {why}", key.name), key.span.to(v.span));
            }
            out.insert(key.name.clone(), target.to_string());
        }
        out
    }

    fn define(&mut self, value: &Value) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (key, v) in self.object(value, "`define`").unwrap_or_default() {
            if !super::is_define_name(&key.name) {
                self.error(
                    format!("define `{}`: the name must be an identifier", key.name),
                    key.span,
                );
            }
            let Some(text) = self.string(v, &key.name) else {
                continue;
            };
            if let Err(why) = super::define_literal(text) {
                self.error(format!("define `{}`: {why}", key.name), v.span);
            }
            out.insert(key.name.clone(), text.to_string());
        }
        out
    }

    fn jsx(&mut self, value: &Value) -> Option<JsxConfig> {
        let mut config = JsxConfig::default();
        for (key, v) in self.object(value, "`jsx`")? {
            if key.name != "importSource" {
                self.unknown_key(key, schema::JSX);
                continue;
            }
            if let Some(s) = self.string(v, "importSource") {
                self.check(check_import_source(s), v.span);
                config.import_source = Some(s.to_string());
            }
        }
        Some(config)
    }

    fn build(&mut self, value: &Value) -> Option<BuildConfig> {
        let mut config = BuildConfig::default();
        for (key, v) in self.object(value, "`build`")? {
            if key.name != "cpu" {
                self.unknown_key(key, schema::BUILD);
                continue;
            }
            if let Some(s) = self.string(v, "cpu") {
                self.check(check_cpu(s), v.span);
                config.cpu = Some(s.to_string());
            }
        }
        Some(config)
    }

    fn object<'v>(&mut self, value: &'v Value, what: &str) -> Option<&'v [(ast::Ident, Value)]> {
        match &value.kind {
            ValueKind::Object(fields) => Some(fields),
            other => {
                self.error(
                    format!("{what} must be an object, not {}", other.describe()),
                    value.span,
                );
                None
            }
        }
    }

    fn string<'v>(&mut self, value: &'v Value, key: &str) -> Option<&'v str> {
        match &value.kind {
            ValueKind::Str(s) => Some(s),
            other => {
                self.error(
                    format!("`{key}` must be a string, not {}", other.describe()),
                    value.span,
                );
                None
            }
        }
    }

    fn not_data(&self, kind: &ExprKind, span: Span) -> &'static str {
        let text = self.src.get(span.lo as usize..span.hi as usize);
        not_data(kind, text.is_some_and(|t| t.starts_with('/')))
    }

    fn check(&mut self, result: Result<(), String>, span: Span) {
        if let Err(message) = result {
            self.error(message, span);
        }
    }

    fn unknown_key(&mut self, key: &ast::Ident, known: &[schema::Field]) {
        let mut d = Diagnostic::error(
            format!("unknown key `{}` in the manifest", key.name),
            key.span,
        );
        // About one edit per three characters: `dependecies` → `dependencies`, but `x` → nothing.
        let max = key.name.chars().count() / 3;
        let mut keys = known.iter().map(|f| f.key);
        if let Some(close) = keys.find(|k| edit_distance(k, &key.name) <= max) {
            d = d.with_note(format!("did you mean `{close}`?"));
        }
        self.diags.push(d);
    }
}

/// `export const pkg: Package = …` (the binding and type of `shape`).
fn is_declaration(item: &ast::Item, var: &ast::VarDecl, shape: &Shape) -> bool {
    let named = matches!(&var.pattern.kind, PatternKind::Ident(id) if id.name == shape.binding);
    let typed = matches!(
        var.ty.as_ref().map(|t| &t.kind),
        Some(TypeExprKind::Named { path, args }) if args.is_empty()
            && path.len() == 1
            && path[0].name == shape.type_name
    );
    item.exported && var.kind == ast::VarKind::Const && named && typed && var.init.is_some()
}

/// What a non-data expression is, for "… not allowed".
fn not_data(kind: &ExprKind, regex: bool) -> &'static str {
    match kind {
        // The parser lowers `/a/` to `new RegExp("a", "")`.
        ExprKind::New { .. } if regex => "regular expressions are",
        ExprKind::Template { .. } => "template literals are",
        ExprKind::Ident(_) | ExprKind::This | ExprKind::Super => "names are",
        ExprKind::Call { .. } => "calls are",
        ExprKind::New { .. } => "`new` is",
        ExprKind::Unary { .. }
        | ExprKind::Binary { .. }
        | ExprKind::Assign { .. }
        | ExprKind::Update { .. }
        | ExprKind::Cond { .. }
        | ExprKind::InstanceOf { .. } => "operators are",
        ExprKind::Cast { .. } => "`as` is",
        ExprKind::Spread(_) => "spreads are",
        ExprKind::Paren(_) => "parentheses are",
        _ => "expressions are",
    }
}

/// Edits between `a` and `b`: insertions, deletions, substitutions and swaps of two adjacent
/// characters (`jsno` → `json` is one), as Damerau's optimal string alignment counts them.
pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    // Rows `i - 2`, `i - 1` and `i` of the distance table.
    let mut before: Vec<usize> = vec![0; b.len() + 1];
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut row = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            row[j] = (prev[j - 1] + cost).min(prev[j] + 1).min(row[j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                row[j] = row[j].min(before[j - 2] + 1);
            }
        }
        before = std::mem::replace(&mut prev, row);
    }
    prev[b.len()]
}

mod native;
#[cfg(test)]
mod tests;
mod ts_compat;
mod workspace;

pub use workspace::read_workspace;

pub use ts_compat::missing_ts_compat_dirs;
