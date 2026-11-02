//! Host modules a package ships (CONTRACT: docs/internals/contracts/manifest.md): the JavaScript
//! of the import modules its `declare host("m")` functions call, for the hosts whose glue Velt
//! writes (`velt build --host workers`).
//!
//! ```ts ignore
//! hostModules: { cloudflare: "./src/cloudflare.mjs" },
//! ```
//!
//! A module name is the string of `declare host("<name>")`. Names starting with
//! [`RUNTIME_PREFIX`] belong to the runtime's glue (`velt:worker`, `velt:fetch`), so no package
//! provides one. A file is one ES module, `./`-relative and below `./src/` (a package archive
//! carries only `src/`), whose default export makes the module's functions
//! (docs/internals/contracts/workers.md).

use std::collections::BTreeMap;
use std::path::{Component, Path};

/// Import modules whose name starts with this are the runtime glue's own.
pub const RUNTIME_PREFIX: &str = "velt:";

/// Host module name → its file, `./`-relative to the package root (`./src/m.mjs`).
pub type HostModules = BTreeMap<String, String>;

/// Check a module name: one `declare host("<name>")` can import, and not the runtime glue's.
pub(crate) fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a host module needs a name".into());
    }
    if name.chars().any(char::is_control) {
        return Err(format!(
            "`{}`: a host module name cannot hold control characters",
            name.escape_default()
        ));
    }
    if name == "velt" || name.starts_with(RUNTIME_PREFIX) {
        return Err(format!(
            "`{name}` is the runtime glue's own host module: a package cannot provide it"
        ));
    }
    Ok(())
}

/// Check a module's file: `./`-relative, `/`-separated, below `./src/`, a `.mjs` or `.js` file.
pub(crate) fn check_file(file: &str) -> Result<(), String> {
    let Some(rel) = file.strip_prefix("./") else {
        return Err(format!(
            "`{file}`: a host module is a `./`-relative path (`./src/m.mjs`)"
        ));
    };
    let inside = !rel.contains('\\')
        && Path::new(rel)
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
    if !inside || !rel.starts_with("src/") {
        return Err(format!(
            "`{file}`: a host module is a `/`-separated path below `./src/` (a package's \
             archive carries only `src/`)"
        ));
    }
    if !(rel.ends_with(".mjs") || rel.ends_with(".js")) {
        return Err(format!(
            "`{file}`: a host module is a JavaScript module, `.mjs` or `.js`"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_files() {
        for ok in ["cloudflare", "app", "my:kv", "a.b"] {
            assert!(check_name(ok).is_ok(), "{ok}");
        }
        for (bad, why) in [
            ("", "needs a name"),
            ("a\tb", "control characters"),
            ("velt", "runtime glue's own"),
            ("velt:worker", "runtime glue's own"),
        ] {
            let err = check_name(bad).unwrap_err();
            assert!(err.contains(why), "{bad:?}: {err}");
        }
        for ok in ["./src/m.mjs", "./src/js/kv.js"] {
            assert!(check_file(ok).is_ok(), "{ok}");
        }
        for (bad, why) in [
            ("src/m.mjs", "`./`-relative"),
            ("./js/m.mjs", "below `./src/`"),
            ("./src/../m.mjs", "below `./src/`"),
            ("./src\\m.mjs", "below `./src/`"),
            ("./src/m.ts", "`.mjs` or `.js`"),
        ] {
            let err = check_file(bad).unwrap_err();
            assert!(err.contains(why), "{bad}: {err}");
        }
    }
}
