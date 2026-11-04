//! Checking `declare c` functions against their library's C headers
//! (docs/internals/design/c-ffi.md "Checking declarations against C headers"): for a package
//! built here (the program's own, a path dependency, a workspace member) whose C library lists
//! `headers` in package.vlt, the pinned zig's clang parses them for the build's target, and
//! each declaration must name a function they declare, with parameters and a result that
//! match. A mismatch is an error naming the header line. A result that passed is remembered
//! (`$VELT_HOME/cache/headers/`), so clang runs again only when a header, a declaration, the
//! target or zig changes. Registry and git packages were checked where they were published.

mod compare;
mod ctype;
mod shape;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use velt_common::{Diagnostic, Diagnostics, FileId, SourceMap, Span};
use velt_sema::hir::{self, Def};
use vpm::graph::GraphPackage;
use vpm::PackageGraph;

use compare::Shape;
use ctype::Headers;
use shape::shape;

/// One `declare c` function to check: its name in C, what it passes, where it is.
struct Decl {
    name: String,
    params: Vec<Shape>,
    ret: Shape,
    span: Span,
}

/// Check the C declarations of `p` (std's, in `std_files`, aside) against their libraries'
/// headers for `target`. Errors go to `diags`.
pub fn check(
    p: &hir::Program,
    sm: &SourceMap,
    graph: Option<&PackageGraph>,
    std_files: &HashSet<FileId>,
    target: &str,
    diags: &mut Diagnostics,
) {
    let Some(graph) = graph else { return };
    let Some(zig_target) = zig_target(target) else {
        return;
    };
    // (package root, library) → its declarations.
    let mut groups: BTreeMap<(PathBuf, String), (&GraphPackage, Vec<Decl>)> = BTreeMap::new();
    for def in &p.defs {
        let Def::ExternFn(f) = def else { continue };
        let Some(c) = &f.c else { continue };
        if std_files.contains(&f.span.file) {
            continue;
        }
        let Some(package) = graph.package_of(&sm.get(f.span.file).path) else {
            continue;
        };
        let Some(lib) = package.c_libraries.get(&c.library) else {
            continue;
        };
        if package.fetched || lib.unchecked || lib.headers.is_empty() {
            continue;
        }
        let decl = Decl {
            name: velt_vir::system::base_name(&f.symbol).to_string(),
            params: f
                .params
                .iter()
                .zip(&c.params)
                .map(|(&t, &ct)| shape(p, t, ct))
                .collect(),
            ret: shape(p, f.ret, c.ret),
            span: f.span,
        };
        groups
            .entry((package.root.clone(), c.library.clone()))
            .or_insert_with(|| (package, Vec::new()))
            .1
            .push(decl);
    }
    for ((root, library), (package, decls)) in groups {
        let headers = &package.c_libraries[&library].headers;
        check_library(&root, &library, headers, &decls, zig_target, target, diags);
    }
}

#[allow(clippy::too_many_arguments)]
fn check_library(
    root: &Path,
    library: &str,
    headers: &[String],
    decls: &[Decl],
    zig_target: &str,
    target: &str,
    diags: &mut Diagnostics,
) {
    let first = decls[0].span;
    let home = match vpm::locations::velt_home() {
        Ok(home) => home,
        Err(e) => return diags.push(Diagnostic::error(e, first)),
    };
    let key = cache_key(root, headers, decls, target);
    let remembered = home.join("cache").join("headers").join(&key);
    if remembered.is_file() {
        return;
    }
    let parsed = velt_toolchain::zig::zig(&home)
        .and_then(|zig| parse(&zig, &home, root, headers, zig_target));
    let parsed = match parsed {
        Ok(h) => h,
        Err(e) => {
            return diags.push(
                Diagnostic::error(
                    format!(
                        "the declarations of C library `{library}` can't be checked against its headers: {e}"
                    ),
                    first,
                )
                .with_note(
                    "a library without headers says so in package.vlt with `unchecked: true`",
                ),
            )
        }
    };
    let long_bits = if target.contains("windows") { 32 } else { 64 };
    let before = diags.len();
    for d in decls {
        check_decl(d, &parsed, headers, long_bits, diags);
    }
    if diags.len() == before {
        // Remembered only when it passed; a failure to write it costs a check next time.
        let _ = std::fs::create_dir_all(remembered.parent().unwrap_or(&home));
        let _ = std::fs::write(&remembered, b"checked\n");
    }
}

fn check_decl(d: &Decl, h: &Headers, headers: &[String], long_bits: u32, diags: &mut Diagnostics) {
    let Some(c) = h.functions.get(&d.name) else {
        diags.push(Diagnostic::error(
            format!(
                "C function `{}` is not declared in its library's headers ({})",
                d.name,
                headers.join(", ")
            ),
            d.span,
        ));
        return;
    };
    if c.variadic {
        diags.push(Diagnostic::error(
            format!(
                "C function `{}` is variadic in {}, which `declare c` can't declare yet",
                d.name, c.at
            ),
            d.span,
        ));
        return;
    }
    if c.params.len() != d.params.len() {
        diags.push(Diagnostic::error(
            format!(
                "C function `{}` has {} parameters in {}; the declaration has {}",
                d.name,
                c.params.len(),
                c.at,
                d.params.len()
            ),
            d.span,
        ));
        return;
    }
    for (i, (shape, written)) in d.params.iter().zip(&c.params).enumerate() {
        if let Err(note) = compare::compare(shape, &h.resolve(written, long_bits)) {
            diags.push(
                Diagnostic::error(
                    format!(
                        "parameter {} of C function `{}` is `{written}` in {}, not the declaration's `{}`",
                        i + 1,
                        d.name,
                        c.at,
                        compare::describe(shape)
                    ),
                    d.span,
                )
                .with_note(note),
            );
        }
    }
    if let Err(note) = compare::compare(&d.ret, &h.resolve(&c.ret, long_bits)) {
        diags.push(
            Diagnostic::error(
                format!(
                    "C function `{}` returns `{}` in {}, not the declaration's `{}`",
                    d.name,
                    c.ret,
                    c.at,
                    compare::describe(&d.ret)
                ),
                d.span,
            )
            .with_note(note),
        );
    }
}

/// What the result remembered for these declarations and headers is filed under: zig's
/// version, the target, the headers' names and the package's own headers' bytes, and the
/// declarations.
fn cache_key(root: &Path, headers: &[String], decls: &[Decl], target: &str) -> String {
    let mut h = Sha256::new();
    h.update(velt_toolchain::zig::VERSION);
    h.update([0]);
    h.update(target);
    for header in headers {
        h.update([0]);
        h.update(header);
        if !header.starts_with('<') {
            h.update(std::fs::read(root.join(header)).unwrap_or_default());
        }
    }
    for d in decls {
        h.update(format!("\0{}{:?}{:?}", d.name, d.params, d.ret));
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The target as zig names it; `None` for one without C headers (WebAssembly). Windows
/// targets read zig's MinGW headers, which have the MSVC targets' type sizes.
fn zig_target(triple: &str) -> Option<&'static str> {
    Some(match triple {
        "aarch64-apple-darwin" => "aarch64-macos",
        "x86_64-apple-darwin" => "x86_64-macos",
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        t if t.starts_with("x86_64-pc-windows") => "x86_64-windows-gnu",
        t if t.starts_with("aarch64-pc-windows") => "aarch64-windows-gnu",
        _ => return None,
    })
}

/// Parse `headers` (of the package at `root`) with zig's clang for `zig_target`.
fn parse(
    zig: &Path,
    home: &Path,
    root: &Path,
    headers: &[String],
    zig_target: &str,
) -> Result<Headers, String> {
    let dir = work_dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let result = parse_in(zig, &dir, root, headers, zig_target);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// A directory for one parse, whoever else parses at the same time, in this process or another.
fn work_dir(home: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    home.join("cache")
        .join("headers")
        .join(format!("work-{}-{n}", std::process::id()))
}

/// The C file that includes `headers`. A package's headers go by their path in it, which `-I`
/// finds: the package's own path could hold what a `#include` reads differently (`\`, `"`).
fn includes(root: &Path, headers: &[String]) -> Result<String, String> {
    let mut source = String::new();
    for header in headers {
        if !header.starts_with('<') && !root.join(header).is_file() {
            return Err(format!("header `{header}` is not in the package"));
        }
        match header.starts_with('<') {
            true => source.push_str(&format!("#include {header}\n")),
            false => source.push_str(&format!("#include \"{header}\"\n")),
        }
    }
    Ok(source)
}

fn parse_in(
    zig: &Path,
    dir: &Path,
    root: &Path,
    headers: &[String],
    zig_target: &str,
) -> Result<Headers, String> {
    let source = includes(root, headers)?;
    let file = dir.join("headers.c");
    std::fs::write(&file, source).map_err(|e| e.to_string())?;
    let cache = dir.join("zig-cache");
    let command = |args: &[&str]| {
        let mut c = std::process::Command::new(zig);
        c.args(args)
            .current_dir(dir)
            .env("ZIG_GLOBAL_CACHE_DIR", &cache)
            .env("ZIG_LOCAL_CACHE_DIR", &cache);
        c
    };
    // `zig cc -###` prints the clang command it would run; that command with the AST dump in
    // place of the object is what clang parses the headers with (zig's own `-fsyntax-only`
    // still renames an object it never made).
    let include = format!("-I{}", root.display());
    let plan = command(&[
        "cc",
        "-target",
        zig_target,
        &include,
        "-c",
        "headers.c",
        "-o",
        "headers.o",
        "-###",
    ])
    .output()
    .map_err(|e| format!("running {}: {e}", zig.display()))?;
    let plan = String::from_utf8_lossy(&plan.stderr);
    let line = plan
        .lines()
        .rev()
        .find(|l| l.contains("\"-cc1\""))
        .ok_or("zig cc printed no clang command")?;
    let mut args = split_quoted(line);
    if args.len() < 2 || args[1] != "-cc1" {
        return Err("zig cc printed an unexpected clang command".into());
    }
    let mut cc1: Vec<String> = args.split_off(2);
    cc1.retain(|a| a != "-emit-obj");
    if let Some(i) = cc1.iter().position(|a| a == "-o") {
        cc1.drain(i..(i + 2).min(cc1.len()));
    }
    let mut full = vec!["clang".to_string(), "-cc1".to_string()];
    full.extend(cc1);
    full.push("-ast-dump=json".into());
    let refs: Vec<&str> = full.iter().map(String::as_str).collect();
    let out = command(&refs)
        .output()
        .map_err(|e| format!("running {}: {e}", zig.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let errors: Vec<&str> = stderr
            .lines()
            .filter(|l| l.contains("error"))
            .take(3)
            .collect();
        return Err(format!("clang can't parse them: {}", errors.join("; ")));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("clang's AST is not JSON: {e}"))?;
    Ok(Headers::from_json(&json))
}

/// The words of a command line clang printed: each in double quotes, with `\"` and `\\`.
fn split_quoted(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut word = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(n) = chars.next() {
                        word.push(n);
                    }
                }
                '"' => break,
                c => word.push(c),
            }
        }
        out.push(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clang_s_command_line_splits_into_words() {
        assert_eq!(
            split_quoted(r#" "/z/zig" "-cc1" "-I/a b" "-DX=\"y\"""#),
            ["/z/zig", "-cc1", "-I/a b", "-DX=\"y\""]
        );
    }

    #[test]
    fn each_parse_has_its_own_directory() {
        let home = Path::new("/h");
        assert_ne!(work_dir(home), work_dir(home));
    }

    #[test]
    fn a_package_s_headers_are_included_by_their_path_in_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("native")).unwrap();
        std::fs::write(dir.path().join("native/a.h"), "").unwrap();
        let headers = ["native/a.h".to_string(), "<string.h>".to_string()];
        assert_eq!(
            includes(dir.path(), &headers).unwrap(),
            "#include \"native/a.h\"\n#include <string.h>\n"
        );
        let missing = includes(dir.path(), &["native/b.h".to_string()]).unwrap_err();
        assert_eq!(missing, "header `native/b.h` is not in the package");
    }

    #[test]
    fn targets_map_to_zig_s() {
        assert_eq!(
            zig_target("x86_64-pc-windows-msvc"),
            Some("x86_64-windows-gnu")
        );
        assert_eq!(zig_target("aarch64-apple-darwin"), Some("aarch64-macos"));
        assert_eq!(zig_target("wasm32-wasip1"), None);
    }
}
