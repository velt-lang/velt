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
    // A framework only under `darwin`, even under a condition macOS also has.
    for condition in ["linux", "win32", "native", "default"] {
        let (message, covered) = error(&with(&format!(
            "native: {{ c: {{ x: {{ system: {{ {condition}: \"framework:Accelerate\" }} }} }} }}"
        )));
        assert!(
            message.contains("a framework is macOS-only: put it under `darwin`"),
            "{condition}: {message}"
        );
        assert_eq!(covered, "\"framework:Accelerate\"");
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
fn a_system_library_is_not_a_file() {
    for (file, library) in [
        ("foo.lib", "foo"),
        ("FOO.LIB", "FOO"),
        ("foo.dll", "foo"),
        ("libfoo.lib", "libfoo"),
        ("libfoo.a", "foo"),
        ("foo.a", "foo"),
        ("libfoo.so", "foo"),
        ("libfoo.so.1", "foo"),
        ("libfoo.so.1.2", "foo"),
        ("libfoo.dylib", "foo"),
        ("lib.a", "lib"),
    ] {
        let (message, covered) = error(&with(&format!(
            "native: {{ c: {{ x: {{ system: {{ linux: \"{file}\" }} }} }} }}"
        )));
        let expected = format!("name the library, not its file: `{library}`");
        assert!(message.contains(&expected), "{file}: {message}");
        assert_eq!(covered, format!("\"{file}\""));
    }
    // Under `framework:` too.
    let (message, _) = error(&with(
        "native: { c: { x: { system: { darwin: \"framework:Foo.dylib\" } } } }",
    ));
    assert!(
        message.contains("name the library, not its file"),
        "{message}"
    );
    // Dots that aren't a file's suffix.
    let ok = read(&with(
        "native: { c: { x: { system: { linux: \"python3.11\", darwin: \"foo.so.bar\", win32: \"a.b\" } } } }",
    ));
    assert!(ok.c_libraries.is_some());
}

/// Decoding goes on after an error, so what it keeps must already be checked.
#[test]
fn only_system_names_that_pass_are_kept() {
    let src = with(
        "native: { c: { x: { system: { default: \"-lfoo\", linux: \"framework:X\", macos: \"m\", win32: \"good\" } } } }",
    );
    let mut decoded = None;
    let result = read_data(FILE, &src, &PACKAGE_SHAPE, |r, v| {
        decoded = Some(r.manifest(v));
    });
    assert_eq!(result.err().expect("the manifest has errors").len(), 3);
    let libraries = decoded.unwrap().c_libraries.expect("native.c is decoded");
    assert_eq!(
        libraries["x"].system,
        [("win32".to_string(), "good".to_string())]
    );
}

#[test]
fn c_library_names_have_a_length_limit() {
    let at_limit = "x".repeat(64);
    let over = "x".repeat(65);
    // The key and the system name, the latter with and without `framework:`.
    let ok = read(&with(&format!(
        "native: {{ c: {{ {at_limit}: {{ system: {{ darwin: \"framework:{at_limit}\", default: \"{at_limit}\" }} }} }} }}"
    )));
    assert!(ok.c_libraries.is_some_and(|c| c.contains_key(&at_limit)));
    for src in [
        format!("native: {{ c: {{ {over}: {{}} }} }}"),
        format!("native: {{ c: {{ x: {{ system: {{ default: \"{over}\" }} }} }} }}"),
        format!("native: {{ c: {{ x: {{ system: {{ darwin: \"framework:{over}\" }} }} }} }}"),
    ] {
        let (message, _) = error(&with(&src));
        assert!(message.contains("too long for a library name"), "{message}");
        assert!(message.contains("65 bytes, at most 64"), "{message}");
        // The message doesn't repeat the whole name.
        assert!(!message.contains(&over), "{message}");
    }
    // Cut on a character, whatever the name holds.
    let wide = "\u{ff4d}".repeat(40);
    let (message, _) = error(&with(&format!(
        "native: {{ c: {{ x: {{ system: {{ default: \"{wide}\" }} }} }} }}"
    )));
    assert!(message.contains("too long for a library name"), "{message}");
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

#[test]
fn headers_and_unchecked_are_read_checked_and_written_back() {
    let m = read(&with(
        "native: { c: { sqlite3: { system: { default: \"sqlite3\" }, headers: [\"native/sqlite3.h\"] }, sdk: { system: { default: \"sdk\" }, unchecked: true }, c: { headers: [\"<string.h>\", \"<sys/stat.h>\"] } } }",
    ));
    let libraries = m.c_libraries.as_ref().unwrap();
    assert_eq!(libraries["sqlite3"].headers, ["native/sqlite3.h"]);
    assert!(!libraries["sqlite3"].unchecked);
    assert!(libraries["sdk"].unchecked && libraries["sdk"].headers.is_empty());
    // The platform's C library lists only headers.
    assert_eq!(libraries["c"].headers, ["<string.h>", "<sys/stat.h>"]);
    assert!(libraries["c"].system.is_empty());
    let text = m.to_vlt();
    assert!(text.contains("headers: [\"native/sqlite3.h\"]"), "{text}");
    assert!(text.contains("unchecked: true"), "{text}");
    assert_eq!(read(&text), m, "{text}");
    let json = m.to_json();
    assert_eq!(json["native"]["c"]["c"]["headers"][1], "<sys/stat.h>");
    assert_eq!(json["native"]["c"]["sdk"]["unchecked"], true);
    // A header is in the package or a C library's, never elsewhere.
    for bad in [
        "/usr/include/x.h",
        "../x.h",
        "a/../../x.h",
        "C:\\x.h",
        "<>",
        "<../x.h>",
        "",
    ] {
        let (message, _) = error(&with(&format!(
            "native: {{ c: {{ x: {{ headers: [{bad:?}] }} }} }}"
        )));
        assert!(message.contains("is not a header"), "{bad}: {message}");
    }
    let (message, _) = error(&with("native: { c: { x: { unchecked: \"yes\" } } }"));
    assert_eq!(message, "`unchecked` must be a boolean, not a string");
}
