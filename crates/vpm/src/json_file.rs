//! The package manager's generated files (`velt.lock.json`, the registry's `index.json`, a native
//! bundle's `native.json`) are JSON: pretty-printed, fields in declaration order and maps sorted
//! (stable diffs), with a trailing newline. Their former TOML names get an error that says what
//! replaced them.

use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

/// `value` as the text of a generated file.
pub fn to_text(value: &impl Serialize) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("ICE: generated data serializes");
    text.push('\n');
    text
}

/// Parse generated-file text; the error names `what` (a file or its origin).
pub fn parse<T: DeserializeOwned>(text: &str, what: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|e| format!("invalid {what}: {e}"))
}

/// The error for a directory that still has the former TOML file `old` where `new` belongs.
pub fn legacy_error(old: &Path, new: &str, fix: &str) -> String {
    format!(
        "`{}` is no longer read (the file is now `{new}`): {fix}",
        old.display()
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn pretty_sorted_and_newline_terminated() {
        let map = BTreeMap::from([("b", 2), ("a", 1)]);
        let text = to_text(&map);
        assert_eq!(text, "{\n  \"a\": 1,\n  \"b\": 2\n}\n");
        let back: BTreeMap<String, i32> = parse(&text, "test file").unwrap();
        assert_eq!(back["a"], 1);
        let err = parse::<BTreeMap<String, i32>>("a = 1", "`x.json`").unwrap_err();
        assert!(err.starts_with("invalid `x.json`: "), "{err}");
    }
}
