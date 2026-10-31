//! The standard library modules the compiler knows by name, in one table
//! (docs/internals/design/std-layout.md §5), the prelude's load order (§4) and which std files
//! user code may import (§2).
//! CONTRACT FILE — changes need maintainer review (docs/internals/contracts/README.md).
//!
//! Each entry names a module by its path below the std root (`prelude/map` for
//! `std/prelude/map.vlt`, `lang/proxy` for `std/lang/proxy/index.vlt`). The compiler spells a
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
    /// The compiler's hooks: std functions lowering calls (`std_hooks.rs`; std-layout.md §10b).
    lang_hooks => "lang/hooks",
    lang_hooks_adapt => "lang/hooks/adapt",
    lang_hooks_any_object => "lang/hooks/any_object",
    lang_hooks_any_ops => "lang/hooks/any_ops",
    lang_hooks_any_text => "lang/hooks/any_text",
    lang_hooks_array => "lang/hooks/array",
    lang_hooks_array_view => "lang/hooks/array_view",
    lang_hooks_async_iter => "lang/hooks/async_iter",
    lang_hooks_class_object => "lang/hooks/class_object",
    lang_hooks_error => "lang/hooks/error",
    lang_hooks_iter => "lang/hooks/iter",
    lang_hooks_number => "lang/hooks/number",
    lang_hooks_process => "lang/hooks/process",
    lang_hooks_proxy => "lang/hooks/proxy",
    lang_hooks_record => "lang/hooks/record",
    lang_hooks_string => "lang/hooks/string",
    lang_hooks_syntax => "lang/hooks/syntax",
    lang_import_meta => "lang/import_meta",
    /// The compiler's intrinsics, declared (`declare intrinsic function`; std-layout.md §10).
    lang_intrinsics => "lang/intrinsics",
    lang_proxy => "lang/proxy",
    lang_proxy_array => "lang/proxy/array",
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
    db_row_source => "db/row_source",
    encoding_json_reader => "encoding/json/reader",
    encoding_json_writer => "encoding/json/writer",
    encoding_json_generated => "encoding/json/generated",
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
    text_regex_string_methods => "text/regex/string_methods",
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

/// The file listing the prelude's modules in load order, below the std root.
pub const PRELUDE_INDEX: &str = "prelude/index.vlt";

/// The prelude's modules in load order, as `prelude/index.vlt` (`index_src`) lists them: one
/// side-effect import per module (`import "./array";`), comments and blank lines between. Each
/// is a file name below `prelude/` without its extension (`array`). A line that is not such an
/// import is an error naming it (1-based line number).
pub fn prelude_order(index_src: &str) -> Result<Vec<String>, String> {
    let mut out = vec![];
    for (n, raw) in index_src.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let name = line
            .strip_prefix("import \"./")
            .and_then(|rest| rest.strip_suffix("\";"))
            .filter(|name| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            });
        match name {
            Some(name) if out.iter().any(|m| m == name) => {
                return Err(format!("line {}: `{name}` is listed twice", n + 1));
            }
            Some(name) => out.push(name.to_string()),
            None => {
                return Err(format!(
                    "line {}: expected `import \"./<module>\";`, found `{line}`",
                    n + 1
                ))
            }
        }
    }
    Ok(out)
}

/// The std folders user code never imports: the prelude (implicit) and `lang/` (what
/// implements the language).
const NOT_IMPORTABLE: [&str; 2] = ["prelude", "lang"];

/// Whether user code (any module not loaded from std) may import the std file at `rel_file`
/// (its path below the std root, `/`-separated: `net/http/index.vlt`): a module's `index.vlt`
/// outside `prelude/` and `lang/` (std-layout.md §2).
pub fn importable(rel_file: &str) -> bool {
    let first = rel_file.split('/').next().unwrap_or("");
    rel_file.ends_with("/index.vlt") && !NOT_IMPORTABLE.contains(&first)
}

/// The module that owns the std file at `rel_file` (as for [`importable`]): the nearest folder
/// above it with an `index.vlt` (`has_index("encoding/json")`), as its module path
/// (`encoding/json`). `None` for a file in no module, and in `prelude/` and `lang/`, which are
/// not modules.
pub fn owner(rel_file: &str, has_index: &dyn Fn(&str) -> bool) -> Option<String> {
    let mut dir = rel_file.rsplit_once('/').map(|(d, _)| d)?;
    if NOT_IMPORTABLE.contains(&dir.split('/').next().unwrap_or("")) {
        return None;
    }
    loop {
        if has_index(dir) {
            return Some(dir.to_string());
        }
        dir = dir.rsplit_once('/').map(|(d, _)| d)?;
    }
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

    #[test]
    fn prelude_order_reads_the_imports_in_order() {
        let src = "// Prelude: the order.\n\nimport \"./array\";\n// note\nimport \"./any_ops\";\n";
        assert_eq!(prelude_order(src).unwrap(), ["array", "any_ops"]);
        let err = prelude_order("import \"./a\";\nexport { x } from \"./b\";\n").unwrap_err();
        assert!(err.starts_with("line 2: expected"), "{err}");
        let err = prelude_order("import \"./a\";\nimport \"./a\";\n").unwrap_err();
        assert!(err.contains("twice"), "{err}");
        assert!(prelude_order("import \"../x\";").is_err());
        assert!(prelude_order("import \"./global/x\";").is_err());
    }

    #[test]
    fn std_prelude_index_lists_every_prelude_file() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std/prelude");
        let src = std::fs::read_to_string(dir.join("index.vlt")).expect("std/prelude/index.vlt");
        let mut listed = prelude_order(&src).expect("std/prelude/index.vlt");
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .expect("std/prelude")
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_str()?
                    .strip_suffix(".vlt")
                    .map(str::to_string)
            })
            .filter(|f| f != "index")
            .collect();
        listed.sort();
        files.sort();
        assert_eq!(
            listed, files,
            "std/prelude/index.vlt must list every prelude file once"
        );
    }

    #[test]
    fn user_code_imports_module_indexes_outside_prelude_and_lang() {
        assert!(importable("fs/index.vlt"));
        assert!(importable("net/http/index.vlt"));
        assert!(!importable("encoding/json/scan.vlt"));
        assert!(!importable("lang/proxy/index.vlt"));
        assert!(!importable("prelude/array.vlt"));
        assert!(!importable("prelude/index.vlt"));
        // A file directly in std is in no module (what was `sqlite.vlt` left for packages/).
        assert!(!importable("sqlite.vlt") && !importable("fs.vlt"));
    }

    #[test]
    fn a_file_belongs_to_the_nearest_module_above_it() {
        let indexes = ["encoding", "encoding/json", "net/http"];
        let has = |d: &str| indexes.contains(&d);
        assert_eq!(
            owner("encoding/json/scan.vlt", &has).as_deref(),
            Some("encoding/json")
        );
        assert_eq!(
            owner("encoding/utf8.vlt", &has).as_deref(),
            Some("encoding")
        );
        assert_eq!(
            owner("net/http/fetch/client/wire.vlt", &has).as_deref(),
            Some("net/http")
        );
        assert_eq!(owner("lang/proxy/ops.vlt", &|_| true), None);
        assert_eq!(owner("stray.vlt", &has), None);
        assert_eq!(owner("x/y.vlt", &has), None);
    }
}
