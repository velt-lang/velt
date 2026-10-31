//! Classifying `import ... from "<spec>"` strings (no filesystem access).

use std::path::{Path, PathBuf};

use velt_common::std_modules;

/// What an import specifier refers to, before the file is looked up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleRef {
    /// `velt/x` — standard library module; `rel` is `x`. `velt` is a package name the toolchain
    /// provides, so no dependency may take it.
    Std {
        /// Path below the std root, `/`-separated, no extension.
        rel: String,
    },
    /// `./x` or `../x` — a file relative to the importing module's directory.
    Relative {
        /// The specifier's path joined to the importer's directory, lexically normalized, with
        /// its extension as written (`./util` → `<dir>/util`, `./card.tsx` → `<dir>/card.tsx`);
        /// the loader picks the file (`util.vlt`, `util.ts`, …).
        file: PathBuf,
    },
    /// Bare name — a package dependency (`name`) and optional sub-module path.
    Package {
        /// Dependency name.
        name: String,
        /// Module inside the package's `src/` (`None` → `src/lib.vlt`).
        sub: Option<String>,
    },
}

/// Classify an import specifier. `importer_dir` is the directory of the importing file.
pub fn resolve_spec(spec: &str, importer_dir: &Path) -> Result<ModuleRef, String> {
    if spec.is_empty() {
        return Err("empty module specifier".into());
    }
    if let Some(rel) = spec.strip_prefix(std_modules::CANONICAL_PREFIX) {
        return Err(format!(
            "standard library modules are imported as `velt/{rel}` (not `{spec}`)"
        ));
    }
    if spec == "std" || spec == "velt" {
        return Err(format!(
            "`{spec}` names no module: standard library modules are imported as `velt/<module>` \
             (`velt/fs`)"
        ));
    }
    if let Some(rel) = spec.strip_prefix("velt:") {
        let list = super::moved::STD_DOCS_URL;
        return Err(match super::moved::now(rel, spec) {
            // A package's sentence links its page; the others, the std docs.
            Some(now) if now.contains("https://") => {
                format!("{now}; `velt:` specifiers were removed")
            }
            Some(now) => format!("{now} (`velt:` specifiers were removed; the modules: {list})"),
            None => format!(
                "`velt:` specifiers were removed: standard library modules are imported as \
                 `velt/<module>` (`velt/fs`; the modules: {list}), not `velt:{rel}`"
            ),
        });
    }
    if let Some(rel) = spec.strip_prefix(std_modules::SPECIFIER_PREFIX) {
        extension_check(spec)?;
        if !is_std_subpath(rel) {
            return Err(format!("invalid standard library module `{spec}`"));
        }
        return Ok(ModuleRef::Std {
            rel: rel.to_string(),
        });
    }
    // Vitest's module is `velt/test`, which has its API (`describe`, `it`, `expect`), so a test
    // file written for Vitest runs as it is.
    if spec == "vitest" {
        return Ok(ModuleRef::Std {
            rel: velt_common::std_rel!(test).into(),
        });
    }
    if spec.starts_with("./") || spec.starts_with("../") {
        if spec.ends_with('/') || spec.contains('\\') {
            return Err(format!("invalid module specifier `{spec}`"));
        }
        return Ok(ModuleRef::Relative {
            file: vpm::relpath::normalize(&importer_dir.join(spec)),
        });
    }
    if spec.starts_with('/') || spec.contains('\\') || spec.contains(':') {
        return Err(format!(
            "invalid module specifier `{spec}` (use `./relative/path`, `velt/x` or a package name)"
        ));
    }
    // A scoped name (`@scope/name`) spans two segments, as in npm.
    let name_end = match spec.strip_prefix('@').and_then(|rest| rest.find('/')) {
        Some(slash) => {
            let after = &spec[slash + 2..];
            slash + 2 + after.find('/').unwrap_or(after.len())
        }
        None => spec.find('/').unwrap_or(spec.len()),
    };
    let name = &spec[..name_end];
    let sub = spec
        .get(name_end..)
        .and_then(|rest| rest.strip_prefix('/'))
        .map(str::to_string);
    if !vpm::manifest::is_valid_dependency_name(name) {
        return Err(format!(
            "invalid package name `{name}` in module specifier `{spec}`"
        ));
    }
    extension_check(spec)?;
    if sub.as_deref().is_some_and(|s| !is_clean_subpath(s)) {
        return Err(format!("invalid module path in specifier `{spec}`"));
    }
    Ok(ModuleRef::Package {
        name: name.to_string(),
        sub,
    })
}

/// Standard library and package specifiers name modules without an extension (`velt/fs`, not
/// `velt/fs.vlt`); only relative imports may name one.
fn extension_check(spec: &str) -> Result<(), String> {
    match vpm::sources::SOURCE_EXTENSIONS
        .iter()
        .find(|ext| spec.ends_with(&format!(".{ext}")))
    {
        Some(ext) => Err(format!(
            "module specifier `{spec}` should not include the `.{ext}` extension (only relative imports may name one)"
        )),
        None => Ok(()),
    }
}

/// A standard library module path: `/`-separated segments of `[a-z0-9_-]`. Nothing else may
/// reach `std_root.join(rel)`: on Windows `C:/x` or `..\x` would leave the std root.
fn is_std_subpath(rel: &str) -> bool {
    !rel.is_empty()
        && rel.split('/').all(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        })
}

/// Non-empty `/`-separated segments without `.`/`..`.
fn is_clean_subpath(rel: &str) -> bool {
    !rel.is_empty()
        && rel
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_specs_stay_inside_the_std_root() {
        let dir = Path::new("proj");
        for spec in [
            "velt/C:/Users/me/evil",
            "velt/..\\..\\proj\\evil",
            "velt/..\\prelude\\evil",
            "velt/a\\b",
            "velt//etc/x",
            "velt/../x",
            "velt/a/./b",
            "velt/Fs",
            "velt/a.b",
            "velt/a b",
            "velt/",
        ] {
            let err = resolve_spec(spec, dir).unwrap_err();
            assert!(
                err.contains("invalid standard library module"),
                "{spec}: {err}"
            );
        }
        for spec in ["velt/jsx/jsx-runtime", "velt/collections", "velt/fs2"] {
            assert!(resolve_spec(spec, dir).is_ok(), "{spec}");
        }
        for spec in ["std", "velt"] {
            let err = resolve_spec(spec, dir).unwrap_err();
            assert!(err.contains("`velt/<module>`"), "{err}");
        }
    }

    #[test]
    fn velt_colon_specifiers_are_removed() {
        let err = resolve_spec("velt:fs", Path::new(".")).unwrap_err();
        assert!(err.contains("`velt:` specifiers were removed"), "{err}");
        assert!(err.starts_with("`velt:fs` is now `velt/fs`"), "{err}");
        // Per module: where it is now.
        let err = resolve_spec("velt:json", Path::new(".")).unwrap_err();
        assert!(
            err.starts_with("`velt:json` is now `velt/encoding/json`"),
            "{err}"
        );
        let err = resolve_spec("velt:sqlite", Path::new(".")).unwrap_err();
        assert!(err.contains("the `sqlite` package"), "{err}");
        // Not a module of the old layout: the general form.
        let err = resolve_spec("velt:nothing", Path::new(".")).unwrap_err();
        assert!(err.contains("not `velt:nothing`"), "{err}");
        let err = resolve_spec("std/fs", Path::new(".")).unwrap_err();
        assert!(err.contains("`velt/fs`"), "{err}");
    }

    #[test]
    fn classify_specs() {
        let dir = Path::new("proj/src");
        assert_eq!(
            resolve_spec("velt/fs", dir).unwrap(),
            ModuleRef::Std { rel: "fs".into() }
        );
        assert_eq!(
            resolve_spec("vitest", dir).unwrap(),
            ModuleRef::Std { rel: "test".into() }
        );
        assert_eq!(
            resolve_spec("velt/net/http", dir).unwrap(),
            ModuleRef::Std {
                rel: "net/http".into()
            }
        );
        assert_eq!(
            resolve_spec("./util", dir).unwrap(),
            ModuleRef::Relative {
                file: Path::new("proj/src").join("util")
            }
        );
        assert_eq!(
            resolve_spec("../lib/math", dir).unwrap(),
            ModuleRef::Relative {
                file: Path::new("proj").join("lib").join("math")
            }
        );
        assert_eq!(
            resolve_spec("./math.test", dir).unwrap(),
            ModuleRef::Relative {
                file: Path::new("proj/src").join("math.test")
            }
        );
        assert_eq!(
            resolve_spec("./card.tsx", dir).unwrap(),
            ModuleRef::Relative {
                file: Path::new("proj/src").join("card.tsx")
            }
        );
        assert_eq!(
            resolve_spec("json", dir).unwrap(),
            ModuleRef::Package {
                name: "json".into(),
                sub: None
            }
        );
        assert_eq!(
            resolve_spec("@sigx/reactivity/internals", dir).unwrap(),
            ModuleRef::Package {
                name: "@sigx/reactivity".into(),
                sub: Some("internals".into())
            }
        );
        assert_eq!(
            resolve_spec("@sigx/reactivity", dir).unwrap(),
            ModuleRef::Package {
                name: "@sigx/reactivity".into(),
                sub: None
            }
        );
        assert_eq!(
            resolve_spec("http/client", dir).unwrap(),
            ModuleRef::Package {
                name: "http".into(),
                sub: Some("client".into())
            }
        );
    }

    #[test]
    fn bad_specs() {
        let dir = Path::new(".");
        for bad in [
            "",
            "std/fs",
            "velt",
            "velt:fs",
            "velt/",
            "velt/../x",
            "/abs",
            "C:\\x",
            "Bad",
            "./",
            "pkg/../x",
            "pkg/x.vlt",
            "pkg/x.ts",
            "@sigx",
            "@sigx/",
            "@Sigx/x",
        ] {
            assert!(
                resolve_spec(bad, dir).is_err(),
                "`{bad}` should be rejected"
            );
        }
        let err = resolve_spec("velt/fs.vlt", dir).unwrap_err();
        assert!(
            err.contains("should not include the `.vlt` extension"),
            "{err}"
        );
    }
}
