//! A subclass may not redefine a method an `extend` block gives one of its base classes (its
//! own method, or one of an `extend` block of its own). `extend` methods are not virtual: a
//! call through the base type would run the `extend` method, not the subclass's, which is a
//! wrong result rather than an error. Until `extend` methods can be overridden, the redefinition
//! is an error (docs/reference/classes.md, "extend"; #1133 makes them virtual).

use std::collections::HashMap;

use velt_common::Diagnostic;

use crate::ctx::Ctx;
use crate::hir::{AdtKind, DefId};

pub(super) fn check(cx: &mut Ctx) {
    // The instance methods of `extend` blocks, by the class they extend.
    let mut extended: HashMap<DefId, Vec<(String, DefId)>> = HashMap::new();
    for e in &cx.extensions {
        let Some((d, _)) = cx.class_of(e.target) else {
            continue;
        };
        for (key, m) in &e.methods {
            if !m.is_static {
                extended.entry(d).or_default().push((key.clone(), m.def));
            }
        }
    }
    if extended.is_empty() {
        return;
    }
    let classes: Vec<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .filter(|d| {
            cx.adt(*d)
                .is_some_and(|a| a.kind == AdtKind::Class && a.decl.is_some())
        })
        .collect();
    let mut errors = vec![];
    for c in classes {
        let own: Vec<(String, DefId)> = cx
            .adt(c)
            .map(|a| {
                a.methods
                    .iter()
                    .filter(|(_, m)| !m.is_static)
                    .map(|(k, m)| (k.clone(), m.def))
                    .collect()
            })
            .unwrap_or_default();
        let own_extended = extended.get(&c).cloned().unwrap_or_default();
        let mut base = base_class(cx, c);
        for _ in 0..64 {
            let Some(b) = base else { break };
            for (key, ext) in extended.get(&b).into_iter().flatten() {
                let redefinition = own
                    .iter()
                    .chain(own_extended.iter())
                    .find(|(k, _)| k == key);
                if let Some((_, def)) = redefinition {
                    errors.push((key.clone(), *def, *ext, b));
                }
            }
            base = base_class(cx, b);
        }
    }
    for (key, def, ext, b) in errors {
        let name = key.strip_prefix("set ").unwrap_or(&key).to_string();
        let class = cx.adt(b).map(|a| a.name.clone()).unwrap_or_default();
        let class = class.rsplit("::").next().unwrap_or(&class).to_string();
        cx.error(
            Diagnostic::error(
                format!(
                    "`{name}` is an `extend` method of `{class}` and cannot be redefined in a subclass yet"
                ),
                cx.fn_info(def).name_span,
            )
            .with_label(cx.fn_info(ext).name_span, "the `extend` method")
            .with_note(format!(
                "`extend` methods are not virtual: a call through a `{class}` would run the `extend` method; give the subclass's method another name"
            )),
        );
    }
}

fn base_class(cx: &Ctx, d: DefId) -> Option<DefId> {
    let base = cx.adt(d)?.base?;
    cx.class_of(base).map(|(b, _)| b)
}
