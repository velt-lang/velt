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
/// marked "WebAssembly only" (velt_rt_wasm keeps its own, #2196), or of a row whose description
/// starts "No runtime" (std defines it on every target, #3905).
fn not_velt_rts(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .filter_map(|l| {
            let cells: Vec<&str> = l.trim_start().trim_start_matches('|').split('|').collect();
            let std_only = cells
                .get(2)
                .is_some_and(|d| d.trim().starts_with("No runtime"));
            (l.contains("WebAssembly only") || std_only).then(|| cells[0])
        })
        .flat_map(documented_cell)
        .collect()
}

#[test]
fn rows_no_runtime_defines_are_not_velt_rts() {
    let doc = "| `velt_rt_a` | `()` | No runtime defines it: std's hook. |\n\
               | `velt_rt_b` | `()` | WebAssembly only (velt_rt_wasm). |\n\
               | `velt_rt_c` | `()` | Takes `velt_rt_a`; No runtime is involved. |\n";
    let names: Vec<String> = not_velt_rts(doc).into_iter().collect();
    assert_eq!(names, ["velt_rt_a", "velt_rt_b"]);
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
    let not_ours: BTreeSet<String> = docs.iter().flat_map(|d| not_velt_rts(d)).collect();
    let documented: BTreeSet<String> = docs
        .iter()
        .flat_map(|d| documented(d))
        .filter(|n| n != "velt_rt_wasm" && !not_ours.contains(n))
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
