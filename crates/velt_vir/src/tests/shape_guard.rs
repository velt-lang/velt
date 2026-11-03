//! Shape sharing (docs/internals/design/shape-sharing.md, "How the questions are enforced"):
//! lowering code outside `boxing/` and `shape/` reads a counted-types set of types only through
//! `Cx::ph_rep`, so a type built from a placeholder is answered for its representative and
//! checked at the end of the pass, never silently as "not in the set". Sets of class
//! hierarchies and interfaces (by `DefId`) need no mapping.

/// The sets keyed by `DefId`: no placeholder can be in one.
const BY_DEF: &[&str] = &[
    "roots",
    "recursive",
    "weak_roots",
    "lock_roots",
    "proxy_roots",
    "dyn_to_any",
];

/// `path`'s reads of a counted-types set of types that don't map placeholders first.
fn unguarded(path: &std::path::Path, out: &mut Vec<String>) {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    for (i, line) in text.lines().enumerate() {
        let mut rest = line;
        while let Some(at) = rest.find("boxing.") {
            rest = &rest[at + "boxing.".len()..];
            let set: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let after = &rest[set.len()..];
            if after.starts_with(".contains(")
                && !BY_DEF.contains(&set.as_str())
                && !line.contains("ph_rep(")
            {
                out.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    }
}

fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if name != "boxing" && name != "shape" {
                walk(&p, out);
            }
        } else if name.ends_with(".rs") {
            unguarded(&p, out);
        }
    }
}

#[test]
fn counted_type_sets_are_read_through_the_representative() {
    let lower = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lower");
    let mut found = vec![];
    walk(&lower, &mut found);
    assert!(
        found.is_empty(),
        "counted-types sets read without `ph_rep` (shape sharing would answer them silently \
         for a placeholder):\n{}",
        found.join("\n")
    );
}

#[test]
fn the_guard_finds_an_unguarded_read() {
    let dir = std::env::temp_dir().join(format!("velt-shape-guard-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("probe.rs");
    std::fs::write(
        &f,
        "let b = self.boxing.array_views.contains(&t);\nlet c = self.boxing.roots.contains(&d);\n",
    )
    .unwrap();
    let mut found = vec![];
    unguarded(&f, &mut found);
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(found.len(), 1, "{found:?}");
}
