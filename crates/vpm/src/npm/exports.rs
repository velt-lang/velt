//! A `package.json` `exports` map, read as Node reads it (PACKAGE_EXPORTS_RESOLVE in Node's ESM
//! resolution algorithm): the subpath (`"."`, `"./internals"`, a `"./*"` pattern) picks an
//! entry, and the entry's condition objects are matched in the order their keys are written.
//!
//! Node stops at the first condition that matches; here every target a matching condition
//! reaches is returned, best first, because the caller maps each target back to a TypeScript
//! source and takes the first one that has one (`types` usually comes first and names the
//! declaration file next to the source's place in `dist/`).

use super::json::Json;

/// Why an `exports` map gives no target for a subpath.
#[derive(Debug, PartialEq, Eq)]
pub enum ExportsError {
    /// The subpath is not in the map (or maps to `null`).
    NotExported,
    /// The map mixes `"."`-keys with condition names.
    Invalid,
}

/// The targets `exports` maps `subpath` (`"."` or `"./x"`) to under `conditions`, best first,
/// each a `./`-relative path inside the package.
pub fn resolve(
    exports: &Json,
    subpath: &str,
    conditions: &[&str],
) -> Result<Vec<String>, ExportsError> {
    let subpaths = match exports {
        Json::Obj(fields) if !fields.is_empty() => {
            let dotted = fields.iter().filter(|(k, _)| k.starts_with('.')).count();
            if dotted == fields.len() {
                Some(fields)
            } else if dotted == 0 {
                None
            } else {
                return Err(ExportsError::Invalid);
            }
        }
        _ => None,
    };
    let Some(fields) = subpaths else {
        // A string, an array or a condition object: what `"."` exports.
        if subpath != "." {
            return Err(ExportsError::NotExported);
        }
        return found(targets(exports, None, conditions));
    };
    if let Some((_, entry)) = fields
        .iter()
        .find(|(k, _)| k == subpath && !k.contains('*'))
    {
        return found(targets(entry, None, conditions));
    }
    // Patterns: the one with the longest part before `*` wins (Node's PATTERN_KEY_COMPARE).
    let mut best: Option<(&str, &Json, String)> = None;
    for (key, entry) in fields {
        let Some((prefix, suffix)) = key.split_once('*') else {
            continue;
        };
        if suffix.contains('*') {
            continue;
        }
        let Some(rest) = subpath.strip_prefix(prefix) else {
            continue;
        };
        if subpath.len() < key.len() - 1 {
            continue;
        }
        let Some(matched) = rest.strip_suffix(suffix) else {
            continue;
        };
        if best.as_ref().is_none_or(|(k, _, _)| pattern_cmp(key, k)) {
            best = Some((key, entry, matched.to_string()));
        }
    }
    match best {
        Some((_, entry, matched)) => found(targets(entry, Some(&matched), conditions)),
        None => Err(ExportsError::NotExported),
    }
}

fn found(targets: Vec<String>) -> Result<Vec<String>, ExportsError> {
    if targets.is_empty() {
        Err(ExportsError::NotExported)
    } else {
        Ok(targets)
    }
}

/// Whether pattern key `a` ranks before pattern key `b`: a longer part before `*`, then the
/// longer key.
fn pattern_cmp(a: &str, b: &str) -> bool {
    let base = |k: &str| k.find('*').unwrap_or(k.len());
    match base(a).cmp(&base(b)) {
        std::cmp::Ordering::Equal => a.len() > b.len(),
        o => o == std::cmp::Ordering::Greater,
    }
}

/// Every target `entry` reaches under `conditions`, in order; `matched` replaces the `*` of a
/// pattern's targets. A target must start with `./` and stay inside the package.
fn targets(entry: &Json, matched: Option<&str>, conditions: &[&str]) -> Vec<String> {
    let mut out = vec![];
    collect(entry, matched, conditions, &mut out);
    out
}

fn collect(entry: &Json, matched: Option<&str>, conditions: &[&str], out: &mut Vec<String>) {
    match entry {
        Json::Str(target) => {
            let target = match matched {
                Some(m) => target.replace('*', m),
                None => target.clone(),
            };
            if is_valid_target(&target) && !out.contains(&target) {
                out.push(target);
            }
        }
        Json::Arr(items) => {
            for item in items {
                collect(item, matched, conditions, out);
            }
        }
        Json::Obj(fields) => {
            for (key, value) in fields {
                if key == "default" || conditions.contains(&key.as_str()) {
                    // `null` under a matching condition: not exported under it (Node stops).
                    if *value == Json::Null {
                        return;
                    }
                    collect(value, matched, conditions, out);
                }
            }
        }
        _ => {}
    }
}

/// Node's rule for a target: starts with `./`, and no segment is empty, `.`, `..` or
/// `node_modules` after it.
fn is_valid_target(target: &str) -> bool {
    let Some(rest) = target.strip_prefix("./") else {
        return false;
    };
    !rest.contains('\\')
        && rest
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != ".." && s != "node_modules")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: &[&str] = &["types", "node", "import"];

    fn j(text: &str) -> Json {
        Json::parse(text).unwrap()
    }

    #[test]
    fn subpaths_and_conditions_in_written_order() {
        let exports = j(r#"{
            ".": { "types": "./dist/index.d.ts", "development": "./dist/index.js", "import": "./dist/index.js" },
            "./internals": { "import": "./dist/internals.js", "types": "./dist/internals.d.ts" },
            "./package.json": "./package.json"
        }"#);
        assert_eq!(
            resolve(&exports, ".", SERVER).unwrap(),
            ["./dist/index.d.ts", "./dist/index.js"]
        );
        assert_eq!(
            resolve(&exports, "./internals", SERVER).unwrap(),
            ["./dist/internals.js", "./dist/internals.d.ts"]
        );
        assert_eq!(
            resolve(&exports, "./missing", SERVER),
            Err(ExportsError::NotExported)
        );
    }

    #[test]
    fn server_conditions_skip_the_browser_build() {
        let exports = j(
            r#"{ ".": { "browser": "./src/dom.ts", "node": "./src/server.ts", "default": "./src/dom.ts" } }"#,
        );
        assert_eq!(
            resolve(&exports, ".", SERVER).unwrap(),
            ["./src/server.ts", "./src/dom.ts"]
        );
        let nested = j(r#"{ "node": { "import": "./a.js" }, "default": "./b.js" }"#);
        assert_eq!(resolve(&nested, ".", SERVER).unwrap(), ["./a.js", "./b.js"]);
    }

    #[test]
    fn sugar_patterns_null_and_invalid() {
        assert_eq!(
            resolve(&j(r#""./index.js""#), ".", SERVER).unwrap(),
            ["./index.js"]
        );
        assert_eq!(
            resolve(&j(r#""./index.js""#), "./x", SERVER),
            Err(ExportsError::NotExported)
        );
        let patterns = j(r#"{
            "./*": "./dist/*.js",
            "./features/*.js": "./dist/features/*.js",
            "./private/*": null
        }"#);
        assert_eq!(
            resolve(&patterns, "./util", SERVER).unwrap(),
            ["./dist/util.js"]
        );
        assert_eq!(
            resolve(&patterns, "./features/a/b.js", SERVER).unwrap(),
            ["./dist/features/a/b.js"]
        );
        assert_eq!(
            resolve(&patterns, "./private/x", SERVER),
            Err(ExportsError::NotExported)
        );
        let mixed = j(r#"{ ".": "./a.js", "import": "./b.js" }"#);
        assert_eq!(resolve(&mixed, ".", SERVER), Err(ExportsError::Invalid));
        let escaping = j(r#"{ ".": "../outside.js" }"#);
        assert_eq!(
            resolve(&escaping, ".", SERVER),
            Err(ExportsError::NotExported)
        );
    }
}
