//! The runtime externs the hooks replace (`Hook::replaces`, std_hooks.rs) and the benchmarks
//! that measure them (`Hook::bench`): each named extern is one the runtime defines or one listed
//! as deleted (`std_hooks::DELETED`, named nowhere any more), each benchmark exists, and the compiler names a replaced extern only where [`ALLOWED`] lists it
//! (#2197). Each later change that deletes a fallback deletes its entries here: an entry whose
//! uses are gone fails too, so the list only shrinks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use velt_common::std_hooks::{DELETED, HOOKS};

/// The crates whose sources make code (sema writes calls, lowering emits them, the optimizer
/// rewrites them, the backends compile them).
const EMITTERS: &[&str] = &[
    "velt_sema",
    "velt_vir",
    "velt_opt",
    "velt_codegen_llvm",
    "velt_codegen_cl",
];

/// The places that still name a replaced extern: (file under `crates/`, extern, uses, why). A
/// use is the extern's name as a string literal, or its `Rt` variant (velt_vir `lower/rt.rs`,
/// where lowering names a runtime call). Several entries of one file and extern add up.
#[rustfmt::skip]
const ALLOWED: &[(&str, &str, usize, &str)] = &[
    // The runtime calls lowering can emit (`Rt` and its symbols).
    ("velt_vir/src/lower/rt.rs", "velt_rt_str_from_f64", 3, DECLARED),
    ("velt_vir/src/lower/rt.rs", "velt_rt_strbuf_push_f64", 3, DECLARED),
    // The fallbacks when a float hook is missing or unchecked.
    ("velt_vir/src/lower/template.rs", "velt_rt_str_from_f64", 1, FALLBACK),
    ("velt_vir/src/lower/strbuf.rs", "velt_rt_strbuf_push_f64", 1, FALLBACK),
    // Emitters with no hook in between.
    ("velt_vir/src/lower/glue/literals.rs", "velt_rt_strbuf_push_f64", 1, FLOAT_LITERAL),
    ("velt_vir/src/lower/any_member.rs", "velt_rt_str_from_f64", 1, NUMERIC_KEY),
    ("velt_vir/src/lower/any_has.rs", "velt_rt_str_from_f64", 1, NUMERIC_KEY),
    ("velt_vir/src/lower/any_proxy.rs", "velt_rt_str_from_f64", 1, NUMERIC_KEY),
    ("velt_vir/src/lower/proxy_throws.rs", "velt_rt_str_from_f64", 1, NUMERIC_KEY),
    // The optimizer's knowledge of the externs, which goes with their last call.
    ("velt_opt/src/borrowed_copies/counted/mod.rs", "velt_rt_strbuf_push_f64", 1, NO_FREE),
];

const DECLARED: &str = "the `Rt` call's declaration and its symbol";
const FALLBACK: &str = "fallback when the float hook is unchecked: #2161";
const FLOAT_LITERAL: &str = "`console.log` of a float literal, no hook yet: #2197 PR 4";
const NUMERIC_KEY: &str = "a numeric key of an `any` as text, no hook yet: #2197 PR 4";
const NO_FREE: &str = "the externs whose call frees nothing (borrowed copies)";

fn crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Every extern some hook replaces, once.
fn replaced() -> Vec<&'static str> {
    let mut names: Vec<&str> = HOOKS.iter().flat_map(|h| h.replaces).copied().collect();
    names.sort();
    names.dedup();
    names
}

/// The Rust files under `dir` that are no tests (a `tests` directory, a file named for tests),
/// each with its path under `crates/` and its text without the items marked `#[cfg(test)]`.
fn sources(dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("source directory") {
        let path = entry.expect("source entry").path();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let name = name.unwrap_or_default();
        if path.is_dir() {
            if name != "tests" {
                sources(&path, out);
            }
            continue;
        }
        if !name.ends_with(".rs") || name.contains("tests") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("source file");
        let src = without_tests(&src);
        let rel = path.strip_prefix(crates()).expect("under crates/");
        let rel = rel.to_string_lossy().replace('\\', "/");
        out.push((rel, src));
    }
}

/// `src` with each item marked `#[cfg(test)]` (on a line of its own) blanked out, its lines
/// kept: the item ends at the first `;` outside brackets or at the `}` that closes its first `{`
/// (a `mod tests { … }`, a `mod tests;`, a `use`, a function). String and character literals
/// and comments are skipped while counting.
///
/// A small hand scanner, not a Rust lexer: it handles strings, characters and line comments, but
/// not raw strings (`r#"…"#`) or block comments holding braces, so a test item containing one
/// can end early and expose test text to the scan: a false failure, never a false pass.
fn without_tests(src: &str) -> String {
    const MARK: &str = "#[cfg(test)]";
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    // The attribute on a line of its own, not a mention in a comment or a string.
    let attribute = |s: &str| {
        s.match_indices(MARK).map(|(i, _)| i).find(|&i| {
            s[..i]
                .rsplit('\n')
                .next()
                .is_some_and(|l| l.trim().is_empty())
        })
    };
    while let Some(at) = attribute(rest) {
        out.push_str(&rest[..at]);
        let item = &rest[at..];
        let end = item_end(item, MARK.len());
        out.extend(
            item[..end]
                .chars()
                .map(|c| if c == '\n' { '\n' } else { ' ' }),
        );
        rest = &item[end..];
    }
    out.push_str(rest);
    out
}

/// The byte offset just past the item that starts at `from` in `s` ([`without_tests`]).
fn item_end(s: &str, from: usize) -> usize {
    let b = s.as_bytes();
    let (mut i, mut depth) = (from, 0usize);
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'\'' if b.get(i + 2) == Some(&b'\'') => i += 2,
            b'\'' if b.get(i + 1) == Some(&b'\\') => {
                i += 2;
                while i < b.len() && b[i] != b'\'' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            b';' if depth == 0 => return i + 1,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// The `Rt` variant lowering names extern `name` by: the one on the line of `lower/rt.rs` that
/// spells the symbol (`Rt::StrFromI64 => ("velt_rt_str_from_i64", …)`).
fn rt_variant(rt: &str, name: &str) -> Option<String> {
    let line = rt.lines().find(|l| l.contains(&format!("\"{name}\"")))?;
    let at = line.find("Rt::")? + 4;
    let variant: String = line[at..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    Some(variant)
}

/// The byte offsets of `word` in `src` as a whole identifier.
fn word_uses<'a>(src: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    let ident = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    src.match_indices(word).map(|(i, _)| i).filter(move |&i| {
        !ident(src[..i].chars().next_back()) && !ident(src[i + word.len()..].chars().next())
    })
}

/// The `bench/stdport/` rows of ports that have no hook, because only std called the Rust they
/// replaced, so `HOOKS` can't name them.
const PORTS_WITHOUT_A_HOOK: [&str; 1] = ["prng"];

/// The warm-up stores what debug builds link (`velt cache warm`, std_cache/serve.rs), and prebuilt
/// objects will be what it reaches: a port not on its list runs unoptimized in debug builds, as
/// `localeCompare` did (+163% against the Rust). A port's benchmark exercises it, so it goes there.
#[test]
fn every_ported_hook_is_warmed() {
    let list = crates().join("../share/velt/warm/programs.txt");
    let list = std::fs::read_to_string(list).expect("the warm-up list");
    let hooked = HOOKS.iter().filter_map(|h| Some((h.name, h.bench?)));
    let unhooked = PORTS_WITHOUT_A_HOOK.iter().map(|&row| (row, row));
    for (name, row) in hooked.chain(unhooked) {
        let line = format!("../../../bench/stdport/{row}.velt");
        assert!(
            list.lines().any(|l| l.trim() == line),
            "{name}: share/velt/warm/programs.txt has no `{line}`, so debug builds would compile \
             this port unoptimized",
        );
    }
}

#[test]
fn every_bench_row_exists() {
    let rows = crates().join("../bench/stdport");
    let mut benched = 0;
    for h in HOOKS {
        let Some(row) = h.bench else { continue };
        benched += 1;
        let file = rows.join(format!("{row}.velt"));
        assert!(
            file.is_file(),
            "{}: bench(\"{row}\") names no bench/stdport/{row}.velt",
            h.name
        );
    }
    assert!(benched > 0, "no hook names its benchmark");
}

/// Each replaced extern is a runtime function the compiler could call: velt_rt defines it as
/// `extern "C" fn <name>(` (what a program links against; std's `declare function`s only
/// declare what the runtime defines), unless the port is done and its Rust deleted
/// (`std_hooks::DELETED`).
#[test]
fn every_replaced_extern_is_defined_by_the_runtime_or_deleted() {
    let mut rt = vec![];
    sources(&crates().join("velt_rt/src"), &mut rt);
    let names = replaced();
    assert!(!names.is_empty(), "no hook replaces an extern");
    for name in &names {
        assert!(
            name.starts_with("velt_rt_"),
            "`{name}` is no runtime extern"
        );
        let def = format!("extern \"C\" fn {name}(");
        let defined = rt.iter().any(|(_, src)| src.contains(&def));
        if DELETED.contains(name) {
            assert!(
                !defined,
                "replaces(\"{name}\"): velt_rt defines it, but std_hooks::DELETED lists it"
            );
        } else {
            assert!(
                defined,
                "replaces(\"{name}\"): velt_rt defines no `{def}` (deleted: list it in \
                 std_hooks::DELETED)"
            );
        }
    }
    // An extern only std called is deleted with no hook replacing it, so the check above doesn't
    // see it.
    for name in DELETED {
        assert!(
            name.starts_with("velt_rt_"),
            "std_hooks::DELETED lists `{name}`, no runtime extern"
        );
        let def = format!("extern \"C\" fn {name}(");
        assert!(
            !rt.iter().any(|(_, src)| src.contains(&def)),
            "velt_rt defines `{name}`, but std_hooks::DELETED lists it"
        );
    }
}

/// The text files under `dir` (recursively) with extension `ext`, skipping `target` folders.
fn files_with(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_none_or(|n| n != "target") {
                files_with(&path, ext, out);
            }
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

/// A deleted extern is gone for good: no Rust source of any crate (tests included: a test
/// harness defining its own copy would hide a call that slipped back) and no std file names it,
/// so a program cannot reach it and a link cannot need it.
#[test]
fn a_deleted_extern_is_named_nowhere() {
    let mut files = vec![];
    files_with(&crates(), "rs", &mut files);
    files_with(&crates().join("../std"), "velt", &mut files);
    let this = Path::new(file!()).file_name().expect("this file");
    let mut wrong = vec![];
    for path in files {
        if path.ends_with(Path::new("velt_common/src/std_hooks.rs"))
            || path.file_name() == Some(this)
        {
            continue; // The table and this test name them as history.
        }
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        for name in DELETED {
            for (i, line) in src.lines().enumerate() {
                if word_uses(line, name).next().is_some() {
                    wrong.push(format!("  {}:{}: {}", path.display(), i + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "a deleted runtime extern (std_hooks::DELETED) is named:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn the_compiler_names_a_replaced_extern_only_where_allowed() {
    let mut files = vec![];
    for c in EMITTERS {
        sources(&crates().join(c).join("src"), &mut files);
    }
    let rt = std::fs::read_to_string(crates().join("velt_vir/src/lower/rt.rs")).expect("rt.rs");
    // (file, extern) -> the lines that name it.
    let mut found: BTreeMap<(String, &str), Vec<String>> = BTreeMap::new();
    for name in replaced() {
        let literal = format!("\"{name}\"");
        let variant = rt_variant(&rt, name);
        for (rel, src) in &files {
            let mut at: Vec<usize> = src.match_indices(&literal).map(|(i, _)| i).collect();
            if let Some(v) = &variant {
                at.extend(word_uses(src, v));
            }
            for i in at {
                let line = src[..i].matches('\n').count() + 1;
                let text = src.lines().nth(line - 1).unwrap_or_default().trim();
                found
                    .entry((rel.clone(), name))
                    .or_default()
                    .push(format!("  {rel}:{line}: {text}"));
            }
        }
    }
    let mut allowed: BTreeMap<(String, &str), usize> = BTreeMap::new();
    for (file, name, uses, _) in ALLOWED {
        *allowed.entry((file.to_string(), *name)).or_default() += uses;
    }
    let mut wrong = vec![];
    for (key, lines) in &found {
        let want = allowed.get(key).copied().unwrap_or(0);
        if lines.len() > want {
            wrong.push(format!(
                "{} names `{}` {} times, {want} allowed: call the hook that replaces it \
                 (velt_common std_hooks.rs) instead\n{}",
                key.0,
                key.1,
                lines.len(),
                lines.join("\n")
            ));
        }
    }
    for (key, want) in &allowed {
        let have = found.get(key).map_or(0, Vec::len);
        if have < *want {
            wrong.push(format!(
                "{} names `{}` {have} times, ALLOWED lists {want}: remove the stale entries",
                key.0, key.1
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "crates/velt_common/tests/hook_replaces.rs ALLOWED:\n{}",
        wrong.join("\n")
    );
}

/// The runtime's exports, one `<crate> <name>` per line, sorted (#2196: the runtime only
/// shrinks): what the runtime may define with `#[no_mangle]` or `#[export_name]`.
const EXPORTS: &str = include_str!("runtime_exports.txt");

/// `src` as bytes with its comments blanked to spaces and the insides of its string and
/// character literals to `x` (newlines kept), so that each byte stays where it was in `src` and
/// an attribute or keyword found in it is code, not text.
fn code_only(src: &str) -> Vec<u8> {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize, with: u8| {
        for c in &mut out[from..to.min(b.len())] {
            if *c != b'\n' {
                *c = with;
            }
        }
    };
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let end = src[i..].find('\n').map_or(b.len(), |n| i + n);
                blank(&mut out, i, end, b' ');
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                // Block comments nest in Rust.
                let (mut depth, mut j) = (0usize, i);
                while j < b.len() {
                    if b[j..].starts_with(b"/*") {
                        depth += 1;
                        j += 2;
                    } else if b[j..].starts_with(b"*/") {
                        depth -= 1;
                        j += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        j += 1;
                    }
                }
                blank(&mut out, i, j, b' ');
                i = j;
            }
            b'r' if matches!(b.get(i + 1), Some(b'#' | b'"')) && (i == 0 || !ident(b[i - 1])) => {
                // A raw string `r#"…"#`; a raw identifier `r#name` has no quote.
                let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
                if b.get(i + 1 + hashes) != Some(&b'"') {
                    i += 1;
                    continue;
                }
                let open = i + 2 + hashes;
                let close: Vec<u8> = std::iter::once(b'"')
                    .chain(std::iter::repeat_n(b'#', hashes))
                    .collect();
                let end = b[open..]
                    .windows(close.len())
                    .position(|w| w == close.as_slice())
                    .map_or(b.len(), |n| open + n);
                blank(&mut out, open, end, b'x');
                i = end + close.len();
            }
            b'"' => {
                let mut j = i + 1;
                while j < b.len() && b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                blank(&mut out, i + 1, j, b'x');
                i = j + 1;
            }
            // A character literal (`'"'`, `'\''`), not a lifetime (`'a`); the escaped character
            // may itself be a quote.
            b'\'' if b.get(i + 1) == Some(&b'\\') => {
                let close = b[(i + 3).min(b.len())..]
                    .iter()
                    .position(|&c| c == b'\'')
                    .map_or(b.len(), |n| i + 3 + n);
                blank(&mut out, i + 1, close, b'x');
                i = close + 1;
            }
            b'\'' if b.get(i + 2) == Some(&b'\'') => {
                out[i + 1] = b'x';
                i += 3;
            }
            _ => i += 1,
        }
    }
    out
}

/// Where `word` first occurs in `code` as a whole word, from `from` on.
fn find_word(code: &[u8], word: &str, from: usize) -> Option<usize> {
    let w = word.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    (from..(code.len() + 1).saturating_sub(w.len())).find(|&i| {
        code[i..].starts_with(w)
            && (i == 0 || !ident(code[i - 1]))
            && code.get(i + w.len()).is_none_or(|&c| !ident(c))
    })
}

/// The symbols `src` exports. An attribute that names `no_mangle` anywhere in it
/// (`#[no_mangle]`, `#[unsafe(no_mangle)]`, `#[cfg_attr(not(test), no_mangle)]`, on a line of its
/// own or on the item's) exports the name of the `fn` or `static` it is on; one that names
/// `export_name` exports the name it gives. Comments and literals don't count.
fn no_mangle_names(src: &str) -> Vec<String> {
    let code = code_only(src);
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let skip_space = |mut at: usize| {
        while code.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        at
    };
    let mut out = vec![];
    let mut at = 0;
    while let Some(n) = code[at..].windows(2).position(|w| w == b"#[") {
        let open = at + n;
        let line = src[..open].matches('\n').count() + 1;
        // The attribute ends at its matching `]` (literals are blanked, so brackets count).
        let mut depth = 0usize;
        let mut close = code.len();
        for (k, &c) in code.iter().enumerate().skip(open + 1) {
            if c == b'[' {
                depth += 1;
            } else if c == b']' {
                depth -= 1;
                if depth == 0 {
                    close = k;
                    break;
                }
            }
        }
        let attr = &code[open..close];
        if let Some(e) = find_word(attr, "export_name", 0) {
            let quote = attr[e..]
                .iter()
                .position(|&c| c == b'"')
                .unwrap_or_else(|| panic!("line {line}: `export_name` gives no name"));
            let from = open + e + quote + 1;
            let len = src[from..].find('"').expect("a closed string");
            out.push(src[from..from + len].to_string());
        } else if find_word(attr, "no_mangle", 0).is_some() {
            let (kw, kw_len) = ["fn", "static"]
                .iter()
                .filter_map(|kw| find_word(&code, kw, close).map(|i| (i, kw.len())))
                .min()
                .unwrap_or_else(|| panic!("line {line}: `no_mangle` on no fn or static"));
            let mut name = skip_space(kw + kw_len);
            if code[name..].starts_with(b"mut") && !code.get(name + 3).is_some_and(|&c| ident(c)) {
                name = skip_space(name + 3);
            }
            let len = code[name..].iter().take_while(|&&c| ident(c)).count();
            assert!(
                len > 0,
                "line {line}: `no_mangle` on an item with no plain name"
            );
            out.push(src[name..name + len].to_string());
        }
        at = close.max(open + 2);
    }
    out
}

#[test]
fn no_mangle_names_finds_each_form_of_export() {
    let names = no_mangle_names;
    assert_eq!(names("#[no_mangle]\npub extern \"C\" fn a() {}"), ["a"]);
    assert_eq!(
        names("#[unsafe(no_mangle)]\npub static mut B: u8 = 0;"),
        ["B"]
    );
    assert_eq!(
        names("#[no_mangle]\n#[inline(never)]\n/// Doc.\npub unsafe extern \"C\" fn c() {}"),
        ["c"]
    );
}

#[test]
fn no_mangle_names_finds_no_mangle_on_the_items_line() {
    assert_eq!(
        no_mangle_names("#[no_mangle] pub extern \"C\" fn d() {}"),
        ["d"]
    );
}

#[test]
fn no_mangle_names_finds_no_mangle_inside_cfg_attr() {
    let src = "#[cfg_attr(not(test), no_mangle)]\npub extern \"C\" fn e() {}";
    assert_eq!(no_mangle_names(src), ["e"]);
}

#[test]
fn no_mangle_names_finds_export_name() {
    let src = "#[export_name = \"f_sym\"]\npub extern \"C\" fn f() {}";
    assert_eq!(no_mangle_names(src), ["f_sym"]);
    let src = "#[unsafe(export_name = \"g_sym\")] extern \"C\" fn g() {}";
    assert_eq!(no_mangle_names(src), ["g_sym"]);
}

#[test]
fn no_mangle_names_skips_comments_and_literals() {
    let src = "//! Every export is `#[no_mangle] extern \"C\"`.\n\
               /* #[no_mangle] /* nested */ fn h() {} */\n\
               const S: &str = \"#[no_mangle] fn s\";\n\
               const R: &str = r#\"#[export_name = \"r\"]\"#;\n\
               const Q: char = '\"'; const E: char = '\\''; #[inline] fn plain() {}\n";
    assert!(
        no_mangle_names(src).is_empty(),
        "{:?}",
        no_mangle_names(src)
    );
}

/// Every symbol the runtime exports is in `runtime_exports.txt`, and every line there is one it
/// exports: a new export fails here (the runtime gets no new Rust, #2196: write it in Velt, or
/// add what Velt lacks to the language), and a port that deletes one deletes its line, so the
/// list only shrinks.
#[test]
fn the_runtime_exports_only_what_runtime_exports_lists() {
    let mut have = vec![];
    for krate in ["velt_rt", "velt_rt_wasm"] {
        let mut files = vec![];
        sources(&crates().join(krate).join("src"), &mut files);
        for (_, src) in &files {
            have.extend(
                no_mangle_names(src)
                    .into_iter()
                    .map(|n| format!("{krate} {n}")),
            );
        }
    }
    have.sort();
    have.dedup();
    let listed: Vec<&str> = EXPORTS.lines().filter(|l| !l.is_empty()).collect();
    let mut sorted = listed.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        listed, sorted,
        "runtime_exports.txt must be sorted, each line once"
    );
    let new: Vec<&String> = have
        .iter()
        .filter(|h| !listed.contains(&h.as_str()))
        .collect();
    assert!(
        new.is_empty(),
        "the runtime exports symbols runtime_exports.txt doesn't list: {new:?}. The runtime gets \
         no new Rust (#2196): write it in Velt, or file what Velt lacks"
    );
    let gone: Vec<&&str> = listed
        .iter()
        .filter(|l| !have.iter().any(|h| h == **l))
        .collect();
    assert!(
        gone.is_empty(),
        "runtime_exports.txt lists symbols the runtime no longer exports: {gone:?}. Delete \
         their lines"
    );
}
