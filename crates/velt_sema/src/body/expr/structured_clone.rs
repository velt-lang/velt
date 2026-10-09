//! `structuredClone(x)` (std/prelude/clone.vlt) copies with `x.clone()`, which copies anything.
//! JS's structured clone does not: it throws a `DataCloneError` for a function or a promise, and
//! copies a class instance as a plain object (its prototype is dropped). Such a value, at any
//! depth of the argument's type, is a compile error here, with what JS does and what to write
//! instead. std's own classes (`Map`, `Set`, `Date`, `RegExp`, `Error`, ...) are the ones JS
//! clones as themselves, so they are copied; their type arguments are looked into.

use velt_common::{Diagnostic, Span};

use crate::body::FnCx;
use crate::defs::DefInfo;
use crate::hir::{AdtKind, TyId, TyKind};

/// Deeper than this, a type is not looked into (recursive types).
const MAX_DEPTH: u32 = 16;

/// What JS's structured clone does not copy.
enum Uncloneable {
    Function,
    Promise,
    /// A class declared outside std (also behind an interface type).
    Class(TyId),
}

impl FnCx<'_, '_> {
    /// Report an error if a value of type `t` (`structuredClone`'s argument, at `span`) holds
    /// something JS's structured clone throws for or copies as a plain object.
    pub(super) fn check_structured_clone(&mut self, t: TyId, span: Span) {
        let Some((found, at)) = self.uncloneable(t, 0) else {
            return;
        };
        let tn = self.cx.display(t);
        let (what, note) = match found {
            Uncloneable::Function => (
                "a function".to_string(),
                "JS throws a `DataCloneError` for a function value; copy the data around it and pass the function along as it is".to_string(),
            ),
            Uncloneable::Promise => (
                "a promise".to_string(),
                "JS throws a `DataCloneError` for a promise; await it and copy its value".to_string(),
            ),
            Uncloneable::Class(c) => {
                let cn = self.cx.display(c);
                (
                    format!("an instance of class `{cn}`"),
                    format!("JS copies a class instance as a plain object: the copy is not `instanceof {cn}` and has none of its methods; write `.clone()` for a deep copy that stays a `{cn}`"),
                )
            }
        };
        let inside = if at == t {
            String::new()
        } else {
            format!(" (inside `{tn}`)")
        };
        self.cx.error(
            Diagnostic::error(
                format!("`structuredClone` cannot copy {what}{inside}"),
                span,
            )
            .with_note(note),
        );
    }

    /// The first thing in a value of type `t` that JS does not clone, and its type.
    fn uncloneable(&mut self, t: TyId, depth: u32) -> Option<(Uncloneable, TyId)> {
        if depth > MAX_DEPTH {
            return None;
        }
        let parts: Vec<TyId> = match self.cx.ty.kind(t).clone() {
            TyKind::FnPtr { .. } | TyKind::Closure(_) => return Some((Uncloneable::Function, t)),
            TyKind::Promise(..) => return Some((Uncloneable::Promise, t)),
            TyKind::Dyn(..) => return Some((Uncloneable::Class(t), t)),
            TyKind::Adt(d, args) => match &self.cx.info[d.0 as usize] {
                DefInfo::Adt(a) if a.kind == AdtKind::Class => {
                    if !self.cx.scopes[a.module].is_std {
                        return Some((Uncloneable::Class(t), t));
                    }
                    // A std class: what JS clones as itself; its contents are its type
                    // arguments (`Map<K, V>`, `Set<T>`).
                    args
                }
                DefInfo::Adt(a) => {
                    let tys: Vec<TyId> = a.fields.iter().map(|f| f.ty).collect();
                    tys.into_iter().map(|f| self.cx.subst(f, &args)).collect()
                }
                DefInfo::Enum(e) => {
                    let tys: Vec<TyId> =
                        e.variants.iter().flat_map(|v| v.payload.clone()).collect();
                    tys.into_iter().map(|f| self.cx.subst(f, &args)).collect()
                }
                _ => args,
            },
            k => crate::types::children(&k),
        };
        parts
            .into_iter()
            .find_map(|p| self.uncloneable(p, depth + 1))
    }
}
