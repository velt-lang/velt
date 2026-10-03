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

/// Write `value` to `path` as a generated file, atomically ([`write_atomic`]).
pub fn write(path: &Path, value: &impl Serialize) -> Result<(), String> {
    write_atomic(path, &to_text(value))
}

/// Replace `path` with `text` atomically: a temporary file in the same directory, synced to disk
/// and renamed over it, so a concurrent reader (a registry server answering while a package is
/// published, a build reading the lock file) sees the old file or the new one, never a truncated
/// one, and a crash never leaves an empty one.
pub fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map_or_else(Default::default, |n| n.to_string_lossy());
    let tmp = path.with_file_name(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let written = write_synced(&tmp, text.as_bytes()).and_then(|()| std::fs::rename(&tmp, path));
    written.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write `{}`: {e}", path.display())
    })?;
    sync_dir(path);
    Ok(())
}

/// Write `bytes` to a new file at `path` and wait until they are on disk, so a crash after the
/// rename never leaves an empty file.
fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Make a rename in `path`'s directory durable (Unix; Windows has no directory handle to sync).
/// Best effort: the rename itself has already succeeded.
fn sync_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    }
    #[cfg(not(unix))]
    let _ = path;
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
    fn writes_replace_the_file_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("index.json");
        write(&path, &BTreeMap::from([("a", 1)])).unwrap();
        write(&path, &BTreeMap::from([("b", 2)])).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"b\": 2\n}\n"
        );
        // No temporary file is left behind.
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
        let e = write(&tmp.path().join("missing/x.json"), &1).unwrap_err();
        assert!(e.contains("cannot write"), "{e}");
    }

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
