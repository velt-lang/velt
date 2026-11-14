//! The std identity oracle (#2943 phase 0): a hash per checked std definition that is the same
//! in every program exactly when the definition's checked form is. Separate compilation needs
//! std's checked form to depend on std alone; comparing these hashes between `velt check --std`
//! and the programs that use std finds every definition that still depends on its program.
//!
//! A definition is hashed from its checked HIR's debug form, with what differs between two checks
//! of the same code made canonical: a `DefId` becomes the definition's name, a `TyId` the hash of
//! its type by structure ([`Hashing::ty`]), and a source position (`Span`) is left out (a file's
//! number depends on load order; positions inside std don't change between programs). Two hashes
//! are kept: the interface ([`Hashing::function`]: what callers see) and the whole definition, so
//! a report can tell a changed summary from a changed body.

use rustc_hash::FxHashMap;

use super::hashing::{finish, Hashing};
use crate::hir::{self, Def, DefId, TyId};

/// One checked std definition: its name, its interface hash (functions only, else 0) and the hash
/// of its whole checked form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StdDef {
    pub name: String,
    pub interface: u64,
    pub whole: u64,
}

/// The checked std definitions of `p` (by the `std/` prefix of their names), sorted by name. A
/// function lazy std left unchecked is not one.
pub fn std_defs(p: &hir::Program) -> Vec<StdDef> {
    let mut h = Hashing::new(p);
    let mut out = Vec::new();
    for (i, d) in p.defs.iter().enumerate() {
        let id = DefId(i as u32);
        let name = h.name(id);
        if !name.starts_with("std/") || p.unchecked.contains(&id) {
            continue;
        }
        let interface = match d {
            Def::Fn(f) => h.function(id, f),
            _ => 0,
        };
        let text = canonical(&mut h, &format!("{d:?}"));
        out.push(StdDef {
            name: name.to_string(),
            interface,
            whole: finish(text),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The canonical form [`std_defs`] hashes, of the checked std definitions whose names contain `part`
/// (`VELT_STD_SUMMARY_SHOW`): to see why one differs between two programs.
pub fn std_def_texts(p: &hir::Program, part: &str) -> Vec<(String, String)> {
    let mut h = Hashing::new(p);
    let mut out = Vec::new();
    for (i, d) in p.defs.iter().enumerate() {
        let id = DefId(i as u32);
        let name = h.name(id);
        if name.starts_with("std/") && name.contains(part) && !p.unchecked.contains(&id) {
            // The hashed (one-line) form, a field per line, to compare with `diff`.
            let text = canonical(&mut h, &format!("{d:?}")).replace(", ", ",\n");
            out.push((name.to_string(), text));
        }
    }
    out
}

/// `debug` with `DefId(n)` replaced by the definition's name, `TyId(n)` by its structural hash
/// and every `Span { … }` removed.
fn canonical(h: &mut Hashing<'_>, debug: &str) -> String {
    let mut out = String::with_capacity(debug.len());
    let mut tys: FxHashMap<u32, u64> = FxHashMap::default();
    let mut rest = debug;
    while let Some(at) = next_marker(rest) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("Span {") {
            // `Span { file: FileId(1), lo: 2, hi: 3 }`: no braces inside.
            let end = after.find('}').map_or(after.len(), |e| e + 1);
            rest = &after[end..];
        } else if let Some((n, after)) = number_in(tail, "DefId(") {
            out.push('@');
            out.push_str(h.name(DefId(n)));
            rest = after;
        } else if let Some((n, after)) = number_in(tail, "TyId(") {
            let hash = *tys.entry(n).or_insert_with(|| h.ty(TyId(n)));
            out.push_str(&format!("#{hash:x}"));
            rest = after;
        } else {
            // A marker's prefix without its number: keep one byte and go on.
            out.push_str(&tail[..1]);
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// The position of the next `Span {`, `DefId(` or `TyId(` in `s`.
fn next_marker(s: &str) -> Option<usize> {
    ["Span {", "DefId(", "TyId("]
        .iter()
        .filter_map(|m| s.find(m))
        .min()
}

/// `s` starting with `prefix`, a number and `)`: the number and what follows.
fn number_in<'s>(s: &'s str, prefix: &str) -> Option<(u32, &'s str)> {
    let after = s.strip_prefix(prefix)?;
    let digits = after.bytes().take_while(u8::is_ascii_digit).count();
    let n = after[..digits].parse().ok()?;
    let close = after[digits..].strip_prefix(')')?;
    Some((n, close))
}

#[cfg(test)]
mod tests {
    use super::{next_marker, number_in};

    #[test]
    fn markers_and_numbers() {
        assert_eq!(next_marker("a TyId(3) DefId(1)"), Some(2));
        assert_eq!(next_marker("nothing"), None);
        assert_eq!(number_in("DefId(42) rest", "DefId("), Some((42, " rest")));
        assert_eq!(number_in("DefId(x)", "DefId("), None);
    }
}
