//! Classifying `import ... from "<spec>"` strings (no filesystem access).

use std::path::{Path, PathBuf};

/// What an import specifier refers to, before the file is looked up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleRef {
    /// `velt:x` — standard library module (like Node's `node:fs`); `rel` is `x`.
    Std {
        /// Path below the std root, `/`-separated, no extension.
        rel: String,
    },
    /// `./x` or `../x` — a file relative to the importing module's directory.
    Relative {
        /// The resolved file (lexically normalized, `.vlt` appended).
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
    if spec.ends_with(".vlt") {
        return Err(format!(
            "module specifier `{spec}` should not include the `.vlt` extension"
        ));
    }
    if let Some(rel) = spec.strip_prefix("std/") {
        return Err(format!(
            "standard library modules are imported as `velt:{rel}` (not `{spec}`)"
        ));
    }
    if let Some(rel) = spec.strip_prefix("velt:") {
        if !is_clean_subpath(rel) {
            return Err(format!("invalid standard library module `{spec}`"));
        }
        return Ok(ModuleRef::Std {
            rel: rel.to_string(),
        });
    }
    if spec.starts_with("./") || spec.starts_with("../") {
        if spec.ends_with('/') || spec.contains('\\') {
            return Err(format!("invalid module specifier `{spec}`"));
        }
        // Append rather than `set_extension`: `./math.test` names `math.test.vlt`.
        let file = importer_dir.join(format!("{spec}.vlt"));
        return Ok(ModuleRef::Relative {
            file: vpm::relpath::normalize(&file),
        });
    }
    if spec.starts_with('/') || spec.contains('\\') || spec.contains(':') {
        return Err(format!(
            "invalid module specifier `{spec}` (use `./relative/path`, `velt:x` or a package name)"
        ));
    }
    let (name, sub) = match spec.split_once('/') {
        Some((n, s)) => (n, Some(s.to_string())),
        None => (spec, None),
    };
    if !vpm::manifest::is_valid_package_name(name) {
        return Err(format!(
            "invalid package name `{name}` in module specifier `{spec}`"
        ));
    }
    if sub.as_deref().is_some_and(|s| !is_clean_subpath(s)) {
        return Err(format!("invalid module path in specifier `{spec}`"));
    }
    Ok(ModuleRef::Package {
        name: name.to_string(),
        sub,
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
    fn classify_specs() {
        let dir = Path::new("proj/src");
        assert_eq!(
            resolve_spec("velt:fs", dir).unwrap(),
            ModuleRef::Std { rel: "fs".into() }
        );
        assert_eq!(
            resolve_spec("velt:collections/map", dir).unwrap(),
            ModuleRef::Std {
                rel: "collections/map".into()
            }
        );
        assert_eq!(
            resolve_spec("./util", dir).unwrap(),
            ModuleRef::Relative {
                file: Path::new("proj/src").join("util.vlt")
            }
        );
        assert_eq!(
            resolve_spec("../lib/math", dir).unwrap(),
            ModuleRef::Relative {
                file: Path::new("proj").join("lib").join("math.vlt")
            }
        );
        assert_eq!(
            resolve_spec("./math.test", dir).unwrap(),
            ModuleRef::Relative {
                file: Path::new("proj/src").join("math.test.vlt")
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
            "velt:",
            "velt:../x",
            "/abs",
            "C:\\x",
            "Bad",
            "./x.vlt",
            "./",
            "pkg/../x",
        ] {
            assert!(
                resolve_spec(bad, dir).is_err(),
                "`{bad}` should be rejected"
            );
        }
    }
}
