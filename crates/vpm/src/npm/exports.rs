//! A `package.json` `exports` map, read as Node reads it (PACKAGE_EXPORTS_RESOLVE in Node's ESM
//! resolution algorithm): the subpath (`"."`, `"./internals"`, a `"./*"` pattern) picks an
//! entry, and the entry's condition objects are matched in the order their keys are written.
//!
//! Node stops at the first condition that matches, and so does this, with one addition: the
//! targets of `types` and `velt` (wherever written) are returned too, as hints, because the
//! caller maps each target back to a TypeScript source and takes the first one that has one.
//! They are ordered `velt` first (the package's own choice for Velt), then the target Node
//! loads, then `types` (the declaration file usually sits next to the source's place in
//! `dist/`, so it finds the source when the JavaScript bundle has another name; but where the
//! runtime target has a source of its own, that one is what Node runs). A later condition
//! (`default` after `node`) is never taken in place of the first matching one: that would
//! compile another build than Node runs.

use super::json::Json;

/// Why an `exports` map gives no target for a subpath.
#[derive(Debug, PartialEq, Eq)]
pub enum ExportsError {
    /// The subpath is not in the map (or maps to `null`).
    NotExported,
    /// The map mixes `"."`-keys with condition names.
    Invalid,
}

/// What `exports` maps a subpath to under the build's conditions.
#[derive(Debug, PartialEq, Eq)]
pub struct Targets {
    /// Every target to try, best first (`velt` hints, the runtime target, then `types`
    /// hints; written order within each); each a `./`-relative path inside the package.
    pub all: Vec<String>,
    /// The target Node loads and the condition that chose it (empty for a target without
    /// one; `None` when a `velt` target stands for it).
    pub runtime: Option<(String, String)>,
}

/// The targets `exports` maps `subpath` (`"."` or `"./x"`) to under `conditions`, best first,
/// each a `./`-relative path inside the package.
pub fn resolve(
    exports: &Json,
    subpath: &str,
    conditions: &[&str],
) -> Result<Vec<String>, ExportsError> {
    resolve_targets(exports, subpath, conditions).map(|t| t.all)
}

/// [`resolve`], with the target Node loads.
pub fn resolve_targets(
    exports: &Json,
    subpath: &str,
    conditions: &[&str],
) -> Result<Targets, ExportsError> {
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

fn found(targets: Targets) -> Result<Targets, ExportsError> {
    if targets.all.is_empty() {
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

/// The conditions that are source hints: their targets are tried, but they don't stand in
/// for the condition Node takes.
const HINTS: &[&str] = &["velt", "types"];

/// The targets `entry` gives under `conditions` (see the module docs); `matched` replaces the
/// `*` of a pattern's targets. A target must start with `./` and stay inside the package.
fn targets(entry: &Json, matched: Option<&str>, conditions: &[&str]) -> Targets {
    let mut walk = Walk {
        matched,
        conditions,
        out: vec![],
        runtime: None,
        path: vec![],
    };
    if walk.entry(entry) != Outcome::Taken {
        // Nothing Node would load (only `types`, or `null`): not exported.
        return Targets {
            all: vec![],
            runtime: None,
        };
    }
    // Stable: written order within each rank.
    walk.out.sort_by_key(|(_, rank)| *rank);
    Targets {
        all: walk.out.into_iter().map(|(t, _)| t).collect(),
        runtime: walk.runtime,
    }
}

/// Where a target ranks among those to try: `velt` hints, then the runtime target, then
/// `types` hints.
fn rank(path: &[String]) -> u8 {
    if path.iter().any(|c| c == "velt") {
        0
    } else if path.iter().any(|c| HINTS.contains(&c.as_str())) {
        2
    } else {
        1
    }
}

/// How an entry turned out ([`Walk::entry`]).
#[derive(PartialEq)]
enum Outcome {
    /// A target was chosen.
    Taken,
    /// No condition matched (Node goes on with the next one).
    NoMatch,
    /// A matching condition maps to `null`: not exported (Node stops).
    Null,
}

struct Walk<'a> {
    matched: Option<&'a str>,
    conditions: &'a [&'a str],
    /// The targets found and their [`rank`].
    out: Vec<(String, u8)>,
    runtime: Option<(String, String)>,
    /// The conditions leading to the entry being walked.
    path: Vec<String>,
}

impl Walk<'_> {
    fn entry(&mut self, entry: &Json) -> Outcome {
        match entry {
            Json::Str(target) => {
                let target = match self.matched {
                    Some(m) => target.replace('*', m),
                    None => target.clone(),
                };
                if !is_valid_target(&target) {
                    return Outcome::NoMatch;
                }
                let hint = self.path.iter().any(|c| HINTS.contains(&c.as_str()));
                if !hint && self.runtime.is_none() {
                    let cond = self.path.last().cloned().unwrap_or_default();
                    self.runtime = Some((cond, target.clone()));
                }
                let rank = rank(&self.path);
                match self.out.iter_mut().find(|(t, _)| *t == target) {
                    Some((_, r)) => *r = (*r).min(rank),
                    None => self.out.push((target, rank)),
                }
                Outcome::Taken
            }
            Json::Arr(items) => {
                for item in items {
                    let mark = self.out.len();
                    match self.entry(item) {
                        Outcome::Taken => return Outcome::Taken,
                        _ => self.out.truncate(mark),
                    }
                }
                Outcome::NoMatch
            }
            Json::Obj(fields) => {
                let mut outcome = Outcome::NoMatch;
                for (key, value) in fields {
                    let hint = HINTS.contains(&key.as_str());
                    let applies =
                        hint || key == "default" || self.conditions.contains(&key.as_str());
                    // After the condition Node takes, only hints are read.
                    if !applies || (outcome != Outcome::NoMatch && !hint) {
                        continue;
                    }
                    if *value == Json::Null {
                        if hint {
                            continue;
                        }
                        return Outcome::Null;
                    }
                    let mark = self.out.len();
                    self.path.push(key.clone());
                    let got = self.entry(value);
                    self.path.pop();
                    match got {
                        Outcome::Taken if hint => {
                            // `velt` is a real condition: it stands for a runtime target.
                            if key == "velt" && outcome == Outcome::NoMatch {
                                outcome = Outcome::Taken;
                            }
                        }
                        Outcome::Taken => outcome = Outcome::Taken,
                        Outcome::NoMatch => self.out.truncate(mark),
                        Outcome::Null if hint => self.out.truncate(mark),
                        Outcome::Null => return Outcome::Null,
                    }
                }
                outcome
            }
            _ => Outcome::NoMatch,
        }
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
            ["./dist/index.js", "./dist/index.d.ts"]
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
        assert_eq!(resolve(&exports, ".", SERVER).unwrap(), ["./src/server.ts"]);
        let nested = j(r#"{ "node": { "import": "./a.js" }, "default": "./b.js" }"#);
        assert_eq!(resolve(&nested, ".", SERVER).unwrap(), ["./a.js"]);
        // A nested object without a match goes on with the next condition, as in Node.
        let nested = j(r#"{ "node": { "require": "./a.cjs" }, "default": "./b.js" }"#);
        assert_eq!(resolve(&nested, ".", SERVER).unwrap(), ["./b.js"]);
    }

    #[test]
    fn only_the_first_matching_condition_besides_hints() {
        // #1091 review: `default` after `node` is not a fallback for `node`'s target.
        let exports =
            j(r#"{ ".": { "node": "./dist/server.js", "default": "./dist/client.js" } }"#);
        let t = resolve_targets(&exports, ".", SERVER).unwrap();
        assert_eq!(t.all, ["./dist/server.js"]);
        assert_eq!(
            t.runtime,
            Some(("node".to_string(), "./dist/server.js".to_string()))
        );
        // `types` is a hint wherever written; `velt` too, and it stands for a match.
        let exports =
            j(r#"{ "import": "./dist/a.js", "default": "./dist/b.js", "types": "./dist/a.d.ts" }"#);
        assert_eq!(
            resolve(&exports, ".", SERVER).unwrap(),
            ["./dist/a.js", "./dist/a.d.ts"]
        );
        let velt = j(r#"{ "velt": "./src/velt.ts", "node": "./dist/n.js" }"#);
        let t = resolve_targets(&velt, ".", &["velt", "types", "node"]).unwrap();
        assert_eq!(t.all, ["./src/velt.ts"]);
        assert_eq!(t.runtime, None);
        // Only a hint, nothing Node loads: not exported.
        let types_only = j(r#"{ "types": "./a.d.ts", "require": "./a.cjs" }"#);
        assert_eq!(
            resolve(&types_only, ".", SERVER),
            Err(ExportsError::NotExported)
        );
        let null = j(r#"{ "types": "./a.d.ts", "node": null, "default": "./a.js" }"#);
        assert_eq!(resolve(&null, ".", SERVER), Err(ExportsError::NotExported));
    }

    #[test]
    fn the_runtime_target_before_a_types_hint() {
        // #1091 re-check: `types` written first must not pick another source than Node runs.
        let exports = j(r#"{ ".": {
            "types": "./dist/index.d.ts",
            "node": "./dist/index.node.js",
            "default": "./dist/index.js"
        } }"#);
        assert_eq!(
            resolve(&exports, ".", SERVER).unwrap(),
            ["./dist/index.node.js", "./dist/index.d.ts"]
        );
        // A `velt` hint stays first, wherever written.
        let velt = j(r#"{ "types": "./a.d.ts", "node": "./n.js", "velt": "./v.ts" }"#);
        assert_eq!(
            resolve(&velt, ".", &["velt", "types", "node"]).unwrap(),
            ["./v.ts", "./n.js", "./a.d.ts"]
        );
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
