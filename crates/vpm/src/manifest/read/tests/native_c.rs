//! `native.c` and `allowNative` of package.vlt (docs/internals/design/c-ffi.md).

use super::*;

#[test]
fn c_libraries_are_decoded_and_only_c_is_no_crate() {
    let m = read(&with(
        "native: { c: { sqlite3: { system: { darwin: \"sqlite3\", linux: \"sqlite3\", win32: \"winsqlite3\" } }, blas: { system: { darwin: \"framework:Accelerate\", default: \"openblas\" } } } }, allowNative: [\"sqlite\"]",
    ));
    // `native` with only `c` is no Rust crate.
    assert!(m.native.is_none());
    assert_eq!(m.allow_native, ["sqlite"]);
    let libraries = m.c_libraries.as_ref().expect("native.c is decoded");
    let sqlite = &libraries["sqlite3"];
    assert_eq!(sqlite.system_for("aarch64-apple-darwin"), Some("sqlite3"));
    assert_eq!(
        sqlite.system_for("x86_64-pc-windows-msvc"),
        Some("winsqlite3")
    );
    assert_eq!(sqlite.system_for("wasm32-wasip1"), None);
    let blas = &libraries["blas"];
    // The first condition the build has, in the order written.
    assert_eq!(
        blas.system_for("aarch64-apple-darwin"),
        Some("framework:Accelerate")
    );
    assert_eq!(
        blas.system_for("x86_64-unknown-linux-gnu"),
        Some("openblas")
    );
    // With crate keys too, the crate is there.
    let both = read(&with(
        "native: { targets: [], c: { z: { system: { default: \"z\" } } } }",
    ));
    assert!(both.native.is_some() && both.c_libraries.is_some_and(|c| c.contains_key("z")));
    // A package declaring only the C library's functions says so with an empty `c`: it has
    // native code, as one with libraries or a crate does, and one without `native` has none.
    let libc_only = read(&with("native: { c: {} }"));
    assert!(libc_only.native.is_none() && libc_only.has_native_code());
    assert!(!read(&with("define: {}")).has_native_code());
}

#[test]
fn c_library_entries_are_checked() {
    // A system library is a name, never a linker flag.
    for bad in ["-lfoo", "-Wl,-rpath,/x", "a b", "", "framework:", ".hidden"] {
        let src = with(&format!(
            "native: {{ c: {{ x: {{ system: {{ default: {bad:?} }} }} }} }}"
        ));
        let (message, _) = error(&src);
        assert!(
            message.contains("is not a system library"),
            "{bad}: {message}"
        );
    }
    let (message, covered) = error(&with("native: { c: { x: { system: { macos: \"x\" } } } }"));
    assert!(
        message.contains("`macos` is not a condition of a native target"),
        "{message}"
    );
    assert_eq!(covered, "macos");
    let (message, _) = error(&with(
        "native: { c: { c: { system: { default: \"c\" } } } }",
    ));
    assert!(
        message.contains("`c` is the platform's C library"),
        "{message}"
    );
    let (message, _) = error(&with("native: { c: { x: { sytem: {} } } }"));
    assert!(message.contains("unknown key `sytem`"), "{message}");
    let (message, _) = error(&with("allowNative: \"sqlite\""));
    assert_eq!(message, "`allowNative` must be an array, not a string");
}

#[test]
fn c_libraries_and_allow_native_are_written_back() {
    let m = read(&with(
        "native: { c: { sqlite3: { system: { linux: \"sqlite3\", default: \"winsqlite3\" } }, \"my-lib\": {} } }, allowNative: [\"sqlite\", \"llama\"]",
    ));
    let text = m.to_vlt();
    assert!(
        text.contains("allowNative: [\"sqlite\", \"llama\"]"),
        "{text}"
    );
    assert!(
        text.contains("system: { linux: \"sqlite3\", default: \"winsqlite3\" }"),
        "{text}"
    );
    assert_eq!(read(&text), m, "{text}");
    // Only the C library's: `c: {}` stays, so the package still says it has native code.
    let libc = read(&with("native: { c: {} }"));
    let text = libc.to_vlt();
    assert!(text.contains("native: { c: {} }"), "{text}");
    assert_eq!(read(&text), libc);
    // Beside a crate.
    let both = read(&with(
        "native: { targets: [\"x86_64-unknown-linux-gnu\"], c: {} }",
    ));
    assert_eq!(read(&both.to_vlt()), both);
    let json = both.to_json();
    assert_eq!(json["native"]["c"], serde_json::json!({}));
    assert_eq!(json["native"]["path"], "native");
}
