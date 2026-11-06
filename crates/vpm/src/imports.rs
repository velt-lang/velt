//! Conditional imports, Node's `imports` field (CONTRACT: docs/internals/contracts/manifest.md):
//! a package (or the standard library, `std/imports.velt`) maps `#name` specifiers to one of
//! several files, chosen by the build's conditions.
//!
//! ```ts ignore
//! imports: {
//!   "#transport": { js: "./src/transport/js.velt", default: "./src/transport/native.velt" },
//!   "#config": "./src/config.velt",          // one file: the same as { default: … }
//! },
//! ```
//!
//! A build has a set of conditions ([`conditions`]: `native` or `wasm`, the operating system or
//! `wasi`/`js`, the condition of the host package a `--host` build is for (`crate::host`), always
//! `default`). The first variant, in the order written, whose condition the
//! build has wins; `default` matches every build and so comes last. A `#name` resolves only in
//! the package that defines it (its own modules' imports); the compiler checks that the variants
//! of one name export the same API.

use std::collections::BTreeMap;
use std::path::{Component, Path};

/// The standard library's map, in the std root: `export const imports: Imports = { … }`, its
/// variants `./`-relative to the std root.
pub const STD_IMPORTS_FILE: &str = "imports.velt";

/// The condition every build has: the variant used when no other one applies.
pub const DEFAULT: &str = "default";

/// The built-in conditions, which the target sets. `native` and `wasm` split the targets;
/// `darwin`, `linux` and `win32` (Node's `process.platform` names) refine `native`; `wasi`
/// (`wasm32-wasip1`) and `js` (`wasm32-unknown-unknown`, a module the JavaScript glue runs)
/// refine `wasm`. A variant may also name a host package's condition (`crate::host`), which only
/// a `--host` build for that package has, as Node's custom conditions.
pub const CONDITIONS: &[&str] = &[
    "native", "darwin", "linux", "win32", "wasm", "wasi", "js", DEFAULT,
];

/// One file of a `#name` entry, under one condition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    /// A name of [`CONDITIONS`], or a host package's condition.
    pub condition: String,
    /// The file, `./`-relative to the package root (to the std root for std's map), with its
    /// source extension: `./src/transport/js.velt`.
    pub target: String,
}

/// `#name` → its variants, in the order written (a plain string is one [`DEFAULT`] variant).
pub type ImportMap = BTreeMap<String, Vec<Variant>>;

/// Whether `spec` is a conditional import (`#transport`).
pub fn is_specifier(spec: &str) -> bool {
    spec.starts_with('#')
}

/// The conditions of a build for target triple `triple` and, with `--host`, the condition of its
/// host package (`host`), [`DEFAULT`] included.
pub fn conditions(triple: &str, host: Option<&str>) -> Vec<String> {
    let mut out: Vec<&str> = vec![];
    if triple.starts_with("wasm") && triple.contains("-wasi") {
        out.extend(["wasm", "wasi"]);
    } else if triple.starts_with("wasm") {
        out.extend(["wasm", "js"]);
    } else {
        out.push("native");
        if triple.contains("-darwin") || triple.contains("-apple-") {
            out.push("darwin");
        } else if triple.contains("-linux") {
            out.push("linux");
        } else if triple.contains("-windows") {
            out.push("win32");
        }
    }
    out.extend(host);
    out.push(DEFAULT);
    out.into_iter().map(str::to_string).collect()
}

/// The variant a build with `conditions` uses: the first whose condition it has.
pub fn select<'a>(variants: &'a [Variant], conditions: &[String]) -> Option<&'a Variant> {
    variants.iter().find(|v| conditions.contains(&v.condition))
}

/// One target per set of conditions a build can have: what [`target_selecting`] tries.
const REPRESENTATIVE_TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "wasm32-wasip1",
    "wasm32-unknown-unknown",
];

/// A target whose build uses the file `target` (a variant's `./`-relative path) for some `#name`
/// of `map`, when a build for `host` does not: the editor checks an open variant as the build
/// that uses it. `None` when `host` uses it, or no `#name` names it.
pub fn target_selecting(map: &ImportMap, target: &str, host: &str) -> Option<&'static str> {
    // `./src//js.velt` and `./src/./js.velt` name `src/js.velt`.
    let file = |t: &str| -> Vec<String> {
        t.split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .map(str::to_string)
            .collect()
    };
    let wanted = file(target);
    let uses = |triple: &str| {
        let conditions = conditions(triple, None);
        map.values()
            .filter_map(|variants| select(variants, &conditions))
            .any(|v| file(&v.target) == wanted)
    };
    if uses(host) {
        return None;
    }
    REPRESENTATIVE_TARGETS.iter().copied().find(|t| uses(t))
}

/// Check a key: `#` and `/`-separated segments of ASCII letters, digits, `_` and `-`.
pub(crate) fn check_key(key: &str) -> Result<(), String> {
    let ok = key.strip_prefix('#').is_some_and(|name| {
        !name.is_empty()
            && name.split('/').all(|seg| {
                !seg.is_empty()
                    && seg
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
    });
    if ok {
        Ok(())
    } else if key.contains('*') {
        Err(format!(
            "`{key}`: patterns (`#name/*`) are not supported: list each name (`#io/file`)"
        ))
    } else {
        Err(format!(
            "`{key}` is not a conditional import: write `#` and a name (`#transport`, `#io/file`)"
        ))
    }
}

/// Whether `name` has a condition's spelling: a lowercase letter, then lowercase letters, digits
/// and `-`.
fn condition_spelling(name: &str) -> bool {
    name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Check a variant's condition: a built-in one, or, when `hosts` (a package's map, not std's), a
/// host package's (any name spelled as one).
pub(crate) fn check_condition(condition: &str, hosts: bool) -> Result<(), String> {
    if CONDITIONS.contains(&condition) || (hosts && condition_spelling(condition)) {
        return Ok(());
    }
    if !hosts {
        return Err(format!(
            "unknown condition `{condition}`: the standard library names only the built-in ones \
             ({})",
            CONDITIONS.join(", ")
        ));
    }
    Err(format!(
        "`{condition}` is not a condition: the built-in ones are {}, and a host package's is \
         lowercase letters, digits and `-`",
        CONDITIONS.join(", ")
    ))
}

/// Check the condition a host package gives its builds (`crate::host`): spelled as one, and not
/// a built-in one, which the target sets.
pub(crate) fn check_host_condition(condition: &str) -> Result<(), String> {
    if CONDITIONS.contains(&condition) {
        return Err(format!(
            "`{condition}` is a built-in condition, which the target sets: name the host's own"
        ));
    }
    if !condition_spelling(condition) {
        return Err(format!(
            "`{condition}`: a host's condition is a lowercase letter, then lowercase letters, \
             digits and `-`"
        ));
    }
    Ok(())
}

/// Check a variant's file: `./`-relative, `/`-separated, inside the package (below `src/` when
/// `src` is set, as a package's modules are), naming a source file by its extension.
pub(crate) fn check_target(target: &str, src: bool) -> Result<(), String> {
    let Some(rel) = target.strip_prefix("./") else {
        return Err(format!(
            "`{target}`: a variant is a `./`-relative path (`./src/x.velt`)"
        ));
    };
    let inside = !rel.contains('\\')
        && Path::new(rel)
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
    if !inside {
        return Err(format!(
            "`{target}`: a variant must be a `/`-separated path inside the package"
        ));
    }
    if src && !rel.starts_with("src/") {
        return Err(format!(
            "`{target}`: a variant is one of the package's modules, below `./src/`"
        ));
    }
    if !crate::sources::is_source_name(rel) {
        return Err(format!(
            "`{target}`: name the file with its extension (`.velt`, `.ts` or `.tsx`)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variants(list: &[(&str, &str)]) -> Vec<Variant> {
        list.iter()
            .map(|(c, t)| Variant {
                condition: c.to_string(),
                target: t.to_string(),
            })
            .collect()
    }

    #[test]
    fn conditions_per_target() {
        assert_eq!(
            conditions("aarch64-apple-darwin", None),
            ["native", "darwin", "default"]
        );
        assert_eq!(
            conditions("x86_64-unknown-linux-gnu", None),
            ["native", "linux", "default"]
        );
        assert_eq!(
            conditions("x86_64-pc-windows-msvc", None),
            ["native", "win32", "default"]
        );
        assert_eq!(
            conditions("wasm32-wasip1", None),
            ["wasm", "wasi", "default"]
        );
        assert_eq!(
            conditions("wasm32-unknown-unknown", None),
            ["wasm", "js", "default"]
        );
        // A host package's condition comes after the target's.
        assert_eq!(
            conditions("wasm32-unknown-unknown", Some("edge")),
            ["wasm", "js", "edge", "default"]
        );
        for triple in [
            "aarch64-apple-darwin",
            "wasm32-wasip1",
            "wasm32-unknown-unknown",
        ] {
            assert!(conditions(triple, None)
                .iter()
                .all(|c| CONDITIONS.contains(&c.as_str())));
        }
        for ok in ["edge", "my-host2"] {
            assert!(
                check_condition(ok, true).is_ok() && check_host_condition(ok).is_ok(),
                "{ok}"
            );
        }
        for bad in ["Edge", "2x", "", "a_b", "a.b"] {
            assert!(
                check_condition(bad, true).is_err() && check_host_condition(bad).is_err(),
                "{bad}"
            );
        }
        let err = check_host_condition("js").unwrap_err();
        assert!(err.contains("built-in condition"), "{err}");
        assert!(check_condition("js", true).is_ok());
    }

    #[test]
    fn the_first_variant_the_build_has_wins() {
        let v = variants(&[
            ("js", "./src/js.velt"),
            ("wasm", "./src/wasm.velt"),
            ("default", "./src/native.velt"),
        ]);
        let pick = |triple: &str| select(&v, &conditions(triple, None)).map(|v| v.target.as_str());
        // A host's variant comes before the target's when written first.
        let edge = variants(&[("edge", "./src/w.velt"), ("js", "./src/js.velt")]);
        let on = |host| select(&edge, &conditions("wasm32-unknown-unknown", host));
        assert_eq!(
            on(Some("edge")).map(|v| v.target.as_str()),
            Some("./src/w.velt")
        );
        assert_eq!(on(None).map(|v| v.target.as_str()), Some("./src/js.velt"));
        assert_eq!(pick("wasm32-unknown-unknown"), Some("./src/js.velt"));
        assert_eq!(pick("wasm32-wasip1"), Some("./src/wasm.velt"));
        assert_eq!(pick("x86_64-unknown-linux-gnu"), Some("./src/native.velt"));
        // Without `default`, a build no variant names has none.
        let no_default = variants(&[("js", "./src/js.velt")]);
        assert_eq!(
            select(&no_default, &conditions("wasm32-wasip1", None)),
            None
        );
    }

    #[test]
    fn a_variant_the_host_does_not_use_gets_a_target_that_does() {
        let map = ImportMap::from([(
            "#io".to_string(),
            variants(&[
                ("js", "./src/io/js.velt"),
                ("win32", "./src/io/win.velt"),
                ("default", "./src/io/native.velt"),
            ]),
        )]);
        let host = "aarch64-apple-darwin";
        assert_eq!(
            target_selecting(&map, "./src/io/js.velt", host),
            Some("wasm32-unknown-unknown")
        );
        assert_eq!(
            target_selecting(&map, "./src/io/win.velt", host),
            Some("x86_64-pc-windows-msvc")
        );
        assert_eq!(target_selecting(&map, "./src/io/native.velt", host), None);
        assert_eq!(target_selecting(&map, "./src/other.velt", host), None);
        assert_eq!(
            target_selecting(&map, "./src//io/./js.velt", host),
            Some("wasm32-unknown-unknown")
        );
        assert_eq!(
            conditions("aarch64-apple-macos", None),
            ["native", "darwin", "default"]
        );
        // Any WebAssembly triple is `wasm`, never `native`.
        assert_eq!(
            conditions("wasm64-unknown-unknown", None),
            ["wasm", "js", "default"]
        );
    }

    #[test]
    fn keys_conditions_and_targets() {
        for ok in ["#transport", "#io/file", "#a-b_c1"] {
            assert!(check_key(ok).is_ok(), "{ok}");
        }
        for bad in [
            "transport",
            "#",
            "#/x",
            "#x/",
            "#a b",
            "#x//y",
            "##x",
            "#a.b",
        ] {
            assert!(check_key(bad).is_err(), "{bad}");
        }
        assert!(check_key("#io/*").unwrap_err().contains("patterns"));
        assert!(check_condition("js", false).is_ok());
        assert!(check_condition("edge", false)
            .unwrap_err()
            .contains("only the built-in"));
        let err = check_condition("No_de", true).unwrap_err();
        assert!(err.contains("native, darwin"), "{err}");
        assert!(check_target("./src/a/b.velt", true).is_ok());
        assert!(check_target("./net/x.velt", false).is_ok());
        for (bad, why) in [
            ("src/a.velt", "`./`-relative"),
            ("./src/../../x.velt", "inside the package"),
            ("./src\\a.velt", "inside the package"),
            ("./lib/a.velt", "below `./src/`"),
            ("./src/a", "extension"),
            ("./src/a.d.ts", "extension"),
        ] {
            let err = check_target(bad, true).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
        }
    }
}
