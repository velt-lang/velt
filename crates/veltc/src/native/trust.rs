//! The trust boundary of the C FFI (docs/internals/design/c-ffi.md "The trust boundary"):
//!
//! - `unsafe` code (blocks, expressions, unsafe functions and methods) is allowed only in std
//!   and in packages with native code that the program allows (`allowNative`, or the root's own
//!   package.vlt declaring native code). Elsewhere it is an error, so an audit reads only those
//!   packages.
//! - The raw API, `Ptr`, `OutPtr`, `declare c type`s and unsafe functions, is imported only
//!   there too: by name, through a namespace (`ffi.Ptr`) or re-exported by a package. User code
//!   works with a binding's safe API.
//! - In those packages, each piece of unsafe code has a `// SAFETY:` comment directly before it
//!   (an unsafe function: its doc comment's `Unsafe:` paragraph will do). std is held to the
//!   same rule by a test (`std_s_unsafe_code_has_safety_comments`), not at every build.
//!
//! The check reads the modules' ASTs; sema has already required an unsafe context for each
//! unsafe call, so confining the contexts confines the calls.

use std::collections::{HashMap, HashSet};

use velt_common::{std_module, Diagnostic, Diagnostics, SourceMap, Span};
use velt_sema::SourceModule;
use velt_syntax::ast;
use vpm::PackageGraph;

use super::unsafe_sites::{self, SiteKind};

/// std's raw types, by defining module (canonical path) and name. Unsafe functions and `declare
/// c type`s are raw by their declaration.
fn raw_std_type(module: &str, name: &str) -> bool {
    (module == std_module!(ffi_ptr) && name == "Ptr")
        || (module == std_module!(ffi_out_ptr) && name == "OutPtr")
}

/// Where unsafe code and the raw API are allowed, for the errors' first note.
const ALLOWED: &str =
    "only std and packages with native code that the program allows (`allowNative`) may use it";

/// Whether a module may contain unsafe code and import the raw API, and why not.
enum Trust {
    /// std, or a package with native code the program allows.
    Trusted,
    /// A file outside any package.
    NoPackage,
    /// A package whose package.vlt has no `native`.
    NoNativeCode(String),
    /// A package with native code the root's `allowNative` doesn't list.
    NotAllowed(String),
}

fn trust_of(m: &SourceModule, sm: &SourceMap, graph: Option<&PackageGraph>) -> Trust {
    if m.is_std {
        return Trust::Trusted;
    }
    let file = &sm.get(m.file).path;
    let Some((graph, package)) = graph.and_then(|g| Some((g, g.package_of(file)?))) else {
        return Trust::NoPackage;
    };
    if !package.has_native_code {
        Trust::NoNativeCode(package.name.clone())
    } else if !graph.allows_native(package) {
        Trust::NotAllowed(package.name.clone())
    } else {
        Trust::Trusted
    }
}

impl Trust {
    /// What to do about it, for the error's note.
    fn note(&self) -> String {
        match self {
            Trust::Trusted => String::new(),
            Trust::NoPackage => "put the native binding in a package with C libraries in its package.vlt (`native: { c: … }`), which the program allows with `allowNative`, and call its safe API".into(),
            Trust::NoNativeCode(name) => format!(
                "package `{name}` has no native code: list its C libraries in package.vlt (`native: {{ c: {{}} }}` for the platform's C library only), and allow it in the program's package.vlt with `allowNative: [\"{name}\"]`"
            ),
            Trust::NotAllowed(name) => format!(
                "if you trust package `{name}`, list it in the program's package.vlt: `allowNative: [\"{name}\"]`"
            ),
        }
    }
}

/// Check where `modules` (the loaded program) have unsafe code and raw imports, and the safety
/// comments of the trusted packages' unsafe code. Reports errors into `diags`.
pub fn check(
    modules: &[SourceModule],
    sm: &SourceMap,
    graph: Option<&PackageGraph>,
    diags: &mut Diagnostics,
) {
    let mut raw = RawExports::new(modules);
    for m in modules {
        // std is trusted, and its safety comments are a test's (module docs).
        if m.is_std {
            continue;
        }
        let trust = trust_of(m, sm, graph);
        let found = unsafe_sites::find(&m.ast);
        if let Trust::Trusted = trust {
            lint(&found.sites, &sm.get(m.file).src, diags);
            continue;
        }
        for site in &found.sites {
            let what = match &site.kind {
                SiteKind::Block | SiteKind::Expr => "`unsafe` code".to_string(),
                SiteKind::Function(name) => format!("unsafe function `{name}`"),
            };
            diags.push(
                Diagnostic::error(format!("{what} needs a native package"), site.span)
                    .with_note(ALLOWED)
                    .with_note(trust.note()),
            );
        }
        raw_imports(m, &found.qualified, &trust, &mut raw, diags);
    }
}

/// The `// SAFETY:` lint over a trusted package module's unsafe code (`src`: its text).
fn lint(sites: &[unsafe_sites::Site], src: &str, diags: &mut Diagnostics) {
    for site in sites {
        let function = matches!(site.kind, SiteKind::Function(_));
        if unsafe_sites::safety_comment(src, site.span.lo as usize, function) {
            continue;
        }
        let d = match &site.kind {
            SiteKind::Block | SiteKind::Expr => Diagnostic::error(
                "`unsafe` code needs a `// SAFETY:` comment directly before it",
                site.span,
            )
            .with_note("say why the code is sound: which guarantees the C functions' contracts need, and where they come from"),
            SiteKind::Function(name) => Diagnostic::error(
                format!("unsafe function `{name}` needs a safety comment directly before it"),
                site.span,
            )
            .with_note("say what callers must guarantee: an `Unsafe:` paragraph in its doc comment, or a `// SAFETY:` comment"),
        };
        diags.push(d);
    }
}

/// The errors for an untrusted module `m`'s imports of the raw API: named imports and
/// re-exports, and the members of namespace imports it uses (`qualified`).
fn raw_imports(
    m: &SourceModule,
    qualified: &[(String, ast::Ident)],
    trust: &Trust,
    raw: &mut RawExports,
    diags: &mut Diagnostics,
) {
    let mut namespaces: HashMap<&str, HashSet<String>> = HashMap::new();
    for item in &m.ast.items {
        let ast::ItemKind::Import(imp) = &item.kind else {
            continue;
        };
        if imp.from.is_empty() {
            continue;
        }
        let Some(target) = target_of(m, &imp.from) else {
            continue;
        };
        let exports = raw.of(target);
        if let Some(ns) = &imp.namespace {
            if !exports.is_empty() {
                namespaces.insert(&ns.name, exports.clone());
            }
            continue;
        }
        if imp.all && !exports.is_empty() {
            let mut names: Vec<&String> = exports.iter().collect();
            names.sort();
            let names: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
            let what = format!(
                "`export *` of `{}` re-exports {}, which",
                imp.from,
                names.join(", ")
            );
            diags.push(raw_error(&what, imp.from_span, trust));
        }
        for n in imp.names.iter().filter(|n| exports.contains(&n.name.name)) {
            let what = format!("`{}` of `{}`", n.name.name, imp.from);
            diags.push(raw_error(&what, n.name.span, trust));
        }
    }
    for (ns, member) in qualified {
        if namespaces
            .get(ns.as_str())
            .is_some_and(|e| e.contains(&member.name))
        {
            let what = format!("`{ns}.{}`", member.name);
            diags.push(raw_error(&what, member.span, trust));
        }
    }
}

fn raw_error(what: &str, span: Span, trust: &Trust) -> Diagnostic {
    Diagnostic::error(format!("{what} is raw native API"), span)
        .with_note(ALLOWED)
        .with_note(trust.note())
}

/// The canonical path of the module `m` imports as `spec`.
fn target_of<'a>(m: &'a SourceModule, spec: &str) -> Option<&'a str> {
    m.imports
        .iter()
        .find(|(s, _)| s == spec)
        .map(|(_, path)| path.as_str())
}

/// Each module's raw exports (the raw names others can import from it), computed on demand.
struct RawExports<'a> {
    modules: HashMap<&'a str, &'a SourceModule>,
    done: HashMap<String, HashSet<String>>,
}

impl<'a> RawExports<'a> {
    fn new(modules: &'a [SourceModule]) -> Self {
        RawExports {
            modules: modules.iter().map(|m| (m.path.as_str(), m)).collect(),
            done: HashMap::new(),
        }
    }

    /// The raw exports of the module with canonical path `path`. Modules can import each other
    /// in a cycle, so what `path` reaches is computed together, until nothing changes: a result
    /// is never kept that read a module still being computed.
    fn of(&mut self, path: &str) -> &HashSet<String> {
        if !self.done.contains_key(path) {
            let open = self.reached_from(path);
            let mut sets: HashMap<&str, HashSet<String>> = open
                .iter()
                .map(|m| (m.path.as_str(), HashSet::new()))
                .collect();
            loop {
                let mut changed = false;
                for m in &open {
                    let set = self.compute(m, &sets);
                    if set.len() != sets[m.path.as_str()].len() {
                        sets.insert(m.path.as_str(), set);
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
            for (module, set) in sets {
                self.done.insert(module.to_string(), set);
            }
            self.done.entry(path.to_string()).or_default();
        }
        &self.done[path]
    }

    /// The modules `path` reaches through imports, itself included, without those already done,
    /// dependencies first (so one pass usually finds everything).
    fn reached_from(&self, path: &str) -> Vec<&'a SourceModule> {
        let mut order = vec![];
        let mut seen: HashSet<&str> = HashSet::new();
        let Some(&first) = self.modules.get(path) else {
            return order;
        };
        // (module, how many of its imports were looked at).
        let mut stack = vec![(first, 0)];
        seen.insert(first.path.as_str());
        while let Some((m, next)) = stack.pop() {
            match m.imports.get(next) {
                Some((_, target)) => {
                    stack.push((m, next + 1));
                    let found = self.modules.get(target.as_str()).copied();
                    if let Some(t) = found.filter(|t| !self.done.contains_key(&t.path)) {
                        if seen.insert(t.path.as_str()) {
                            stack.push((t, 0));
                        }
                    }
                }
                None => order.push(m),
            }
        }
        order
    }

    /// `m`'s raw exports given what is known so far of the modules being computed (`sets`).
    fn compute(&self, m: &SourceModule, sets: &HashMap<&str, HashSet<String>>) -> HashSet<String> {
        // The module's own raw names, exported or not (a local `export { f }` can export them).
        let mut local: HashSet<String> = HashSet::new();
        let mut exported: HashSet<String> = HashSet::new();
        for item in &m.ast.items {
            let name = match &item.kind {
                ast::ItemKind::Function(f) if f.sig.is_unsafe => Some(&f.sig.name.name),
                // A C function is unsafe by its declaration.
                ast::ItemKind::ExternFn(sig) if sig.is_unsafe || sig.c.is_some() => {
                    Some(&sig.name.name)
                }
                ast::ItemKind::Struct(t) | ast::ItemKind::Class(t)
                    if t.c_type || (m.is_std && raw_std_type(&m.path, &t.name.name)) =>
                {
                    Some(&t.name.name)
                }
                _ => None,
            };
            if let Some(name) = name {
                local.insert(name.clone());
                if item.exported {
                    exported.insert(name.clone());
                }
            }
        }
        let empty = HashSet::new();
        for item in &m.ast.items {
            let ast::ItemKind::Import(imp) = &item.kind else {
                continue;
            };
            if imp.from.is_empty() {
                continue;
            }
            let Some(target) = target_of(m, &imp.from) else {
                continue;
            };
            let theirs = (sets.get(target))
                .or_else(|| self.done.get(target))
                .unwrap_or(&empty);
            if imp.all && item.exported && imp.namespace.is_none() {
                exported.extend(theirs.iter().cloned());
                continue;
            }
            for n in imp.names.iter().filter(|n| theirs.contains(&n.name.name)) {
                let name = n.alias.as_ref().unwrap_or(&n.name).name.clone();
                if item.exported {
                    exported.insert(name);
                } else {
                    local.insert(name);
                }
            }
        }
        // `export { a, b as c }` exports local names: after the imports, as one can come before
        // the import it names.
        for item in &m.ast.items {
            let ast::ItemKind::Import(imp) = &item.kind else {
                continue;
            };
            if imp.from.is_empty() {
                for n in imp.names.iter().filter(|n| local.contains(&n.name.name)) {
                    exported.insert(n.alias.as_ref().unwrap_or(&n.name).name.clone());
                }
            }
        }
        exported
    }
}
