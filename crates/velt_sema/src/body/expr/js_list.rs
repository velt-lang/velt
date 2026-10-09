//! Arrays written as text (`${xs}`, `xs.join()`, `xs.toString()`), as JS's
//! `Array.prototype.toString` writes them: each element as `String(x)`, joined with ",".
//! Lowering writes numbers, strings, booleans, `null` and nested arrays exactly, a class
//! instance through its `toString()` (the class's `to_string` hook, `crate::hooks`), and every
//! other object as JS's default `Object.prototype.toString` does (`[object Object]`,
//! `[object Map]`, `[object Set]`). What it cannot write is an element whose JS text comes from
//! a method it has no hook for: a struct's `toString()` or a class's that is not a plain
//! `toString(): string` (JS calls it for each element), an `Error` (`Error: message`) or a
//! `RegExp` (`/source/flags`). Such an array is rejected here, with the `.map(...).join(",")`
//! that writes the same text.

use velt_common::{Diagnostic, Span};

use crate::body::FnCx;
use crate::collect::lookup_method;
use crate::hir::{TyId, TyKind};

/// Deeper than this, a type is not looked into (recursive types).
const MAX_DEPTH: u32 = 16;

impl FnCx<'_, '_> {
    /// Report an error if a value of type `t` is (or holds) an array that `what` (`a template
    /// literal`, `` `join` ``) cannot write as JS does; true if it reported one.
    pub(crate) fn reject_js_list(&mut self, t: TyId, what: &str, span: Span) -> bool {
        let Some(elem) = self.js_list_blocker(t, 0, false) else {
            return false;
        };
        let en = self.cx.display(elem);
        let (why, each) = if self.is_error_class(elem) {
            (
                "JS writes each element as `Error: message`",
                "`Error: ${x.message}`",
            )
        } else if self.is_regexp(elem) {
            (
                "JS writes each element as `/source/flags`",
                "`/${x.source}/${x.flags}`",
            )
        } else {
            ("JS calls each element's `toString()`", "x.toString()")
        };
        self.cx.error(
            Diagnostic::error(
                format!("{what} cannot write an array of `{en}` as JavaScript does"),
                span,
            )
            .with_note(format!(
                "{why}; write the text with `.map((x) => {each}).join(\",\")`"
            )),
        );
        true
    }

    /// The first element type inside an array (at any depth, also through tuples, nullables
    /// and unions) that JS writes with its own method; `in_list`: `t` is such an element.
    fn js_list_blocker(&mut self, t: TyId, depth: u32, in_list: bool) -> Option<TyId> {
        if depth > MAX_DEPTH {
            return None;
        }
        match self.cx.ty.kind(t).clone() {
            TyKind::Array(e) => self.js_list_blocker(e, depth + 1, true),
            TyKind::Tuple(tys) => tys
                .into_iter()
                .find_map(|e| self.js_list_blocker(e, depth + 1, true)),
            TyKind::Option(e) | TyKind::Shared(e) => self.js_list_blocker(e, depth + 1, in_list),
            _ if !in_list => None,
            _ => {
                if let Some(members) = self.cx.union_members(t) {
                    return members
                        .into_iter()
                        .find_map(|m| self.js_list_blocker(m, depth + 1, true));
                }
                let own = self.is_error_class(t)
                    || self.is_regexp(t)
                    || (self.has_to_string(t) && !self.has_to_string_hook(t));
                own.then_some(t)
            }
        }
    }

    /// Does the class or struct `t` declare (or inherit) an instance `toString(): string`?
    fn has_to_string(&mut self, t: TyId) -> bool {
        let Some((d, args)) = self.adt_of(t) else {
            return false;
        };
        let Some(found) = lookup_method(self.cx, d, &args, "toString") else {
            return false;
        };
        if found.is_static() {
            return false;
        }
        let f = self.cx.fn_info(found.def());
        f.params.is_empty() && f.ret == self.cx.ty.str_
    }

    /// Does class `t` have a `toString()` lowering calls through the class's `to_string` hook?
    fn has_to_string_hook(&mut self, t: TyId) -> bool {
        let Some((d, args)) = self.adt_of(t) else {
            return false;
        };
        crate::hooks::hook(self.cx, d, &args, crate::hooks::TO_STRING).is_some()
    }

    /// Is `t` the prelude's `Error` class or a subclass of it?
    fn is_error_class(&mut self, t: TyId) -> bool {
        let (Some((d, _)), Some(err)) = (self.cx.class_of(t), self.cx.prelude_adt("Error")) else {
            return false;
        };
        self.cx.class_extends(d, err)
    }

    /// Is `t` std's `RegExp` class?
    fn is_regexp(&mut self, t: TyId) -> bool {
        let Some((d, _)) = self.cx.class_of(t) else {
            return false;
        };
        self.cx
            .adt(d)
            .is_some_and(|a| a.name == "RegExp" && a.qual_name.contains("regex"))
    }
}
