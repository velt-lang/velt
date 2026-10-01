//! `velt_rt_shared` compiles `velt_rt`'s sources, so it must declare the same dependencies and
//! features; a difference would build the shared runtime differently from the linked one.

/// From the `header` line up to `[dev-dependencies]` (or the end).
fn section<'a>(manifest: &'a str, header: &str) -> &'a str {
    let start = manifest
        .find(&format!("\n{header}\n"))
        .unwrap_or_else(|| panic!("no {header} section"));
    let end = manifest
        .find("\n[dev-dependencies]")
        .filter(|&end| end > start)
        .unwrap_or(manifest.len());
    &manifest[start..end]
}

#[test]
fn dependencies_match_velt_rt() {
    let shared = include_str!("../Cargo.toml");
    let rt = include_str!("../../velt_rt/Cargo.toml");
    assert_eq!(
        section(shared, "[dependencies]").trim(),
        section(rt, "[dependencies]").trim(),
        "crates/velt_rt_shared/Cargo.toml must copy [dependencies] and [features] from crates/velt_rt/Cargo.toml"
    );
}
