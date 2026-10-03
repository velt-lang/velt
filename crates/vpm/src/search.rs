//! `velt search`: the packages of a registry that match a text, by name, keywords and description,
//! each with the version `velt add` picks: its newest stable version that is not yanked (a
//! pre-release only when it has no stable one), and that version's description and keywords. A
//! remote registry answers `GET /api/v1/search?q=<text>` with the JSON of [`to_json`]; a local one
//! (and the server itself) is searched by [`search_local`].

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::locations::Locations;
use crate::registry::{parse_index, INDEX_FILE};

/// Most hits one search returns.
pub const MAX_HITS: usize = 50;

/// One package found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    /// Package name.
    pub name: String,
    /// Its newest stable version that is not yanked, else its newest pre-release that is not.
    pub version: String,
    /// That version's `description`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// That version's `keywords`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Answer {
    packages: Vec<Hit>,
}

/// Search `loc`'s registry (remote or local) for `query`.
pub fn search(loc: &Locations, query: &str) -> Result<Vec<Hit>, String> {
    search_within(loc, query, velt_http::Limits::DEFAULT)
}

/// [`search`], asking a remote registry within `limits`.
pub fn search_within(
    loc: &Locations,
    query: &str,
    limits: velt_http::Limits,
) -> Result<Vec<Hit>, String> {
    match &loc.remote {
        Some(url) => crate::remote::search_within(url, query, limits),
        None => search_local(&loc.registry, query),
    }
}

/// Search the registry directory `root` for `query`: lowercased and split into terms on
/// whitespace, a package matches when every term is in its name, one of its keywords or its
/// description. Ranked by the best [`tier`] any term reaches, then by how many terms reach it,
/// then by name. Packages whose versions are all yanked are left out.
pub fn search_local(root: &Path, query: &str) -> Result<Vec<Hit>, String> {
    let query = query.trim().to_lowercase();
    let terms: Vec<&str> = query.split_whitespace().collect();
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(Vec::new());
    };
    let mut ranked = Vec::new();
    // Keywords and descriptions are in the indexes, so every package's index is read: fine for
    // a registry directory of today's size; a large registry would keep them in memory.
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !crate::manifest::is_valid_package_name(&name) {
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
        let Some(version) = crate::manifest::ide::registry::newest(&index) else {
            continue;
        };
        let shown = index.versions.iter().find(|e| e.semver() == version);
        let hit = Hit {
            name,
            version: version.to_string(),
            description: shown.and_then(|e| e.description.clone()),
            keywords: shown.map(|e| e.keywords.clone()).unwrap_or_default(),
        };
        if let Some(rank) = rank(&hit, &query, &terms) {
            ranked.push((rank, hit));
        }
    }
    ranked.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.name.cmp(&y.name)));
    Ok(ranked
        .into_iter()
        .map(|(_, hit)| hit)
        .take(MAX_HITS)
        .collect())
}

/// How well one term matches `hit` (lower is better), if at all: 2 a keyword equals it, 3 the
/// name contains it, 4 a keyword starts with it, 5 the description contains it. (Tiers 0 and 1,
/// the name equal to or starting with the whole query, are [`rank`]'s.)
fn tier(hit: &Hit, term: &str) -> Option<u8> {
    if hit.keywords.iter().any(|k| k == term) {
        Some(2)
    } else if hit.name.contains(term) {
        Some(3)
    } else if hit.keywords.iter().any(|k| k.starts_with(term)) {
        Some(4)
    } else if hit
        .description
        .as_ref()
        .is_some_and(|d| d.to_lowercase().contains(term))
    {
        Some(5)
    } else {
        None
    }
}

/// `(tier, terms not at that tier)` when every term matches `hit`; no terms match everything.
fn rank(hit: &Hit, query: &str, terms: &[&str]) -> Option<(u8, usize)> {
    let tiers = terms
        .iter()
        .map(|t| tier(hit, t))
        .collect::<Option<Vec<u8>>>()?;
    let best = if hit.name == query {
        0
    } else if !query.is_empty() && hit.name.starts_with(query) {
        1
    } else {
        tiers.iter().copied().min().unwrap_or(3)
    };
    let others = tiers.iter().filter(|t| **t != best).count();
    Some((best, others))
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
    fn finds_names_ranks_and_picks_versions_like_add() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        publish(
            root,
            "json",
            &[("1.0.0", false), ("1.2.0", false), ("2.0.0", true)],
        );
        publish(root, "json-schema", &[("0.1.0", false)]);
        // `velt add` picks the stable version; a pre-release shows only when there is no other.
        publish(
            root,
            "json-next",
            &[("1.0.0", false), ("2.0.0-beta.1", false)],
        );
        publish(root, "json-pre", &[("0.1.0-alpha.1", false)]);
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
                ("json-next", "1.0.0"),
                ("json-pre", "0.1.0-alpha.1"),
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

    /// One stable version of `name` with `description` and `keywords`.
    fn publish_described(root: &Path, name: &str, description: Option<&str>, keywords: &[&str]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut entry = serde_json::json!({ "version": "1.0.0", "checksum": "sha256:0" });
        if let Some(d) = description {
            entry["description"] = d.into();
        }
        if !keywords.is_empty() {
            entry["keywords"] = keywords.into();
        }
        let text = serde_json::json!({ "versions": [entry] }).to_string();
        std::fs::write(dir.join(INDEX_FILE), text).unwrap();
    }

    #[test]
    fn keywords_and_descriptions_match_and_rank_after_names() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        publish_described(root, "json", None, &[]);
        publish_described(root, "json-schema", None, &[]);
        publish_described(
            root,
            "fastparse",
            Some("A fast parser"),
            &["json", "parser"],
        );
        publish_described(root, "fastjson", None, &[]);
        publish_described(root, "yaml", None, &["jsonish"]);
        publish_described(root, "config", Some("Reads JSON and TOML files"), &[]);
        publish_described(root, "http", Some("HTTP client"), &["net"]);
        let names = |q: &str| -> Vec<String> {
            search_local(root, q)
                .unwrap()
                .into_iter()
                .map(|h| h.name)
                .collect()
        };
        // Name equal, name prefix, keyword equal, name contains, keyword prefix, description.
        assert_eq!(
            names("json"),
            [
                "json",
                "json-schema",
                "fastparse",
                "fastjson",
                "yaml",
                "config"
            ]
        );
        // Every term must match somewhere.
        assert_eq!(names("json parser"), ["fastparse"]);
        assert_eq!(names("TOML  reads"), ["config"]);
        assert!(names("json net").is_empty());
        // Hits carry the shown version's description and keywords, through the JSON answer too.
        let hits = search_local(root, "fastparse").unwrap();
        assert_eq!(hits[0].description.as_deref(), Some("A fast parser"));
        assert_eq!(hits[0].keywords, ["json", "parser"]);
        assert_eq!(from_json(&to_json(&hits)).unwrap(), hits);
        // An answer from an older server (no such fields) still reads.
        let old = from_json(r#"{"packages":[{"name":"a","version":"1.0.0"}]}"#).unwrap();
        assert_eq!(
            (old[0].description.clone(), old[0].keywords.len()),
            (None, 0)
        );
    }
}
