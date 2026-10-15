//! Whether an object type's property whose name spells a number was written as a number
//! (`{ [K in 1 | 2]: K }`) or as a string (`{ "1": 0 }`): TypeScript's `keyof` gives `1` for the
//! first and `"1"` for the second. Object types with the same fields are one type in Velt, so
//! the kind is recorded per type and name; a name recorded both ways makes `keyof` "not
//! supported yet" rather than a guess.

use velt_common::Span;

use super::template::js_number;
use crate::ctx::Ctx;
use crate::hir::{DefId, LitValue, TyId, TyKind};

/// How a property whose name spells a number was named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyKind {
    /// By a number literal type (the type).
    Number(TyId),
    /// By a string.
    Str,
    /// Both, by different object types with the same fields.
    Both,
}

/// Does `name` spell a number as JavaScript prints it (`"1"`, `"2.5"`, not `"01"`)?
pub(crate) fn spells_number(name: &str) -> bool {
    name.parse::<f64>()
        .is_ok_and(|n| n.is_finite() && js_number(n) == name)
}

impl Ctx<'_> {
    /// Records how object type `t`'s property `name` was named (`key`: the key's type).
    pub(crate) fn record_key_kind(&mut self, t: TyId, name: &str, key: TyId) {
        let TyKind::Adt(d, _) = *self.ty.kind(t) else {
            return;
        };
        if !spells_number(name) {
            return;
        }
        let kind = match self.ty.kind(key) {
            TyKind::Literal(LitValue::Int(..) | LitValue::Float(..)) => KeyKind::Number(key),
            _ => KeyKind::Str,
        };
        let slot = self
            .type_level
            .key_kinds
            .entry((d, name.to_string()))
            .or_insert(kind);
        *slot = match (*slot, kind) {
            (KeyKind::Number(a), KeyKind::Number(_)) => KeyKind::Number(a),
            (KeyKind::Str, KeyKind::Str) => KeyKind::Str,
            _ => KeyKind::Both,
        };
    }

    /// The type `keyof` gives property `name` of the object type defined by `d`: a string
    /// literal, or the number literal it was named by. `Err` (reported at `span`) when that
    /// depends on how the type was written.
    pub(crate) fn key_literal(&mut self, d: DefId, name: &str, span: Span) -> Result<TyId, ()> {
        match self
            .type_level
            .key_kinds
            .get(&(d, name.to_string()))
            .copied()
        {
            Some(KeyKind::Number(t)) => Ok(t),
            Some(KeyKind::Both) => {
                self.err(
                    format!("`keyof` of an object type whose property `{name}` is named by a number in one place and by a string in another is not supported yet (#934)"),
                    span,
                );
                Err(())
            }
            Some(KeyKind::Str) | None => Ok(self.lit_type(LitValue::Str(name.to_string()))),
        }
    }
}
