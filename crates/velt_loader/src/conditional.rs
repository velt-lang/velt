//! `#name` specifiers: conditional imports ([`vpm::imports`]). A module imports `#name` from the
//! `imports` of its own package (`package.velt`), or, for a std module, from std's map
//! (`std/imports.velt`); the build's conditions ([`LoadOptions::target`](super::LoadOptions))
//! select the variant, which is then loaded like a relative import of that file.
//!
//! A `#name` resolves only in the package (or std) that defines it, so a package's variants are
//! its own modules; std's are internal files of a module, never a module a program imports.

use std::path::{Path, PathBuf};

use velt_common::{std_modules, Span};
use vpm::imports::{self, ImportMap, Variant, DEFAULT, STD_IMPORTS_FILE};

use super::locate::{LocateError, Origin, PackageResolver};
use super::Loader;

/// A `#name` a module of the program imported: what [`super::variants`] compares.
pub(super) struct Resolved {
    /// `#name`.
    pub name: String,
    /// The directory the variants' paths are relative to (a package's root, the std root).
    pub base: PathBuf,
    /// Its variants, in the order written.
    pub variants: Vec<Variant>,
    /// The file of the variant this build uses (not compared when it failed to load: the
    /// import's error says why).
    pub selected: PathBuf,
    /// Where it was first imported.
    pub span: Span,
}

/// The conditions of a build for `target` (`None`: this machine) and, with `--host`, its host
/// package's condition `host`.
pub(super) fn build_conditions(target: Option<&str>, host: Option<&str>) -> Vec<String> {
    match target {
        Some(triple) => imports::conditions(triple, host),
        None => imports::conditions(&velt_toolchain::release::host_triple(), host),
    }
}

/// The conditions of the build `opts` loads for.
pub(super) fn load_conditions(opts: &super::LoadOptions) -> Vec<String> {
    build_conditions(opts.target.as_deref(), opts.host_condition.as_deref())
}

/// Where the `#name`s of a module resolve.
struct Scope {
    /// "package `app`", "the standard library".
    owner: String,
    /// The directory the variants' paths are relative to.
    base: PathBuf,
    map: ImportMap,
    std: bool,
}

impl Loader<'_, '_> {
    /// The file the `#name` `spec`, imported by `importer` (of origin `from`, at `span`), names
    /// in this build.
    pub(super) fn conditional(
        &mut self,
        spec: &str,
        importer: &Path,
        from: &Origin,
        span: Span,
    ) -> Result<PathBuf, LocateError> {
        if !matches!(from, Origin::Std(_)) {
            if let Some(npm) = npm_scope(&mut self.npm, importer, self.opts.packages) {
                return self.npm.resolve_import(&npm, spec);
            }
        }
        let scope = match from {
            Origin::Std(root) => Scope {
                owner: "the standard library".into(),
                base: root.clone(),
                map: self.std_imports(root).clone(),
                std: true,
            },
            _ => package_scope(spec, importer, self.opts.packages)?,
        };
        let Some(variants) = scope.map.get(spec) else {
            let std_has = !scope.std
                && self
                    .opts
                    .std_root
                    .clone()
                    .is_some_and(|root| self.std_imports(&root).contains_key(spec));
            return Err(unknown(spec, &scope, std_has));
        };
        let file = select(spec, variants, &self.conditions, &scope)?;
        if scope.std {
            let rel = vpm::relpath::relative(&file, &scope.base);
            if std_modules::importable(&rel) {
                return Err((
                    format!("`{spec}` names `{rel}`, a module of the standard library"),
                    vec!["a conditional import's variants are internal files of a module".into()],
                ));
            }
        }
        let known = (self.variant_checks.iter()).any(|r| r.name == spec && r.base == scope.base);
        if self.opts.check_variants && !known {
            self.variant_checks.push(Resolved {
                name: spec.to_string(),
                base: scope.base,
                variants: variants.clone(),
                selected: file.clone(),
                span,
            });
        }
        Ok(file)
    }

    /// std's conditional imports: `std/imports.velt` below `root`, read once (none without the
    /// file; its errors are reported in it).
    fn std_imports(&mut self, root: &Path) -> &ImportMap {
        if self.std_imports.is_none() {
            let path = root.join(STD_IMPORTS_FILE);
            let map = match self.read(&path) {
                Ok(src) => {
                    let file = self.sm.add(&path, src);
                    match vpm::manifest::read::read_std_imports(file, &self.sm.get(file).src) {
                        Ok(map) => map,
                        Err(diags) => {
                            self.diags.extend(diags);
                            ImportMap::new()
                        }
                    }
                }
                Err(_) => ImportMap::new(),
            };
            self.std_imports = Some(map);
        }
        self.std_imports.get_or_insert_with(ImportMap::new)
    }
}

/// The npm package scope of `importer` when it belongs to an npm package rather than to a Velt
/// package: its nearest directory with a `package.json` (Node's package scope), whose `imports`
/// its `#name`s then resolve through, as in Node, never a Velt package's map. Outside every Velt
/// package that is any `package.json`. Inside one, the file belongs to an npm package when a
/// `package.json` between it and the Velt root (the root's own excluded) is an npm package's: it
/// names a package, has `exports` or `imports`, is a workspace member (`pnpm-workspace.yaml`,
/// `workspaces`), lies below `node_modules`, or comes from a git dependency. Only nameless folder
/// files (`{ "type": "module" }`) all the way up leave its `#name`s to `package.velt`.
fn npm_scope(
    npm: &mut vpm::npm::NpmResolver,
    importer: &Path,
    packages: Option<&dyn PackageResolver>,
) -> Option<PathBuf> {
    let scope = npm.package_scope(importer)?;
    let key = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let Some(velt) = packages.and_then(|p| p.imports(importer)) else {
        return Some(scope);
    };
    let root = key(&velt.root);
    let mut dir = Some(scope.clone());
    while let Some(at) = dir {
        let at_key = key(&at);
        let Ok(below) = at_key.strip_prefix(&root) else {
            return None;
        };
        if below.as_os_str().is_empty() {
            return None;
        }
        let in_node_modules = below.components().any(|c| c.as_os_str() == "node_modules");
        let provided = npm.package_name(&at).is_some_and(|name| {
            let dir = packages.and_then(|p| p.npm_package(importer, &name));
            dir.is_some_and(|d| key(&d) == at_key)
        });
        if in_node_modules || provided || npm.is_npm_package(&at) {
            return Some(scope);
        }
        dir = at.parent().and_then(|p| npm.package_scope(&p.join("_")));
    }
    None
}

/// The package `importer` belongs to, whose `imports` its `#name`s resolve in.
fn package_scope(
    spec: &str,
    importer: &Path,
    packages: Option<&dyn PackageResolver>,
) -> Result<Scope, LocateError> {
    match packages.and_then(|p| p.imports(importer)) {
        Some(p) => Ok(Scope {
            owner: format!("package `{}`", p.name),
            base: p.root,
            map: p.map,
            std: false,
        }),
        None => Err((
            format!(
                "`{spec}` names a conditional import of the importing package, and `{}` is in no package",
                importer.display()
            ),
            vec!["a `#name` resolves through `imports` in the `package.velt` of the package that imports it".into()],
        )),
    }
}

/// The error for `spec`, which `scope` does not define (`std_has`: std does).
fn unknown(spec: &str, scope: &Scope, std_has: bool) -> LocateError {
    let mut notes = vec![if scope.std {
        format!("std's conditional imports are in `std/{STD_IMPORTS_FILE}`")
    } else {
        format!("add it to `imports` in the package's `package.velt`: `\"{spec}\": {{ js: \"./src/…\", default: \"./src/…\" }}`")
    }];
    if std_has {
        notes.push(format!(
            "`{spec}` is the standard library's own: a `#name` resolves only in the package that defines it"
        ));
    }
    (
        format!("{} has no conditional import `{spec}`", scope.owner),
        notes,
    )
}

/// The file of the variant of `spec` that `conditions` select.
fn select(
    spec: &str,
    variants: &[Variant],
    conditions: &[String],
    scope: &Scope,
) -> Result<PathBuf, LocateError> {
    match imports::select(variants, conditions) {
        Some(v) => Ok(vpm::relpath::normalize(
            &scope.base.join(v.target.trim_start_matches("./")),
        )),
        None => {
            let build: Vec<&str> = conditions
                .iter()
                .map(String::as_str)
                .filter(|c| *c != DEFAULT)
                .collect();
            let written: Vec<&str> = variants.iter().map(|v| v.condition.as_str()).collect();
            Err((
                format!(
                    "`{spec}` of {} has no variant for this build (it has {}; the build: {})",
                    scope.owner,
                    written.join(", "),
                    build.join(", ")
                ),
                vec![format!(
                    "add a `{DEFAULT}` variant, the file every other build uses, or one for `{}`",
                    build.first().copied().unwrap_or(DEFAULT)
                )],
            ))
        }
    }
}

/// [`Loader::conditional`] without a loader (the language server's import help, a generator's
/// module): the file `spec` names for `importer` in a build for `target` (`None`: the host), or
/// `None` when the import would fail.
pub(super) fn resolve(
    spec: &str,
    importer: &Path,
    in_std: Option<&Path>,
    packages: Option<&dyn PackageResolver>,
    target: Option<&str>,
) -> Option<PathBuf> {
    let scope = match in_std {
        Some(root) => {
            let path = root.join(STD_IMPORTS_FILE);
            let map = match vpm::probe::read_to_string(&path) {
                Ok(src) => {
                    vpm::manifest::read::read_std_imports(velt_common::FileId(0), &src).ok()?
                }
                Err(_) => ImportMap::new(),
            };
            Scope {
                owner: String::new(),
                base: root.to_path_buf(),
                map,
                std: true,
            }
        }
        None => {
            let mut npm = vpm::npm::NpmResolver::default();
            if let Some(scope) = npm_scope(&mut npm, importer, packages) {
                return npm.resolve_import(&scope, spec).ok();
            }
            package_scope(spec, importer, packages).ok()?
        }
    };
    let variants = scope.map.get(spec)?;
    select(spec, variants, &build_conditions(target, None), &scope).ok()
}
