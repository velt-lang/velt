//! The JIT host's symbol table covers the whole runtime ABI: every `velt_rt_*` function in the
//! symbol tables of docs/internals/contracts/rt_abi*.md is in `ABI_SYMBOLS` with a real address.

use std::collections::BTreeSet;

use velt_rt_host::abi_symbols::ABI_SYMBOLS;

/// `velt_rt_*` identifiers in a contract document's tables (lines starting with `|`).
fn documented(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let table: String = text
        .lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .collect::<Vec<_>>()
        .join("\n");
    let mut rest = table.as_str();
    while let Some(i) = rest.find("velt_rt_") {
        let name: String = rest[i..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        rest = &rest[i + name.len()..];
        out.insert(name);
    }
    out
}

/// The symbols whose table row says velt_rt no longer defines them: the first cell of a row
/// marked "WebAssembly only" (velt_rt_wasm keeps its own, #2196).
fn webassembly_only(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter(|l| l.trim_start().starts_with('|') && l.contains("WebAssembly only"))
        .filter_map(|l| l.trim_start().trim_start_matches('|').split('|').next())
        .flat_map(documented_cell)
        .collect()
}

/// `velt_rt_*` identifiers in one cell.
fn documented_cell(cell: &str) -> BTreeSet<String> {
    documented(&format!("|{cell}"))
}

#[test]
fn table_covers_the_documented_abi() {
    let docs = [
        include_str!("../../../docs/internals/contracts/rt_abi.md"),
        include_str!("../../../docs/internals/contracts/rt_abi_async.md"),
    ];
    let table: BTreeSet<&str> = ABI_SYMBOLS.iter().map(|(n, _)| *n).collect();
    let wasm_only: BTreeSet<String> = docs.iter().flat_map(|d| webassembly_only(d)).collect();
    let documented: BTreeSet<String> = docs
        .iter()
        .flat_map(|d| documented(d))
        .filter(|n| n != "velt_rt_wasm" && !wasm_only.contains(n))
        .collect();
    // That the tables were read at all: the native ABI shrinks as std takes it over (99 with this
    // change and the process lifetime's, #2196).
    assert!(documented.len() > 60, "found only {documented:?}");
    let missing: Vec<&String> = documented
        .iter()
        .filter(|n| !table.contains(n.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "documented but not exported: {missing:?}"
    );
    assert!(ABI_SYMBOLS.iter().all(|(_, a)| !a.0.is_null()));
    assert!(!table.contains("main"));
}
