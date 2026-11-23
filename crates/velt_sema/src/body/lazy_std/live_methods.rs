//! Which methods of a live class or struct are live with it ([`super`]; #3580). Lowering
//! (`velt_vir` `lower/`) builds a method of a class whose values live code has in these ways
//! only, so these methods are live with the class:
//!
//! - its table (`glue/vtable.rs` `class_entries`): every method with a slot in `vtable`
//!   (one overridden somewhere), whichever slot a call names, once it has a table;
//! - its constructor, which runs the base classes' (and the default-filling bodies of
//!   `virtual_defaults`, which have no name of their own);
//! - glue that calls a method by its name or key: `toJSON` and `[INSPECT]()` (`hooks.rs`,
//!   `glue/format.rs`, `any_reflect.rs`), `toString` where a text conversion has a value of the
//!   class ([`note_texts`]), `clone` (`transfer.rs`
//!   `own_clone`), `[Symbol.dispose]()` (drop glue), and std's methods keyed by a symbol, which
//!   lowering calls by key (`json.rs` `class_method`, `proxy_invoke`, `json_value_call`);
//!   [`BY_NAME`] adds the names sema's own desugarings call (the iteration and thenable
//!   protocols, `valueOf`, `compareTo`);
//! - getters and setters, which glue reads by name (forwarders of interfaces, absent fields,
//!   reflection);
//! - the methods of an implementation (`Callee::Dyn`, `ParamMethod`, the tables of interface
//!   values): live by [`super::live_impls`] once the interface is, and kept here as well;
//! - what sema makes for the class (adapters, the members `any` reads, forwarders), which has a
//!   name no code writes;
//! - the member slots of the `any` tables (`any_glue.rs`, `Program::dyn_members`): every
//!   method and accessor of every class, by name, once the program reads, calls or writes a
//!   member of an `any` (or makes an object literal with accessors). Then every method of a
//!   live class is live, as every one was before #3580.
//!
//! Every other method, a method or static method with a plain name, is live only where live
//! code names it (a call, a function value: [`super::walk`]), a base class's included when
//! called on a subclass's value (sema names the base's method). `VELT_LIVE_ALL_METHODS=1`
//! makes every method of a live class live, the rule before #3580.

use rustc_hash::FxHashSet;

use super::{loaded_for, mark, methods};
use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnKind};
use crate::hir::{DefId, TyId, TyKind};
use crate::hooks;

/// The plain names lowering's glue or sema's desugarings call on a class value by name: the
/// `toJSON` hook, the clone glue's `clone`, `valueOf` (`any_prim.rs`), the iteration protocol
/// (`for_iter.rs`, generators), the thenable protocol (`await_value.rs`), and `Comparable`'s
/// `compareTo` (velt_vir `ops.rs`). The `toString` hook has a rule of its own ([`note_texts`]).
const BY_NAME: &[&str] = &[
    "toJSON",
    "valueOf",
    "clone",
    "next",
    "return",
    "throw",
    "then",
    "catch",
    "finally",
    "compareTo",
];

/// The methods (and constructors) of class or struct `d` that are live with it (module docs).
pub(super) fn with_class(cx: &mut Ctx, d: DefId) -> Vec<DefId> {
    let ms = methods(cx).get(&d).cloned().unwrap_or_default();
    if cx.lazy.live.all_methods {
        return ms;
    }
    note_impl_methods(cx);
    ms.into_iter().filter(|&m| !named_only(cx, d, m)).collect()
}

/// Is `m`, a method of live class or struct `owner` made after the walk began, live with it?
pub(super) fn live_with(cx: &mut Ctx, owner: DefId, m: DefId) -> bool {
    if cx.lazy.live.all_methods {
        return true;
    }
    note_impl_methods(cx);
    !named_only(cx, owner, m)
}

/// Make live the methods of the live classes that are live with them now and weren't: when
/// every method became live (a member of an `any` is read), or an implementation was added.
pub(super) fn open_methods(cx: &mut Ctx) {
    if !cx.lazy.live.all_methods && every_method(cx) {
        cx.lazy.live.all_methods = true;
    }
    let live = &cx.lazy.live;
    let now = (cx.impls.len(), live.all_methods, live.texts_generic);
    if cx.lazy.live.methods_opened == Some(now) {
        return;
    }
    cx.lazy.live.methods_opened = Some(now);
    let adts: Vec<DefId> = cx.lazy.live.adts.iter().copied().collect();
    for d in adts {
        for m in with_class(cx, d) {
            if !cx.lazy.live.live.contains(&m) && loaded_for(cx, m, Some(d)) {
                mark(cx, m);
            }
        }
    }
}

/// Is every method of a live class live: does lowering give the `any` tables their member
/// slots (`any::dyn_members`, which lists every class's), or does `VELT_LIVE_ALL_METHODS` ask?
fn every_method(cx: &Ctx) -> bool {
    static ALL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ALL.get_or_init(|| std::env::var_os("VELT_LIVE_ALL_METHODS").is_some_and(|v| v != "0"))
        || cx.any.as_ref().is_some_and(|a| a.members)
        || crate::any::accessor_literals_dynamic(cx)
}

/// Bring the methods of the implementations up to date ([`named_only`]).
fn note_impl_methods(cx: &mut Ctx) {
    let live = &mut cx.lazy.live;
    if live.impl_methods_of == cx.impls.len() {
        return;
    }
    live.impl_methods_of = cx.impls.len();
    live.impl_methods = (cx.impls.iter())
        .flat_map(|i| i.methods.iter().copied())
        .collect::<FxHashSet<DefId>>();
}

/// Is `m`, of class or struct `owner`, live only where live code names it: a method or static
/// method of `owner`'s own, with a plain name no glue calls, not in its table, and no
/// implementation's (module docs)? Anything else is live with the class.
fn named_only(cx: &Ctx, owner: DefId, m: DefId) -> bool {
    let Some(DefInfo::Fn(f)) = cx.info.get(m.0 as usize) else {
        return false;
    };
    if !matches!(f.kind, FnKind::Method | FnKind::Static) || f.is_getter || f.owner != Some(owner) {
        return false;
    }
    let Some(a) = cx.adt(owner) else {
        return false;
    };
    // The key a method is declared by is the last part of its name: a setter's (`set v`), a
    // symbol's (`[INSPECT]`) and what sema makes (`<toString>`, `<any v>`) are no plain name.
    let key = f.name.strip_prefix(a.qual_name.as_str());
    let Some(key) = key.and_then(|k| k.strip_prefix('.')) else {
        return false;
    };
    let text = key == hooks::TO_STRING;
    plain_name(key)
        && !BY_NAME.contains(&key)
        && !(text && (cx.lazy.live.texts_generic || is_error_class(cx, owner)))
        && a.methods.get(key).is_some_and(|r| r.def == m)
        && !a.vtable.contains(&m)
        && !cx.lazy.live.impl_methods.contains(&m)
}

/// The `toString()` hook is called by the text glue only (velt_vir `js_string.rs`
/// `push_to_string`), which lowering builds for the operand of a text conversion
/// (`ToString`, `TemplateToString`: `String(x)`, a template, `+` with a string), for a class
/// value in it, an array's element, a union's member; and for an `Error` an `any` holds
/// (`push_dyn_error`). So the `toString` of a class (its own or the one it inherits) is live
/// where live code converts a value whose type holds the class to text, every class's where the
/// operand's type holds a type parameter (its instance may be any class), and `Error`'s and its
/// subclasses' always (with the table's slots, an override's).
pub(super) fn note_texts(cx: &mut Ctx, texts: &[TyId]) {
    let mut found = vec![];
    for &t in texts {
        if !cx.lazy.live.text_types.insert(t) {
            continue;
        }
        let (mut todo, mut seen) = (vec![t], FxHashSet::default());
        while let Some(t) = todo.pop() {
            if !seen.insert(t) {
                continue;
            }
            let kind = cx.ty.kind(t);
            match kind {
                TyKind::Param(_) | TyKind::Op(..) => cx.lazy.live.texts_generic = true,
                TyKind::Adt(d, _) => found.extend(to_string_of(cx, *d)),
                _ => {}
            }
            todo.extend(crate::types::contents(kind));
        }
    }
    for m in found {
        mark(cx, m);
    }
}

/// The `toString()` method of class `d`: its own, or the nearest base class's.
fn to_string_of(cx: &Ctx, d: DefId) -> Option<DefId> {
    let mut cur = d;
    for _ in 0..64 {
        let a = cx.adt(cur)?;
        if let Some(r) = a.methods.get(hooks::TO_STRING).filter(|r| !r.is_static) {
            return Some(r.def);
        }
        cur = cx.class_of(a.base?)?.0;
    }
    None
}

/// Is `d` the prelude's `Error` or a class that extends it?
fn is_error_class(cx: &Ctx, d: DefId) -> bool {
    let Some(error) = cx.prelude_adt("Error") else {
        return false;
    };
    let mut cur = d;
    for _ in 0..64 {
        if cur == error {
            return true;
        }
        let Some(next) = (cx.adt(cur).and_then(|a| a.base)).and_then(|b| cx.class_of(b)) else {
            return false;
        };
        cur = next.0;
    }
    false
}

/// Is `key` an identifier (`toISOString`), not a symbol, a setter or a name sema made?
fn plain_name(key: &str) -> bool {
    let mut cs = key.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}
