//! A subclass may not redefine a method an `extend` block gives one of its base classes (its
//! own method, or one of an `extend` block of its own). `extend` methods are not virtual: a
//! call through the base type would run the `extend` method, not the subclass's, which is a
//! wrong result rather than an error. Until `extend` methods can be overridden, the redefinition
//! is an error (docs/reference/classes.md, "extend"; #1133 makes them virtual).

use std::collections::HashMap;

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::key_member_name;
use crate::hir::{AdtKind, DefId};

pub(super) fn check(cx: &mut Ctx) {
    // The instance members of `extend` blocks, by the class they extend: name, definition.
    let mut extended: HashMap<DefId, Vec<(String, DefId)>> = HashMap::new();
    for e in &cx.extensions {
        let Some((d, _)) = cx.class_of(e.target) else {
            continue;
        };
        for (key, m) in &e.methods {
            if !m.is_static {
                let name = key_member_name(key).to_string();
                extended.entry(d).or_default().push((name, m.def));
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
    let mut errors: Vec<Diagnostic> = vec![];
    for c in classes {
        // The members the class itself declares: instance methods and accessors (by member name,
        // so a setter-only `set kind` counts), its own fields and parameter properties, and the
        // members of its own `extend` blocks.
        let mut own: Vec<(String, Span)> = vec![];
        if let Some(a) = cx.adt(c) {
            for (key, m) in &a.methods {
                if !m.is_static {
                    own.push((
                        key_member_name(key).to_string(),
                        cx.fn_info(m.def).name_span,
                    ));
                }
            }
            let inherited = base_class(cx, c)
                .and_then(|b| cx.adt(b))
                .map_or(0, |b| b.fields.len());
            for f in a.fields.iter().skip(inherited) {
                own.push((f.name.clone(), f.span));
            }
        }
        let own_extended = extended.get(&c).cloned().unwrap_or_default();
        for (name, def) in &own_extended {
            own.push((name.clone(), cx.fn_info(*def).name_span));
        }
        let mut base = base_class(cx, c);
        for _ in 0..64 {
            let Some(b) = base else { break };
            let class = short_name(cx, b);
            for (name, ext) in extended.get(&b).into_iter().flatten() {
                for (_, span) in own.iter().filter(|(n, _)| n == name) {
                    errors.push(
                        Diagnostic::error(
                            format!(
                                "`{name}` is an `extend` method of `{class}` and cannot be redefined in a subclass yet"
                            ),
                            *span,
                        )
                        .with_label(cx.fn_info(*ext).name_span, "the `extend` method")
                        .with_note(format!(
                            "`extend` methods are not virtual: a call through a `{class}` would run the `extend` method; give the subclass's member another name"
                        )),
                    );
                }
            }
            // An `extend` block of the subclass over a member the base class itself declares:
            // calls through the base would not reach it either.
            if let Some(ba) = cx.adt(b) {
                for (name, def) in &own_extended {
                    let declared = ba
                        .methods
                        .iter()
                        .any(|(k, m)| !m.is_static && key_member_name(k) == name)
                        || ba.fields.iter().any(|f| f.name == *name);
                    if declared {
                        errors.push(
                            Diagnostic::error(
                                format!(
                                    "`{name}` is a member of `{class}`: an `extend` block of a subclass cannot redefine it yet"
                                ),
                                cx.fn_info(*def).name_span,
                            )
                            .with_note(format!(
                                "`extend` methods are not virtual: a call through a `{class}` would run `{class}`'s `{name}`; redefine it in the subclass's body with `override`"
                            )),
                        );
                    }
                }
            }
            base = base_class(cx, b);
        }
    }
    for d in errors {
        cx.error(d);
    }
}

fn short_name(cx: &Ctx, d: DefId) -> String {
    let name = cx.adt(d).map(|a| a.name.clone()).unwrap_or_default();
    name.rsplit("::").next().unwrap_or(&name).to_string()
}

fn base_class(cx: &Ctx, d: DefId) -> Option<DefId> {
    let base = cx.adt(d)?.base?;
    cx.class_of(base).map(|(b, _)| b)
}
