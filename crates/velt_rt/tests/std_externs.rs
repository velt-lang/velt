//! Every `declare function` in `std/**/*.vlt` must match the runtime's definition exactly, as the
//! compiler lowers it: WebAssembly links only identical signatures, so a mismatch native builds
//! tolerate (an ignored `i32` result, a `u64` where a pointer is 64 bits wide) breaks wasm32.
//!
//! The Velt side is mapped with the lowering ABI of `declare function` (rt_abi_async.md §3.1):
//! scalars stay scalars, every other value (string, array, struct, tuple, closure, `Promise`) is a
//! pointer, and an aggregate result becomes a trailing out-pointer with a `void` return. The Rust
//! side is read from the `#[no_mangle] extern "C"` definitions in velt_rt (also what velt_rt_host
//! hosts) and in velt_rt_wasm's own modules. `bool` and `u8` are the same type at this boundary.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// An ABI-level type: the width class the linker (and wasm's validator) sees.
type Sig = (Vec<String>, String);

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

/// Splits `a, b<c, d>, [e, f]` at top-level commas.
fn split_top(list: &str) -> Vec<String> {
    let (mut out, mut depth, mut cur) = (vec![], 0i32, String::new());
    for c in list.chars() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Index of the `)` that closes the `(` at `open`.
fn closing_paren(text: &str, open: usize) -> usize {
    let mut depth = 0;
    for (i, c) in text[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return open + i;
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced parentheses in {}", &text[open..]);
}

/// The ABI class of a Velt type in an extern signature.
fn velt_class(ty: &str) -> String {
    let ty = ty.trim();
    match ty {
        "i8" | "i16" | "i32" | "i64" | "u16" | "u32" | "u64" | "f32" | "f64" => ty.to_string(),
        "bool" | "u8" => "u8".to_string(),
        "number" => "f64".to_string(),
        "" | "void" | "never" => "void".to_string(),
        _ => "agg".to_string(),
    }
}

/// `symbol -> (params, result)` of every `declare function` under `std/`.
fn velt_declarations() -> BTreeMap<String, (Sig, PathBuf)> {
    let mut paths = vec![];
    files(&repo().join("std"), "vlt", &mut paths);
    let mut out = BTreeMap::new();
    for path in paths {
        let text = std::fs::read_to_string(&path).expect("read std source");
        let mut rest = text.as_str();
        while let Some(i) = rest.find("declare ") {
            let line_start = rest[..i].rfind('\n').map_or(0, |n| n + 1);
            if !rest[line_start..i].trim().is_empty() {
                // Prose (a comment) mentioning `declare`, not a declaration.
                rest = &rest[i + "declare ".len()..];
                continue;
            }
            let decl = &rest[i..];
            let end = decl.find(';').expect("declaration ends with `;`");
            rest = &decl[end..];
            let decl = &decl[..end];
            let Some(f) = decl.find("function ") else {
                continue;
            };
            let open = decl.find('(').expect("parameter list");
            let name = decl[f + "function ".len()..open].trim().to_string();
            let close = closing_paren(decl, open);
            let ret = decl[close + 1..].trim().trim_start_matches(':').trim();
            let mut params: Vec<String> = split_top(&decl[open + 1..close])
                .iter()
                .map(|p| velt_class(p.split_once(':').map_or("", |(_, t)| t)))
                .map(|c| if c == "agg" { "ptr".to_string() } else { c })
                .collect();
            let ret = match velt_class(ret).as_str() {
                "agg" if decl.contains("async ") => "ptr".to_string(),
                "agg" => {
                    params.push("ptr".to_string());
                    "void".to_string()
                }
                c => c.to_string(),
            };
            out.insert(name, ((params, ret), path.clone()));
        }
    }
    out
}

/// `pub type NAME = TYPE;` aliases in both runtimes' sources.
fn type_aliases() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for path in runtime_sources("velt_rt")
        .into_iter()
        .chain(runtime_sources("velt_rt_wasm"))
    {
        let text = std::fs::read_to_string(&path).expect("read runtime source");
        for line in text.lines().map(str::trim) {
            let Some(rest) = line.strip_prefix("pub type ") else {
                continue;
            };
            if let Some((name, ty)) = rest.trim_end_matches(';').split_once(" = ") {
                out.insert(name.trim().to_string(), ty.trim().to_string());
            }
        }
    }
    out
}

/// The ABI class of a Rust type in an `extern "C"` signature.
fn rust_class(ty: &str, aliases: &BTreeMap<String, String>) -> String {
    let mut ty = ty.trim();
    while let Some(target) = aliases.get(ty) {
        ty = target;
    }
    if ty.starts_with("Handle<") || ty.starts_with("Key<") {
        // `handle::Handle<T>` and `registry::Key<T>` are `repr(transparent)` over `u64`.
        return "u64".to_string();
    }
    if ty.starts_with('*')
        || ty.starts_with('&')
        || ty.starts_with("Option<")
        || ty.starts_with("extern ")
        || ty.starts_with("unsafe extern ")
    {
        return "ptr".to_string();
    }
    match ty {
        "" | "()" | "!" => "void".to_string(),
        "bool" | "u8" => "u8".to_string(),
        "usize" | "isize" => format!("{ty} (use u64/i64 or a pointer)"),
        // Callbacks named by an alias are function pointers.
        _ if ty.ends_with("Fn") => "ptr".to_string(),
        _ => ty.to_string(),
    }
}

/// `symbol -> signatures` of the `#[no_mangle] extern "C"` functions in the given sources (a
/// symbol can have several cfg-gated definitions).
fn rust_definitions(paths: &[PathBuf]) -> BTreeMap<String, Vec<(Sig, PathBuf)>> {
    let aliases = type_aliases();
    let mut out: BTreeMap<String, Vec<(Sig, PathBuf)>> = BTreeMap::new();
    for path in paths {
        let text = std::fs::read_to_string(path).expect("read runtime source");
        let mut rest = text.as_str();
        while let Some(i) = rest.find("#[no_mangle]") {
            let item = &rest[i..];
            rest = &item["#[no_mangle]".len()..];
            let Some(f) = item.find("extern \"C\" fn ") else {
                continue;
            };
            let open = f + item[f..].find('(').expect("parameter list");
            let name = item[f + "extern \"C\" fn ".len()..open].trim().to_string();
            let close = closing_paren(item, open);
            let body = close + item[close..].find('{').expect("function body");
            let ret = item[close + 1..body].trim().trim_start_matches("->");
            let params = split_top(&item[open + 1..close])
                .iter()
                .map(|p| rust_class(p.split_once(':').map_or("", |(_, t)| t), &aliases))
                .collect();
            let sig = (params, rust_class(ret, &aliases));
            out.entry(name).or_default().push((sig, path.clone()));
        }
    }
    out
}

fn runtime_sources(krate: &str) -> Vec<PathBuf> {
    let mut paths = vec![];
    files(
        &repo().join("crates").join(krate).join("src"),
        "rs",
        &mut paths,
    );
    paths
}

/// `path` relative to the repository root, for messages.
fn short(path: &Path) -> String {
    let root = repo().canonicalize().expect("repository root");
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    path.strip_prefix(&root)
        .unwrap_or(&path)
        .display()
        .to_string()
}

fn render((params, ret): &Sig) -> String {
    format!("({}) -> {ret}", params.join(", "))
}

/// Mismatches between the std declarations and one runtime's definitions.
fn mismatches(defs: &BTreeMap<String, Vec<(Sig, PathBuf)>>, required: bool) -> Vec<String> {
    let mut out = vec![];
    for (name, (want, velt_path)) in velt_declarations() {
        let Some(found) = defs.get(&name) else {
            if required {
                out.push(format!(
                    "{name}: declared in {}, not defined",
                    short(&velt_path)
                ));
            }
            continue;
        };
        for (got, rust_path) in found {
            if *got != want {
                out.push(format!(
                    "{name}: std {} ({}) vs runtime {} ({})",
                    render(&want),
                    short(&velt_path),
                    render(got),
                    short(rust_path)
                ));
            }
        }
    }
    out
}

#[test]
fn std_declarations_match_velt_rt() {
    let defs = rust_definitions(&runtime_sources("velt_rt"));
    let bad = mismatches(&defs, true);
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// velt_rt_wasm compiles velt_rt's portable modules from their sources (checked above) and
/// defines the rest itself; symbols it leaves out are not linkable on wasm at all.
#[test]
fn std_declarations_match_velt_rt_wasm() {
    let own: Vec<PathBuf> = runtime_sources("velt_rt_wasm");
    let defs = rust_definitions(&own);
    let bad = mismatches(&defs, false);
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn the_parsers_see_the_whole_abi() {
    let decls = velt_declarations();
    assert!(decls.len() > 150, "found only {} declarations", decls.len());
    let (params, ret) = &decls["velt_rt_fs_read_file"].0;
    assert_eq!(
        (params.as_slice(), ret.as_str()),
        (&["ptr".to_string()][..], "ptr")
    );
    let defs = rust_definitions(&runtime_sources("velt_rt"));
    assert!(defs.len() > 200, "found only {} definitions", defs.len());
}
