//! npm packages compiled from their TypeScript sources (CONTRACT:
//! docs/internals/contracts/manifest.md "npm packages").
//!
//! A bare import specifier that no `paths` alias and no Velt dependency claims
//! (`@sigx/reactivity`, `@sigx/reactivity/internals`, `sigx`) names an npm package. It is found
//! as Node finds it, and its `package.json` says which file the specifier names; that file is
//! normally build output (`dist/index.js`, `dist/index.d.ts`), so it is mapped back to the
//! TypeScript source it was built from, as TypeScript's project references and Vite's source
//! aliases do. The package's sources then compile like the program's own.
//!
//! - **Finding the package** ([`NpmResolver::package_dir`]): `node_modules/<name>` in the
//!   importer's directory and each one above it, at its real path when it is a symlink (as
//!   Node, which does not preserve symlinks: pnpm's `.pnpm` layout finds a package's own
//!   dependencies next to it);
//!   then the package itself (an import of its own name); then the packages of the workspace
//!   around the importer ([`workspace`]: `pnpm-workspace.yaml`, or `workspaces` in the root
//!   `package.json`), so a workspace needs no `pnpm install` first.
//! - **The entry** ([`exports`]): `exports` when present (a subpath not in it is an error, as in
//!   Node), under the conditions [`SERVER_CONDITIONS`]; else for the package itself `types` (or
//!   `typings`), then `main`, then `index`; a subpath without `exports` names the file at that
//!   path. Of `exports`, the targets of `types` and `velt` are source hints; besides them only
//!   the first other matching condition counts, as in Node (never a later one, such as
//!   `default` after `node`, which would compile another build than Node runs).
//! - **Back to the source** ([`source_candidates`]): a target that is a source file is taken as
//!   is. Otherwise its extension (`.d.ts`, `.js`, `.mjs`, …) is dropped and the stem is looked
//!   for as `.ts`/`.tsx` (or `/index.ts`/`.tsx`): first below `rootDir` where the package's
//!   `tsconfig.json` puts its `outDir` (or `declarationDir`), then with a leading `dist/`,
//!   `lib/`, `build/`, `out/` or `types/` replaced by `src/`, then in place. The targets are
//!   tried in order (a `velt` hint, Node's target, then a `types` hint); the first with a
//!   source wins, and without one the error names the target Node loads.

pub mod exports;
pub mod json;
pub mod workspace;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use json::Json;

/// `dir` with its symlinks resolved when it is one (pnpm links `node_modules/a` to
/// `node_modules/.pnpm/a@1.0.0/node_modules/a`): Node loads a package from its real path, so its
/// own dependencies are found next to it, in the `.pnpm` directory.
fn real_dir(dir: PathBuf) -> PathBuf {
    let link = std::fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_symlink());
    match std::fs::canonicalize(&dir) {
        Ok(real) if link => crate::relpath::plain(&real),
        _ => dir,
    }
}

/// The `exports` conditions a build matches, as Vite's server build does (`node`, `import`,
/// `module`, `default`), plus `types` (whose targets are declaration files, which map back to
/// the sources most reliably) and `velt` (a package can point Velt at a file of its own). Not
/// `browser`, `require`, `development` or `production`: a package that maps a condition to a
/// DOM-only build is compiled from its server sources.
pub const SERVER_CONDITIONS: &[&str] = &["velt", "types", "node", "import", "module"];

/// Output directories whose files map back to `src/` when no `tsconfig.json` says otherwise.
const OUT_DIRS: &[&str] = &["dist", "lib", "build", "out", "types"];

/// Extensions of build output and declaration files, longest first.
const OUTPUT_EXTENSIONS: &[&str] = &[
    ".d.ts", ".d.mts", ".d.cts", ".d.tsx", ".js", ".mjs", ".cjs", ".jsx", ".mts", ".cts",
];

/// Why an npm package's module could not be resolved: the message and notes.
pub type NpmError = (String, Vec<String>);

/// Resolves bare specifiers to npm packages' source files; caches what it reads.
#[derive(Default)]
pub struct NpmResolver {
    /// `package.json` by directory (`None`: none, or unreadable).
    manifests: HashMap<PathBuf, Option<Json>>,
    /// Workspace root → its packages by name; a directory without a workspace maps to `None`.
    workspaces: HashMap<PathBuf, Option<Workspace>>,
    /// The most directories a workspace walk lists (`None`: [`workspace::MAX_DIRS`]).
    max_dirs: Option<usize>,
}

/// A workspace's packages by name, and the pattern whose walk was cut short, if one was.
struct Workspace {
    by_name: HashMap<String, PathBuf>,
    cut: Option<String>,
}

/// A bare specifier split into package name and subpath (`@a/b/c` → (`@a/b`, `./c`)).
pub fn split_specifier(spec: &str) -> Option<(&str, String)> {
    if spec.is_empty()
        || spec.starts_with('.')
        || spec.starts_with('/')
        || spec.contains('\\')
        || spec.contains(':')
    {
        return None;
    }
    let name_end = if spec.starts_with('@') {
        let slash = spec.find('/')?;
        let rest = &spec[slash + 1..];
        slash + 1 + rest.find('/').unwrap_or(rest.len())
    } else {
        spec.find('/').unwrap_or(spec.len())
    };
    let name = &spec[..name_end];
    let bare = name
        .strip_prefix('@')
        .map_or(name, |n| n.split('/').nth(1).unwrap_or(""));
    if bare.is_empty() || bare.starts_with('.') || bare.starts_with('_') || name.ends_with('/') {
        return None;
    }
    let sub = &spec[name_end..];
    if sub == "/"
        || sub
            .split('/')
            .skip(1)
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return None;
    }
    Some((name, format!(".{sub}")))
}

impl NpmResolver {
    /// The source file that bare specifier `spec`, imported by file `importer`, names:
    /// `None` when no npm package of that name is found (the caller reports its own error),
    /// `Some(Err)` when the package is found but does not give a source for `spec`.
    pub fn resolve(&mut self, importer: &Path, spec: &str) -> Option<Result<PathBuf, NpmError>> {
        let (name, subpath) = split_specifier(spec)?;
        let dir = self.package_dir(importer, name)?;
        Some(self.module_in(&dir, name, &subpath, spec))
    }

    /// The directory of npm package `name` as seen from `importer` (see the module docs).
    pub fn package_dir(&mut self, importer: &Path, name: &str) -> Option<PathBuf> {
        let start = crate::relpath::absolute(importer.parent()?);
        for dir in start.ancestors() {
            if dir.file_name().is_some_and(|n| n == "node_modules") {
                continue;
            }
            let candidate = dir.join("node_modules").join(name);
            if candidate.join("package.json").is_file() {
                return Some(real_dir(candidate));
            }
        }
        for dir in start.ancestors() {
            if self.manifest_name(dir).as_deref() == Some(name) {
                return Some(dir.to_path_buf());
            }
        }
        for dir in start.ancestors() {
            if let Some(found) = self.workspace(dir) {
                return found.by_name.get(name).cloned();
            }
        }
        None
    }

    /// Why the workspace around `importer` may lack a package: a note naming its root and the
    /// pattern whose walk stopped at [`workspace::MAX_DIRS`] directories, if one did. For the
    /// error of a specifier [`NpmResolver::resolve`] found no package for.
    pub fn workspace_note(&mut self, importer: &Path) -> Option<String> {
        let start = crate::relpath::absolute(importer.parent()?);
        let max = self.max_dirs.unwrap_or(workspace::MAX_DIRS);
        for dir in start.ancestors() {
            if let Some(found) = self.workspace(dir) {
                let pattern = found.cut.as_ref()?;
                return Some(format!(
                    "the workspace at `{}` was not searched in full: its pattern `{pattern}` reaches more than {} directories; narrow it to the package directories",
                    dir.display(),
                    max
                ));
            }
        }
        None
    }

    /// The `package.json` of `dir`, parsed (cached).
    fn manifest(&mut self, dir: &Path) -> Option<&Json> {
        self.manifests
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
                Json::parse(&text).ok()
            })
            .as_ref()
    }

    fn manifest_name(&mut self, dir: &Path) -> Option<String> {
        self.manifest(dir)?
            .get("name")?
            .as_str()
            .map(str::to_string)
    }

    /// The packages of the workspace rooted at `dir` by name, if `dir` is a workspace root.
    fn workspace(&mut self, dir: &Path) -> Option<&Workspace> {
        if !self.workspaces.contains_key(dir) {
            let max = self.max_dirs.unwrap_or(workspace::MAX_DIRS);
            let found = workspace::patterns(dir).map(|patterns| {
                let listing = workspace::package_dirs_limited(dir, &patterns, max);
                let mut by_name = HashMap::new();
                for pkg in listing.dirs {
                    if let Some(name) = self.manifest_name(&pkg) {
                        by_name.entry(name).or_insert(pkg);
                    }
                }
                Workspace {
                    by_name,
                    cut: listing.cut,
                }
            });
            self.workspaces.insert(dir.to_path_buf(), found);
        }
        self.workspaces.get(dir)?.as_ref()
    }

    /// The source file of `subpath` (`"."`, `"./x"`) of package `name` in `dir`.
    fn module_in(
        &mut self,
        dir: &Path,
        name: &str,
        subpath: &str,
        spec: &str,
    ) -> Result<PathBuf, NpmError> {
        let shown = dir.join("package.json").display().to_string();
        let manifest = self
            .manifest(dir)
            .cloned()
            .ok_or_else(|| (format!("cannot read `{shown}` (package `{name}`)"), vec![]))?;
        let mut runtime = None;
        let targets = match manifest.get("exports") {
            Some(map) => exports::resolve_targets(map, subpath, SERVER_CONDITIONS).map(|t| {
                runtime = t.runtime;
                t.all
            }).map_err(|e| match e {
                exports::ExportsError::NotExported => (
                    format!("package `{name}` does not export `{subpath}` (`exports` in its package.json)"),
                    vec![format!("the conditions Velt matches are {}", conditions_list())],
                ),
                exports::ExportsError::Invalid => (
                    format!("package `{name}` has an invalid `exports` map: its keys mix subpaths (`.`, `./x`) and conditions"),
                    vec![],
                ),
            })?,
            None if subpath == "." => {
                let mut t: Vec<String> = ["types", "typings", "main"]
                    .iter()
                    .filter_map(|k| manifest.get(k)?.as_str())
                    .map(|s| format!("./{}", s.trim_start_matches("./")))
                    .collect();
                t.push("./index".to_string());
                t
            }
            None => vec![subpath.to_string()],
        };
        let out_dirs = self.ts_out_dirs(dir);
        let mut tried = vec![];
        for target in &targets {
            for candidate in source_candidates(target, &out_dirs) {
                let file = dir.join(&candidate);
                if file.is_file() {
                    return Ok(file);
                }
                if !tried.contains(&candidate) {
                    tried.push(candidate);
                }
            }
        }
        let note = format!(
            "looked for {}",
            tried
                .iter()
                .take(6)
                .map(|t| format!("`{t}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut notes = vec![note];
        let shown = match &runtime {
            Some((cond, target)) if !cond.is_empty() => {
                notes.push(format!(
                    "Node loads `{}` (condition `{cond}`); Velt compiles the source of that build, not of a later condition's",
                    target.trim_start_matches("./")
                ));
                target.as_str()
            }
            Some((_, target)) => target.as_str(),
            None => targets[0].as_str(),
        };
        notes.push("Velt compiles npm packages from their `.ts` sources; map the specifier to one with `paths` in package.vlt".to_string());
        Err((
            format!(
                "package `{name}` maps `{spec}` to `{}`, which has no TypeScript source in the package",
                shown.trim_start_matches("./")
            ),
            notes,
        ))
    }

    /// `(outDir, rootDir)` pairs of the package's `tsconfig.json` (`outDir` and
    /// `declarationDir`, each with `rootDir`, default `src`), `/`-separated, without `./`.
    fn ts_out_dirs(&mut self, dir: &Path) -> Vec<(String, String)> {
        let Ok(text) = std::fs::read_to_string(dir.join("tsconfig.json")) else {
            return vec![];
        };
        let Ok(json) = Json::parse_jsonc(&text) else {
            return vec![];
        };
        let Some(opts) = json.get("compilerOptions") else {
            return vec![];
        };
        let clean = |s: &str| s.trim_start_matches("./").trim_end_matches('/').to_string();
        let root = opts
            .get("rootDir")
            .and_then(Json::as_str)
            .map_or_else(|| "src".to_string(), clean);
        ["declarationDir", "outDir"]
            .iter()
            .filter_map(|k| opts.get(k)?.as_str())
            .map(|out| (clean(out), root.clone()))
            .filter(|(out, _)| !out.is_empty() && !out.starts_with(".."))
            .collect()
    }
}

fn conditions_list() -> String {
    let mut all: Vec<String> = SERVER_CONDITIONS.iter().map(|c| format!("`{c}`")).collect();
    all.push("`default`".into());
    all.join(", ")
}

/// The package-relative files (`/`-separated, no `./`) where export target `target` (`./x`)
/// may have its source, best first.
pub fn source_candidates(target: &str, out_dirs: &[(String, String)]) -> Vec<String> {
    let rel = target.trim_start_matches("./");
    if crate::sources::is_source_name(rel) && !rel.ends_with(".d.tsx") {
        return vec![rel.to_string()];
    }
    let stem = OUTPUT_EXTENSIONS
        .iter()
        .find_map(|ext| rel.strip_suffix(ext))
        .unwrap_or(rel);
    let mut stems: Vec<String> = vec![];
    for (out, root) in out_dirs {
        if let Some(rest) = stem
            .strip_prefix(out.as_str())
            .and_then(|r| r.strip_prefix('/'))
        {
            stems.push(format!("{root}/{rest}"));
        }
    }
    if let Some((first, rest)) = stem.split_once('/') {
        if OUT_DIRS.contains(&first) {
            stems.push(format!("src/{rest}"));
        }
    }
    stems.push(stem.to_string());
    let mut out = vec![];
    for s in stems {
        for ext in ["ts", "tsx"] {
            let file = format!("{s}.{ext}");
            if !out.contains(&file) {
                out.push(file);
            }
        }
        for ext in ["ts", "tsx"] {
            let file = format!("{s}/index.{ext}");
            if !out.contains(&file) {
                out.push(file);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
