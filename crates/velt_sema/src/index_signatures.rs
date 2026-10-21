//! Index signatures (docs/reference/types.md "Objects, arrays, tuples and maps"): an object type
//! or interface with only an index signature, `{ [key: string]: V }`, is `Record<string, V>`, as
//! the two are interchangeable in TypeScript. `readonly` is not checked yet, as for
//! `readonly T[]`. Named members next to an index signature whose values are `any` or `unknown`
//! (`{ children?: JSXChildren; [key: string]: any }`) are keys of that record too: every type is
//! one the values have, and a named member reads as `any` (or `unknown`) like any other key.
//! What Velt cannot represent exactly is an error saying it is not supported yet: named members
//! next to another index signature (a read of `name` would be `V | null`, and an optional one
//! needs `undefined`, #930) and a `number` key.

use velt_common::Diagnostic;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};
use crate::resolve::TyEnv;

impl Ctx<'_> {
    /// `{ [key: K]: V }`: `Record<string, V>` (see the module docs).
    pub(crate) fn resolve_index_sig(
        &mut self,
        fields: &[ast::ObjectTypeField],
        key: &ast::TypeExpr,
        value: &ast::TypeExpr,
        env: &TyEnv,
    ) -> TyId {
        let k = self.resolve_type(key, env);
        let v = self.resolve_type(value, env);
        let mut ok = k != self.ty.error && v != self.ty.error;
        match self.ty.kind(k) {
            TyKind::Str | TyKind::Error => {}
            TyKind::Int(_) | TyKind::Float(_) => {
                ok = false;
                self.error(
                    Diagnostic::error(
                        "a `number` index signature is not supported yet (#937)",
                        key.span,
                    )
                    .with_note(
                        "use an array, a `Map<number, V>`, or a `string` key (`{ [key: string]: V }`)",
                    ),
                );
            }
            _ => {
                ok = false;
                let shown = self.display(k);
                self.error(
                    Diagnostic::error(
                        format!("an index signature's key must be `string` or `number`; found `{shown}`"),
                        key.span,
                    )
                    .with_note("for a fixed set of keys, write `Record<K, V>` or an object type"),
                );
            }
        }
        // Next to `[key: string]: any` (or `unknown`) every named member's type is one the
        // values have, so the type is that record: a named member reads as `V`, like any key.
        let dynamic_values = matches!(
            self.dynamic(v),
            Some(crate::any::Dynamic::Any | crate::any::Dynamic::Unknown)
        );
        if dynamic_values {
            for f in fields {
                if self.resolve_type(&f.ty, env) == self.ty.error {
                    ok = false;
                }
            }
        } else if let Some(f) = fields.first() {
            ok = false;
            self.error(
                Diagnostic::error(
                    format!("a named member (`{}`) next to an index signature is not supported yet (#937)", f.name.name),
                    f.name.span,
                )
                .with_note("an object type with only an index signature is a `Record<string, V>`, and so is one with named members next to `[key: string]: any` (or `unknown`); next to other values, each would need its own type (and an optional one `undefined`, #930)"),
            );
        }
        if !ok {
            return self.ty.error;
        }
        let Some(record) = self.prelude_adt("Record") else {
            return self.ty.error;
        };
        let str_ = self.ty.str_;
        self.ty.intern(TyKind::Adt(record, vec![str_, v]))
    }
}
