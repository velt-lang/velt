//! The runtime externs the hooks replace (`Hook::replaces`, std_hooks.rs) and the benchmarks
//! that measure them (`Hook::bench`): each named extern is one the runtime defines, each
//! benchmark exists, and the compiler names a replaced extern only where [`ALLOWED`] lists it
//! (#2197). Each later change that deletes a fallback deletes its entries here: an entry whose
//! uses are gone fails too, so the list only shrinks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use velt_common::std_hooks::HOOKS;

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
    ("velt_vir/src/lower/rt.rs", "velt_rt_str_from_i64", 3, DECLARED),
    ("velt_vir/src/lower/rt.rs", "velt_rt_str_from_u64", 3, DECLARED),
    ("velt_vir/src/lower/rt.rs", "velt_rt_str_from_f64", 3, DECLARED),
    ("velt_vir/src/lower/rt.rs", "velt_rt_strbuf_push_i64", 3, DECLARED),
    ("velt_vir/src/lower/rt.rs", "velt_rt_strbuf_push_u64", 3, DECLARED),
    ("velt_vir/src/lower/rt.rs", "velt_rt_strbuf_push_f64", 3, DECLARED),
    // The fallbacks when a number hook is missing or unchecked.
    ("velt_vir/src/lower/template.rs", "velt_rt_str_from_i64", 1, FALLBACK),
    ("velt_vir/src/lower/template.rs", "velt_rt_str_from_u64", 1, FALLBACK),
    ("velt_vir/src/lower/template.rs", "velt_rt_str_from_f64", 1, FALLBACK),
    ("velt_vir/src/lower/strbuf.rs", "velt_rt_strbuf_push_i64", 1, FALLBACK),
    ("velt_vir/src/lower/strbuf.rs", "velt_rt_strbuf_push_u64", 1, FALLBACK),
    ("velt_vir/src/lower/strbuf.rs", "velt_rt_strbuf_push_f64", 1, FALLBACK),
    // Emitters with no hook in between.
    ("velt_vir/src/lower/strbuf.rs", "velt_rt_strbuf_push_i64", 1, DISCRIMINANT),
    ("velt_vir/src/lower/glue/literals.rs", "velt_rt_strbuf_push_f64", 1, FLOAT_LITERAL),
    ("velt_vir/src/lower/any_member.rs", "velt_rt_str_from_f64", 1, NUMERIC_KEY),
    ("velt_vir/src/lower/any_has.rs", "velt_rt_str_from_f64", 1, NUMERIC_KEY),
    ("velt_vir/src/lower/any_proxy.rs", "velt_rt_str_from_f64", 1, NUMERIC_KEY),
    ("velt_vir/src/lower/proxy_throws.rs", "velt_rt_str_from_f64", 1, NUMERIC_KEY),
    ("velt_vir/src/lower/any_has_builtin.rs", "velt_rt_str_from_u64", 1, NUMERIC_KEY),
    ("velt_vir/src/lower/any_keys.rs", "velt_rt_str_from_u64", 1, INDEX_KEYS),
    ("velt_vir/src/lower/array/builtins.rs", "velt_rt_str_from_u64", 2, OUT_OF_BOUNDS),
    ("velt_vir/src/lower/array/builtins.rs", "velt_rt_str_from_i64", 1, OUT_OF_BOUNDS),
    // The optimizer's knowledge of the externs, which goes with their last call.
    ("velt_opt/src/numrep/print.rs", "velt_rt_strbuf_push_f64", 1, NUMREP),
    ("velt_opt/src/numrep/print.rs", "velt_rt_strbuf_push_i64", 1, NUMREP),
    ("velt_opt/src/numrep/print.rs", "velt_rt_str_from_f64", 1, NUMREP),
    ("velt_opt/src/numrep/print.rs", "velt_rt_str_from_i64", 1, NUMREP),
    ("velt_opt/src/borrowed_copies/counted/mod.rs", "velt_rt_strbuf_push_i64", 1, NO_FREE),
    ("velt_opt/src/borrowed_copies/counted/mod.rs", "velt_rt_strbuf_push_u64", 1, NO_FREE),
    ("velt_opt/src/borrowed_copies/counted/mod.rs", "velt_rt_strbuf_push_f64", 1, NO_FREE),
];

const DECLARED: &str = "the `Rt` call's declaration and its symbol";
const FALLBACK: &str = "fallback when the hook is unchecked, #2197";
const DISCRIMINANT: &str = "an enum's discriminant as text, no hook yet: #2197 PR 4";
const FLOAT_LITERAL: &str = "`console.log` of a float literal, no hook yet: #2197 PR 4";
const NUMERIC_KEY: &str = "a numeric key of an `any` as text, no hook yet: #2197 PR 4";
const INDEX_KEYS: &str = "an array's index keys as text, no hook yet: #2197 PR 4";
const OUT_OF_BOUNDS: &str = "the out-of-bounds message, no hook yet: #2197 PR 4";
const NUMREP: &str = "numrep rewrites a call of the float extern into the integer one";
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

#[test]
fn every_bench_row_exists() {
    let rows = crates().join("../bench/stdport");
    let mut benched = 0;
    for h in HOOKS {
        let Some(row) = h.bench else { continue };
        benched += 1;
        let file = rows.join(format!("{row}.vlt"));
        assert!(
            file.is_file(),
            "{}: bench(\"{row}\") names no bench/stdport/{row}.vlt",
            h.name
        );
    }
    assert!(benched > 0, "no hook names its benchmark");
}

/// Each replaced extern is a runtime function the compiler could call: velt_rt defines it as
/// `extern "C" fn <name>(` (what a program links against; std's `declare function`s only
/// declare what the runtime defines).
#[test]
fn every_replaced_extern_is_defined_by_the_runtime() {
    let mut rt = vec![];
    sources(&crates().join("velt_rt/src"), &mut rt);
    let names = replaced();
    assert!(!names.is_empty(), "no hook replaces an extern");
    for name in names {
        assert!(
            name.starts_with("velt_rt_"),
            "`{name}` is no runtime extern"
        );
        let def = format!("extern \"C\" fn {name}(");
        assert!(
            rt.iter().any(|(_, src)| src.contains(&def)),
            "replaces(\"{name}\"): velt_rt defines no `{def}`"
        );
    }
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
