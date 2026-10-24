//! Build script shared by `velt_rt` (the staticlib linked into programs), `velt_rt_host`
//! (the same sources as an rlib inside `velt`, for the `velt dev` JIT host) and `velt_rt_shared`
//! (the same sources as the shared library debug builds link):
//! - `velt_rt_host` gets `cfg(velt_rt_host)`, which drops the C `main` (the host has its own);
//! - `velt_rt_shared` gets `cfg(velt_rt_shared)`, which drops it too (the executable has its own),
//!   and on macOS the install name `@rpath/libvelt_rt_shared.dylib`;
//! - both get `$OUT_DIR/abi_symbols.rs`: every `#[no_mangle] extern "C"` function of the runtime
//!   as a `(name, address)` table, which the JIT registers so generated code can call the runtime.
//!   The addresses come from `extern "C"` declarations of the same symbols, so the table needs
//!   no module paths and keeps every function from being dead-stripped out of `velt`.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rustc-check-cfg=cfg(velt_rt_host)");
    println!("cargo::rustc-check-cfg=cfg(velt_rt_shared)");
    match std::env::var("CARGO_PKG_NAME").as_deref() {
        Ok("velt_rt_host") => println!("cargo::rustc-cfg=velt_rt_host"),
        Ok("velt_rt_shared") => {
            println!("cargo::rustc-cfg=velt_rt_shared");
            // Executables linked against the dylib record its install name; rustc's default is
            // the absolute path it was built at, which a toolchain installed elsewhere does not
            // have. `@rpath/` makes them find it through the rpath `velt_link` gives them.
            if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
                println!(
                    "cargo::rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libvelt_rt_shared.dylib"
                );
            }
        }
        _ => {}
    }
    // Both packages build from crates/velt_rt/src.
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let src = manifest.join("../velt_rt/src");
    println!("cargo::rerun-if-changed={}", src.display());
    // Listing the sources replaces cargo's default of rerunning when this script changes.
    println!(
        "cargo::rerun-if-changed={}",
        manifest.join("../velt_rt/build.rs").display()
    );
    let mut names = vec![];
    collect(&src, &mut names);
    // std's drivers (crates/velt_std_*: SDK-only crates linked into the runtime) export their
    // functions with `#[export]`; generated code calls them like the runtime's own, and the
    // compiler checks std's declarations against their signature records.
    let mut drivers = vec![];
    if let Ok(crates) = std::fs::read_dir(manifest.join("..")) {
        for c in crates.flatten() {
            if c.file_name().to_string_lossy().starts_with("velt_std_") {
                let dir = c.path().join("src");
                println!("cargo::rerun-if-changed={}", dir.display());
                collect(&dir, &mut drivers);
            }
        }
    }
    drivers.sort();
    drivers.dedup();
    names.extend(drivers.iter().cloned());
    names.sort();
    names.dedup();
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets it"));
    let text = render(&names) + &render_signatures(&drivers);
    std::fs::write(out.join("abi_symbols.rs"), text).expect("write abi_symbols.rs");
}

/// Exported function names in every `.rs` file under `dir` (tests excluded).
fn collect(dir: &Path, names: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, names);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            names.extend(exported_functions(&text));
        }
    }
}

/// `NAME` of each `#[no_mangle]` followed by `pub [unsafe] extern "C" fn NAME`. `main` is the
/// process entry, not part of the ABI the generated code calls. Comments don't count: a
/// `// SAFETY:` line between the attribute and the function keeps it, and a commented-out
/// function is not exported (the change planner, crates/xtask, treats comment edits as such).
pub(crate) fn exported_functions(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut pending = false;
    for line in strip_comments(text).lines().map(str::trim) {
        if line == "#[no_mangle]" || line == "#[export]" || line.starts_with("#[export(") {
            pending = true;
            continue;
        }
        if !pending || line.is_empty() || line.starts_with("#[") {
            continue;
        }
        pending = false;
        // `extern "C" fn name` after `#[no_mangle]`; `fn name` / `async fn name` after `#[export]`.
        let rest = match line.split_once("extern \"C\" fn ") {
            Some((_, rest)) => rest,
            None => match plain_fn(line) {
                Some(rest) => rest,
                None => continue,
            },
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() && name != "main" {
            out.push(name);
        }
    }
    out
}

/// The rest of `line` after `fn ` when it declares a plain Rust function (`fn`, `pub fn`,
/// `async fn`, `pub async fn`): what an `#[export]` attribute decorates.
fn plain_fn(line: &str) -> Option<&str> {
    let (before, rest) = line.split_once("fn ")?;
    matches!(before.trim(), "" | "pub" | "async" | "pub async").then_some(rest)
}

/// `text` without its comments (doc comments included), keeping every newline. String and
/// character literals are copied whole, so `"/*"` in a string opens no comment. (The planner's
/// crates/xtask/src/comments.rs lexes the same way.)
fn strip_comments(text: &str) -> String {
    let c: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if c[i] == '/' && c.get(i + 1) == Some(&'*') {
            let mut depth = 0;
            while i < c.len() {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if c[i] == '\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
            }
            out.push(' ');
        } else {
            let end = literal_end(&c, i).unwrap_or(i + 1);
            out.extend(&c[i..end]);
            i = end;
        }
    }
    out
}

/// The end of the string, raw string or character literal starting at `i` (with its `b`, `c`
/// or `r` prefix), or `None` when none starts there (a lifetime's `'` included).
fn literal_end(c: &[char], i: usize) -> Option<usize> {
    if i > 0 && (c[i - 1].is_alphanumeric() || c[i - 1] == '_') {
        return None;
    }
    let mut j = i;
    if matches!(c[j], 'b' | 'c') {
        j += 1;
    }
    if c.get(j) == Some(&'r') {
        let mut k = j + 1;
        let mut hashes = 0;
        while c.get(k) == Some(&'#') {
            hashes += 1;
            k += 1;
        }
        if c.get(k) != Some(&'"') {
            return None;
        }
        k += 1;
        while k < c.len() {
            if c[k] == '"' && (1..=hashes).all(|h| c.get(k + h) == Some(&'#')) {
                return Some(k + 1 + hashes);
            }
            k += 1;
        }
        return Some(c.len());
    }
    let quoted_end = |quote: char| {
        let mut k = j + 1;
        while k < c.len() {
            match c[k] {
                '\\' => k += 2,
                ch if ch == quote => return k + 1,
                _ => k += 1,
            }
        }
        c.len()
    };
    match c.get(j) {
        Some('"') => Some(quoted_end('"')),
        Some('\'') if c.get(j + 1) == Some(&'\\') => Some(quoted_end('\'')),
        Some('\'') if c.get(j + 2) == Some(&'\'') => Some(j + 3),
        _ => None,
    }
}

/// The drivers' signature records (`velt_sig_<name>`, emitted by the SDK's `#[export]`).
fn render_signatures(names: &[String]) -> String {
    let mut s = String::from("\nextern \"C\" {\n");
    for n in names {
        s.push_str(&format!("    static velt_sig_{n}: u8;\n"));
    }
    s.push_str(
        "}\n\n/// std's driver exports with their signature records (`(name, signature)`).\n",
    );
    s.push_str("pub fn driver_signatures() -> Vec<(&'static str, &'static str)> {\n");
    s.push_str("    // SAFETY: each record is a NUL-terminated static of the linked-in driver.\n");
    s.push_str("    unsafe {\n        vec![\n");
    for n in names {
        s.push_str(&format!(
            "            (\"{n}\", record(std::ptr::addr_of!(velt_sig_{n}))),\n"
        ));
    }
    s.push_str("        ]\n    }\n}\n");
    s
}

fn render(names: &[String]) -> String {
    let mut s = String::from("// Generated by crates/velt_rt/build.rs.\nextern \"C\" {\n");
    for n in names {
        s.push_str(&format!("    fn {n}();\n"));
    }
    s.push_str("}\n\n/// Every runtime function generated code may call, by symbol name.\n");
    s.push_str(&format!(
        "pub static ABI_SYMBOLS: [(&str, AbiAddress); {}] = [\n",
        names.len()
    ));
    for n in names {
        s.push_str(&format!("    (\"{n}\", AbiAddress({n} as *const u8)),\n"));
    }
    s.push_str("];\n");
    s
}
