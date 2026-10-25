//! The checker's maps hash with FxHash (#1135): the standard library's default hasher (SipHash
//! with a random key) cost `velt check` of all of std 0.5% of its instructions in the throws
//! inference's span-keyed maps alone (#1425). This test keeps `std::collections` maps out of
//! the checker's sources, except where a public signature uses them.

use std::path::Path;

/// Files whose `std::collections` maps are part of a signature other crates see.
const ALLOWED: &[&str] = &[
    // The HIR contract (velt_vir reads these maps).
    "hir/mod.rs",
    // `erase` returns the names for the HIR's map.
    "readonly.rs",
    // Builds a `HashMap` field of the HIR.
    "synth_ifaces/upcasts.rs",
];

fn scan(dir: &Path, root: &Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        panic!("cannot read {}", dir.display());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan(&path, root, found);
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if ALLOWED.contains(&rel.as_str()) || rel == "hasher_tests.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if let Some(at) = code.find("std::collections::") {
                let rest = &code[at + "std::collections::".len()..];
                let names = rest.split(';').next().unwrap_or(rest);
                if names.contains("HashMap") || names.contains("HashSet") {
                    found.push(format!("{rel}:{}: {}", i + 1, line.trim()));
                }
            }
        }
    }
}

#[test]
fn checker_maps_use_fxhash() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = vec![];
    scan(&root, &root, &mut found);
    assert!(
        found.is_empty(),
        "use rustc_hash::FxHashMap / FxHashSet instead of std::collections maps:\n{}",
        found.join("\n")
    );
}
