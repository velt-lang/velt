//! Abstract classes and methods (TypeScript's rules, docs/reference/classes.md): an abstract
//! method is a vtable slot whose body never runs; a concrete subclass implements it, with or
//! without `override`, and must implement every abstract member it inherits.

use velt_common::Diagnostic;

use super::classes::{base_of_def, has_default};
use crate::ctx::Ctx;
use crate::hir::DefId;

/// Record the abstract methods of `adts` in `cx.abstract_methods`.
pub(super) fn record(cx: &mut Ctx, adts: &[DefId]) {
    for &d in adts {
        let Some(a) = cx.adt(d) else { continue };
        let (Some(decl), module) = (a.decl, a.module) else {
            continue;
        };
        let defs: Vec<DefId> = (decl.methods.iter())
            .filter(|m| m.is_abstract)
            .filter_map(|m| {
                let key = cx.method_key(module, &m.decl.sig.name, m.is_setter);
                cx.adt(d)?.methods.get(&key).map(|r| r.def)
            })
            .collect();
        cx.abstract_methods.extend(defs);
    }
}

/// The instance method `key` class `d` inherits from its nearest ancestor declaring it (`None`
/// when it has none, or gets it as an interface default).
fn inherited(cx: &Ctx, d: DefId, key: &str) -> Option<DefId> {
    let mut cur = base_of_def(cx, d);
    for _ in 0..64 {
        let c = cur?;
        let a = cx.adt(c)?;
        if let Some(m) = a.methods.get(key).filter(|m| !m.is_static) {
            return Some(m.def);
        }
        if has_default(cx, c, key) {
            return None;
        }
        cur = base_of_def(cx, c);
    }
    None
}

/// Does class `d`'s method `key` implement an abstract method (is the one it would inherit
/// abstract)? Such a method overrides without `override`, as in TypeScript.
pub(super) fn implements_abstract(cx: &Ctx, d: DefId, key: &str) -> bool {
    inherited(cx, d, key).is_some_and(|m| cx.abstract_methods.contains(&m))
}

/// A concrete class must implement every abstract member it inherits (TypeScript's TS2515 and
/// TS2654).
pub(super) fn check_implemented(cx: &mut Ctx, d: DefId) {
    let Some(a) = cx.adt(d) else { return };
    let Some(decl) = a.decl.filter(|t| !t.is_abstract) else {
        return;
    };
    let (class, span) = (a.name.clone(), decl.name.span);
    // (declaring class, member) for each abstract member still unimplemented.
    let mut missing: Vec<(String, String)> = vec![];
    let mut cur = base_of_def(cx, d);
    let mut guard = 0;
    while let Some(c) = cur.filter(|_| guard < 64) {
        guard += 1;
        let Some(ca) = cx.adt(c) else { break };
        let (Some(cdecl), module) = (ca.decl, ca.module) else {
            break;
        };
        for m in cdecl.methods.iter().filter(|m| m.is_abstract) {
            let key = cx.method_key(module, &m.decl.sig.name, m.is_setter);
            let own = (cx.adt(d).and_then(|a| a.methods.get(&key)))
                .filter(|r| !r.is_static)
                .map(|r| r.def);
            let resolved = own.or_else(|| inherited(cx, d, &key));
            // A field of the name that doesn't fit was reported (`abstract_fields`).
            let field = (cx.adt(d).expect("ICE: adt").fields.iter())
                .any(|f| f.name == m.decl.sig.name.name);
            if !field && resolved.is_none_or(|r| cx.abstract_methods.contains(&r)) {
                // An accessor pair is one member, named as TypeScript names it.
                let what = m.decl.sig.name.name.clone();
                if !missing.iter().any(|(_, w)| *w == what) {
                    missing.push((ca.name.clone(), what));
                }
            }
        }
        cur = base_of_def(cx, c);
    }
    if missing.is_empty() {
        return;
    }
    let list = (missing.iter())
        .map(|(owner, m)| format!("`{m}` (from `{owner}`)"))
        .collect::<Vec<_>>()
        .join(", ");
    let (members, them, code) = match missing.len() {
        1 => ("member", "it", "TS2515"),
        _ => ("members", "them", "TS2654"),
    };
    cx.error(
        Diagnostic::error(
            format!("class `{class}` does not implement the abstract {members} {list} (TypeScript's {code})"),
            span,
        )
        .with_note(format!(
            "implement {them} in `{class}`, or declare `abstract class {class}`"
        )),
    );
}
