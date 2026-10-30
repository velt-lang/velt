//! The standard library modules the compiler knows by name, in one table
//! (docs/internals/design/std-layout.md §5).
//! CONTRACT FILE — changes need maintainer review (docs/internals/contracts/README.md).
//!
//! Each entry names a module by its path below the std root (`prelude/adapt` for
//! `std/prelude/adapt.vlt`, `lang/proxy` for `std/lang/proxy/index.vlt`). The compiler spells a
//! module's canonical path (`"std/lang/proxy"`), a definition's qualified name
//! (`"std/lang/proxy::Proxy"`) or a module's import specifier (`"velt/jsx"`) only through the
//! macros below, never as a string literal of its own, so moving a std file changes this table
//! and nothing else. A test checks that every entry names a file of `std/`.
//!
//! ```
//! use velt_common::{std_dir, std_item, std_module, std_rel, std_specifier};
//! assert_eq!(std_rel!(lang_proxy), "lang/proxy");
//! assert_eq!(std_module!(lang_proxy), "std/lang/proxy");
//! assert_eq!(std_item!(lang_proxy, "Proxy"), "std/lang/proxy::Proxy");
//! assert_eq!(std_dir!(prelude), "std/prelude/");
//! assert_eq!(std_specifier!(jsx), "velt/jsx");
//! ```

/// The prefix of every std module's canonical path: `std/fs` is the module `velt/fs`.
pub const CANONICAL_PREFIX: &str = "std/";

/// The prefix of every std import specifier (`velt/fs`).
pub const SPECIFIER_PREFIX: &str = "velt/";

/// Whether canonical module path or qualified name `name` belongs to the standard library.
pub fn is_std(name: &str) -> bool {
    name.starts_with(CANONICAL_PREFIX)
}

/// The import specifier of the std module with canonical path `canonical` (`std/fs` →
/// `velt/fs`, `std/fs/index` → `velt/fs`); `None` outside std.
pub fn specifier(canonical: &str) -> Option<String> {
    let rel = canonical.strip_prefix(CANONICAL_PREFIX)?;
    let rel = rel.strip_suffix("/index").unwrap_or(rel);
    Some(format!("{SPECIFIER_PREFIX}{rel}"))
}

macro_rules! table {
    ($($(#[doc = $doc:literal])* $name:ident => $rel:literal,)*) => {
        /// The table: `std_rel!(name)` is entry `name`'s path below the std root
        /// (`std_rel!(lang_proxy)` is `"lang/proxy"`). The other forms: [`std_module!`],
        /// [`std_item!`], [`std_dir!`], [`std_specifier!`].
        #[macro_export]
        macro_rules! std_rel {
            $( ($name) => { $rel }; )*
        }

        /// Every entry, `(name, path below the std root)`, in table order: for tools and tests
        /// that read the macros' uses in source text ([`rel`]).
        pub const ENTRIES: &[(&str, &str)] = &[$((stringify!($name), $rel)),*];
    };
}

table! {
    // The prelude (loaded into every program, never imported).
    /// The prelude's folder.
    prelude => "prelude",
    prelude_adapt => "prelude/adapt",
    prelude_any_ops => "prelude/any_ops",
    prelude_array => "prelude/array",
    prelude_bits => "prelude/bits",
    prelude_compare => "prelude/compare",
    prelude_error => "prelude/error",
    /// The globals loaded on demand (`prelude/global/*.vlt`).
    prelude_global => "prelude/global",
    prelude_iter => "prelude/iter",
    prelude_json => "prelude/json/json",
    prelude_map => "prelude/map",
    prelude_math => "prelude/math",
    prelude_process => "prelude/process",
    prelude_promise => "prelude/promise",
    prelude_record => "prelude/record",
    prelude_sync => "prelude/sync",
    prelude_weak => "prelude/weak",

    // What implements the language and its globals (never imported by user code).
    lang => "lang",
    lang_import_meta => "lang/import_meta",
    lang_proxy => "lang/proxy",
    lang_proxy_array => "lang/proxy/array",
    lang_proxy_ops => "lang/proxy/ops",
    lang_record => "lang/record",
    lang_sort => "lang/sort",
    lang_sort_stable => "lang/sort/stable",
    lang_symbol => "lang/symbol",
    lang_this_fn => "lang/this_fn",
    lang_typed_array => "lang/typed_array",
    lang_typed_array_core => "lang/typed_array/core",

    // Modules and their files that lowering and the checks know.
    collections => "collections",
    collections_set => "collections/set",
    db => "db",
    encoding_json_reader => "encoding/json/reader",
    encoding_json_writer => "encoding/json/writer",
    /// The default JSX provider (`jsxImportSource`).
    jsx => "jsx",
    net_http_fetch_response => "net/http/fetch/response",
    process_stdout => "process/stdout",
    simd => "simd",
    simd_clmul => "simd/clmul",
    simd_sha => "simd/sha",
    sync_channel => "sync/channel",
    sync_epoch => "sync/epoch",
    sync_epoch_reclaim => "sync/epoch/reclaim",
    /// The test runner's module, also `vitest`.
    test => "test",
    text_regex_regexp => "text/regex/regexp",
    text_regex_regexp_match => "text/regex/regexp_match",
    text_string_builder => "text/string_builder",
}

/// The path below the std root of the table entry named `name` (`"lang_proxy"` →
/// `"lang/proxy"`), for code that reads the macros' uses as text.
pub fn rel(name: &str) -> Option<&'static str> {
    ENTRIES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, rel)| *rel)
}

/// The canonical module path of table entry `name`: `std_module!(lang_proxy)` is
/// `"std/lang/proxy"`. A literal, usable in `const`s, `concat!` and patterns.
#[macro_export]
macro_rules! std_module {
    ($name:ident) => {
        concat!("std/", $crate::std_rel!($name))
    };
}

/// The qualified name of definition `item` of table entry `name`: `std_item!(lang_proxy,
/// "Proxy")` is `"std/lang/proxy::Proxy"` (a method: `"Proxy.__view"`).
#[macro_export]
macro_rules! std_item {
    ($name:ident, $item:literal) => {
        concat!("std/", $crate::std_rel!($name), "::", $item)
    };
}

/// The prefix of the modules in table entry `name`'s folder: `std_dir!(prelude)` is
/// `"std/prelude/"`.
#[macro_export]
macro_rules! std_dir {
    ($name:ident) => {
        concat!("std/", $crate::std_rel!($name), "/")
    };
}

/// The import specifier of table entry `name`: `std_specifier!(jsx)` is `"velt/jsx"`.
#[macro_export]
macro_rules! std_specifier {
    ($name:ident) => {
        concat!("velt/", $crate::std_rel!($name))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn every_entry_names_a_std_file() {
        let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
        let missing: Vec<&str> = ENTRIES
            .iter()
            .map(|(_, rel)| *rel)
            .filter(|rel| {
                let dir = std.join(rel);
                !dir.with_extension("vlt").is_file()
                    && !dir.join("index.vlt").is_file()
                    && !dir.is_dir()
            })
            .collect();
        assert!(missing.is_empty(), "not in std/: {missing:?}");
        let mut sorted: Vec<&str> = ENTRIES.iter().map(|(_, rel)| *rel).collect();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ENTRIES.len(), "an entry is listed twice");
        assert_eq!(rel("lang_proxy"), Some("lang/proxy"));
        assert_eq!(rel("nothing"), None);
    }

    #[test]
    fn specifiers() {
        assert_eq!(specifier("std/fs").as_deref(), Some("velt/fs"));
        assert_eq!(specifier("std/fs/index").as_deref(), Some("velt/fs"));
        assert_eq!(specifier("main"), None);
        assert!(is_std("std/fs::readFile") && !is_std("stdlib"));
    }
}
