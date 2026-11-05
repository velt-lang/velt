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

/// The C libraries whose declarations passed the check on this target, per package root, with
/// the files of the package that check read (its headers, with what they include).
pub type Checked = BTreeMap<PathBuf, BTreeMap<String, Vec<PathBuf>>>;

/// Build-stamp `file` entries for the package files the checks in `checked` read: a build with
/// nothing changed isn't checked again, so a changed header has to change the stamp.
pub fn stamp_inputs(checked: &Checked, inputs: &mut Vec<String>) {
    for file in checked.values().flat_map(|l| l.values()).flatten() {
        let bytes = std::fs::read(file).unwrap_or_default();
        inputs.push(format!(
            "file {}\t{}",
            vpm::probe::content_hash(&bytes),
            vpm::relpath::absolute(file).display()
        ));
    }
}

/// Check the C declarations of `p` (std's, in `std_files`, aside) against their libraries'
/// headers for `target`. Errors go to `diags`; the libraries that passed are returned.
pub fn check(
    p: &hir::Program,
    sm: &SourceMap,
    graph: Option<&PackageGraph>,
    std_files: &HashSet<FileId>,
    target: &str,
    diags: &mut Diagnostics,
) -> Checked {
    let mut checked = Checked::new();
    let Some(graph) = graph else { return checked };
    let Some(zig_target) = zig_target(target) else {
        return checked;
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
        if let Some(read) =
            check_library(&root, &library, headers, &decls, zig_target, target, diags)
        {
            let own = read.into_iter().filter(|f| in_package(f, &root)).collect();
            checked.entry(root).or_default().insert(library, own);
        }
    }
    checked
}

/// Whether `file`, a path clang read, is in the package at `root`.
fn in_package(file: &Path, root: &Path) -> bool {
    file.starts_with(root) || root.canonicalize().is_ok_and(|r| file.starts_with(r))
}

/// Check `decls` of `library` against its `headers`, or find a check of them remembered: when
/// they pass, the files the check read.
#[allow(clippy::too_many_arguments)]
fn check_library(
    root: &Path,
    library: &str,
    headers: &[String],
    decls: &[Decl],
    zig_target: &str,
    target: &str,
    diags: &mut Diagnostics,
) -> Option<Vec<PathBuf>> {
    let first = decls[0].span;
    let home = match vpm::locations::velt_home() {
        Ok(home) => home,
        Err(e) => {
            diags.push(Diagnostic::error(e, first));
            return None;
        }
    };
    let key = cache_key(root, headers, decls, target);
    let remembered = home.join("cache").join("headers").join(&key);
    if let Some(read) = still_holds(&remembered) {
        return Some(read);
    }
    let parsed = velt_toolchain::zig::zig(&home)
        .and_then(|zig| parse(&zig, &home, root, headers, zig_target));
    let (parsed, read) = match parsed {
        Ok(parsed) => parsed,
        Err(e) => {
            diags.push(
                Diagnostic::error(
                    format!(
                        "the declarations of C library `{library}` can't be checked against its headers: {e}"
                    ),
                    first,
                )
                .with_note(
                    "a library without headers says so in package.vlt with `unchecked: true`",
                ),
            );
            return None;
        }
    };
    let windows = target.contains("windows");
    let before = diags.len();
    for d in decls {
        check_decl(d, &parsed, headers, windows, diags);
    }
    if diags.len() != before {
        return None;
    }
    // Remembered only when it passed; a failure to write it costs a check next time.
    remember(&remembered, &read);
    Some(read)
}

/// Remember at `remembered` that a check which read `read` passed. The file is replaced whole
/// (written beside it and renamed), so a torn write can't leave a list of fewer files, which
/// would still hold.
fn remember(remembered: &Path, read: &[PathBuf]) {
    if let Some(dir) = remembered.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Some(text) = remembered_text(read) {
        let _ = vpm::json_file::write_atomic(remembered, &text);
    }
}

/// What a remembered check holds: a digest and the path of each file its parse read, one per
/// line. `None` when one can't be read any more.
fn remembered_text(read: &[PathBuf]) -> Option<String> {
    let mut text = String::new();
    for file in read {
        let path = file.to_str()?;
        text.push_str(&format!("{}\t{path}\n", file_digest(file)?));
    }
    Some(text)
}

/// The files a check remembered at `remembered` read, when there is one and every one of them
/// is unchanged.
fn still_holds(remembered: &Path) -> Option<Vec<PathBuf>> {
    let text = std::fs::read_to_string(remembered).ok()?;
    let read: Option<Vec<PathBuf>> = (text.lines())
        .map(|line| {
            let (digest, file) = line.split_once('\t')?;
            (file_digest(Path::new(file)).as_deref() == Some(digest)).then(|| PathBuf::from(file))
        })
        .collect();
    read.filter(|read| !read.is_empty())
}

/// The SHA-256 of a file's bytes, hex.
fn file_digest(file: &Path) -> Option<String> {
    let bytes = std::fs::read(file).ok()?;
    let digest = Sha256::digest(bytes);
    Some(digest.iter().map(|b| format!("{b:02x}")).collect())
}

fn check_decl(d: &Decl, h: &Headers, headers: &[String], windows: bool, diags: &mut Diagnostics) {
    let long_bits = if windows { 32 } else { 64 };
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
    if let Some(convention) = compare::foreign_convention(&c.attributes, windows) {
        diags.push(
            Diagnostic::error(
                format!(
                    "C function `{}` is `__attribute__(({convention}))` in {}, a calling convention `declare c` doesn't call",
                    d.name, c.at
                ),
                d.span,
            )
            .with_note("`declare c` calls with the target's C calling convention"),
        );
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
/// version, the target, the package's root, the headers' names and the declarations. What it
/// holds says whether the files parsed have changed ([`still_holds`]).
fn cache_key(root: &Path, headers: &[String], decls: &[Decl], target: &str) -> String {
    let mut h = Sha256::new();
    h.update(velt_toolchain::zig::VERSION);
    h.update([0]);
    h.update(target);
    h.update([0]);
    h.update(root.to_string_lossy().as_bytes());
    for header in headers {
        h.update([0]);
        h.update(header);
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

/// Parse `headers` (of the package at `root`) with zig's clang for `zig_target`: what they
/// declare, and every file the parse read.
fn parse(
    zig: &Path,
    home: &Path,
    root: &Path,
    headers: &[String],
    zig_target: &str,
) -> Result<(Headers, Vec<PathBuf>), String> {
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
) -> Result<(Headers, Vec<PathBuf>), String> {
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
    full.extend(
        [
            "-dependency-file",
            DEPENDENCIES,
            "-MT",
            "deps",
            "-sys-header-deps",
        ]
        .map(str::to_string),
    );
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
    let dependencies = std::fs::read_to_string(dir.join(DEPENDENCIES))
        .map_err(|e| format!("clang wrote no list of the files it read: {e}"))?;
    Ok((Headers::from_json(&json), files_read(dir, &dependencies)))
}

/// Where clang writes the files it read, in the parse's directory.
const DEPENDENCIES: &str = "deps.d";

/// The files a parse in `dir` read, from clang's dependency list: its own `headers.c` aside.
fn files_read(dir: &Path, dependencies: &str) -> Vec<PathBuf> {
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    dependency_paths(dependencies)
        .into_iter()
        .map(|file| dir.join(file))
        .filter(|file| !file.starts_with(dir) && !file.starts_with(&canonical))
        .collect()
}

/// The paths of clang's dependency list `deps: a.h b\\ c.h \\\n d.h`: separated by white space,
/// with `\\ `, `\\#` and `$$` for what make would read otherwise. Other backslashes are a Windows
/// path's.
fn dependency_paths(list: &str) -> Vec<String> {
    let list = list.strip_prefix("deps:").unwrap_or(list);
    let mut paths = Vec::new();
    let mut path = String::new();
    let mut chars = list.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('\\', Some(' ' | '#')) | ('$', Some('$')) => path.extend(chars.next()),
            ('\\', Some('\n' | '\r')) => {}
            (c, _) if c.is_whitespace() => {
                if !path.is_empty() {
                    paths.push(std::mem::take(&mut path));
                }
            }
            (c, _) => path.push(c),
        }
    }
    if !path.is_empty() {
        paths.push(path);
    }
    paths
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
    fn clang_s_dependency_list_gives_the_files_read() {
        assert_eq!(
            dependency_paths("deps: headers.c /p/a\\ b.h \\\n  /p/c\\#d.h \\\r\n C:\\z\\e$$.h\n"),
            ["headers.c", "/p/a b.h", "/p/c#d.h", "C:\\z\\e$.h"]
        );
        // A file outside the parse's directory, as an absolute path of this host (a drive
        // letter on Windows), with spaces escaped as clang does.
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().parent().unwrap().join("a.h");
        let listed = outside.to_str().unwrap().replace(' ', "\\ ");
        assert_eq!(
            files_read(dir.path(), &format!("deps: headers.c {listed}\n")),
            [outside]
        );
    }

    /// A remembered check holds while every file it read is the same; an older entry, without
    /// the files, doesn't.
    #[test]
    fn a_remembered_check_holds_while_its_files_do() {
        let dir = tempfile::tempdir().unwrap();
        let (outer, inner) = (dir.path().join("t.h"), dir.path().join("inner.h"));
        std::fs::write(&outer, "#include \"inner.h\"\n").unwrap();
        std::fs::write(&inner, "int f(int a);\n").unwrap();
        let remembered = dir.path().join("remembered");
        assert_eq!(still_holds(&remembered), None);
        let read = [outer, inner.clone()];
        remember(&remembered, &read);
        assert_eq!(still_holds(&remembered).as_deref(), Some(&read[..]));
        std::fs::write(&inner, "int f(long a);\n").unwrap();
        assert_eq!(still_holds(&remembered), None);
        std::fs::remove_file(&inner).unwrap();
        assert_eq!(still_holds(&remembered), None);
        std::fs::write(&remembered, "checked\n").unwrap();
        assert_eq!(still_holds(&remembered), None);
        std::fs::write(&remembered, "").unwrap();
        assert_eq!(still_holds(&remembered), None);
    }

    /// Remembering replaces the file rather than writing into it: what another name of the old
    /// file reads stays whole (as a reader in the middle of the write would see it).
    #[test]
    fn remembering_replaces_the_file_whole() {
        let dir = tempfile::tempdir().unwrap();
        let header = dir.path().join("a.h");
        std::fs::write(&header, "int f(int a);\n").unwrap();
        let remembered = dir.path().join("remembered");
        std::fs::write(&remembered, "old\n").unwrap();
        let other_name = dir.path().join("other");
        std::fs::hard_link(&remembered, &other_name).unwrap();
        remember(&remembered, std::slice::from_ref(&header));
        assert_eq!(std::fs::read_to_string(&other_name).unwrap(), "old\n");
        assert_eq!(still_holds(&remembered), Some(vec![header]));
    }

    /// What the stamp gets: the package files each check read, by digest.
    #[test]
    fn the_stamp_has_the_package_files_a_check_read() {
        let dir = tempfile::tempdir().unwrap();
        let header = dir.path().join("own.h");
        std::fs::write(&header, "int f(int a);\n").unwrap();
        let mut checked = Checked::new();
        checked
            .entry(dir.path().to_path_buf())
            .or_default()
            .insert("own".into(), vec![header.clone()]);
        let mut inputs = Vec::new();
        stamp_inputs(&checked, &mut inputs);
        let hash = vpm::probe::content_hash(b"int f(int a);\n");
        let path = vpm::relpath::absolute(&header);
        assert_eq!(inputs, [format!("file {hash}\t{}", path.display())]);
        assert!(in_package(&header, dir.path()));
        assert!(!in_package(Path::new("/z/lib/stdio.h"), dir.path()));
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
