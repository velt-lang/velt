//! `velt search`: the packages of a registry whose name contains a text, each with its newest
//! version that is not yanked. A remote registry answers `GET /api/v1/search?q=<text>` with the
//! JSON of [`to_json`]; a local one (and the server itself) is searched by [`search_local`].

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::locations::Locations;
use crate::registry::{parse_index, INDEX_FILE};

/// Most hits one search returns.
pub const MAX_HITS: usize = 50;

/// One package found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    /// Package name.
    pub name: String,
    /// Its newest version that is not yanked.
    pub version: String,
}

#[derive(Serialize, Deserialize)]
struct Answer {
    packages: Vec<Hit>,
}

/// Search `loc`'s registry (remote or local) for `query`.
pub fn search(loc: &Locations, query: &str) -> Result<Vec<Hit>, String> {
    match &loc.remote {
        Some(url) => crate::remote::search(url, query),
        None => search_local(&loc.registry, query),
    }
}

/// Search the registry directory `root`: names containing `query` (case-insensitive), exact
/// match first, then prefix matches, then the rest, each group by name; packages whose
/// versions are all yanked are left out.
pub fn search_local(root: &Path, query: &str) -> Result<Vec<Hit>, String> {
    let query = query.trim().to_lowercase();
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(Vec::new());
    };
    let mut hits = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !crate::manifest::is_valid_package_name(&name) || !name.contains(&query) {
            continue;
        }
        let path = entry.path().join(INDEX_FILE);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // A damaged index hides only its own package from the results.
        let Ok(index) = parse_index(&text, &path.display().to_string()) else {
            continue;
        };
        let newest = index
            .versions
            .iter()
            .filter(|v| !v.yanked)
            .map(|v| v.semver())
            .max();
        if let Some(version) = newest {
            hits.push(Hit {
                name,
                version: version.to_string(),
            });
        }
    }
    hits.sort_by_key(|h| (rank(&h.name, &query), h.name.clone()));
    hits.truncate(MAX_HITS);
    Ok(hits)
}

fn rank(name: &str, query: &str) -> u8 {
    if name == query {
        0
    } else if name.starts_with(query) {
        1
    } else {
        2
    }
}

/// The JSON answer of the search endpoint.
pub fn to_json(hits: &[Hit]) -> String {
    serde_json::to_string(&Answer {
        packages: hits.to_vec(),
    })
    .expect("ICE: search hits serialize")
}

/// Read the search endpoint's JSON answer.
pub fn from_json(text: &str) -> Result<Vec<Hit>, String> {
    serde_json::from_str::<Answer>(text)
        .map(|a| a.packages)
        .map_err(|e| format!("invalid search answer: {e}"))
}

/// `query` percent-encoded for a URL query string (everything but unreserved characters).
pub fn encode_query(query: &str) -> String {
    query
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The value of `q` in a query string, percent-decoded (`+` is a space); empty if absent.
pub fn query_param(query_string: &str) -> String {
    let raw = query_string
        .split('&')
        .find_map(|pair| pair.strip_prefix("q="))
        .unwrap_or("");
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                i += 1;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publish(root: &Path, name: &str, versions: &[(&str, bool)]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let entries: Vec<String> = versions
            .iter()
            .map(|(v, yanked)| {
                format!(
                    "{{\"version\": \"{v}\", \"checksum\": \"sha256:0\", \"yanked\": {yanked}}}"
                )
            })
            .collect();
        let text = format!("{{\"versions\": [{}]}}", entries.join(", "));
        std::fs::write(dir.join(INDEX_FILE), text).unwrap();
    }

    #[test]
    fn finds_names_ranks_and_skips_yanked() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        publish(
            root,
            "json",
            &[("1.0.0", false), ("1.2.0", false), ("2.0.0", true)],
        );
        publish(root, "json-schema", &[("0.1.0", false)]);
        publish(root, "fastjson", &[("3.0.0", false)]);
        publish(root, "gone-json", &[("1.0.0", true)]);
        publish(root, "http", &[("1.0.0", false)]);
        std::fs::create_dir_all(root.join("broken-json")).unwrap();
        std::fs::write(root.join("broken-json").join(INDEX_FILE), "[[version").unwrap();
        let hits = search_local(root, "JSON").unwrap();
        let found: Vec<(&str, &str)> = hits
            .iter()
            .map(|h| (h.name.as_str(), h.version.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("json", "1.2.0"),
                ("json-schema", "0.1.0"),
                ("fastjson", "3.0.0")
            ]
        );
        assert_eq!(from_json(&to_json(&hits)).unwrap(), hits);
        assert!(search_local(&root.join("missing"), "x").unwrap().is_empty());
    }

    #[test]
    fn query_strings_round_trip() {
        for q in ["json", "a b&c=d", "ünï"] {
            assert_eq!(query_param(&format!("x=1&q={}", encode_query(q))), q);
        }
        assert_eq!(query_param("q=a+b"), "a b");
        assert_eq!(query_param("other=1"), "");
    }
}
