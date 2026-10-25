//! Number-keyed records (docs/reference/types.md "Record"): `Record<number, V>` and
//! `{ [id: number]: V }` are string-keyed records (`crate::record_keys`), and a variable or
//! parameter declared with one keeps TypeScript's errors for keys that are not numbers: a
//! string key that is not a number's name (`r["abc"]`, TS7015; `r.abc`, TS2339) and such a key
//! in the object literal it is initialized with (TS2353). `"1"` and `"1.5"` are numbers' names,
//! as in TypeScript.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::record::RecordKey;
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, LocalId};

impl FnCx<'_, '_> {
    /// Records local `l` declared with the written type `ty` when that is a number-keyed record,
    /// and checks the object literal `init` it is initialized with.
    pub(crate) fn note_number_record(
        &mut self,
        l: LocalId,
        ty: Option<&ast::TypeExpr>,
        init: Option<&ast::Expr>,
    ) {
        let Some(t) = ty.filter(|t| self.may_be_marked(t) && self.is_number_record_written(t, 0))
        else {
            return;
        };
        self.mark_local(l, crate::body::NUMBER_RECORD);
        let Some(ast::ExprKind::Object(props)) = init.map(|e| &e.kind) else {
            return;
        };
        let shown = number_record_shown(t);
        for p in props {
            let name = match p {
                ast::ObjectProp::KeyValue(n, _) | ast::ObjectProp::Shorthand(n) => n,
                _ => continue,
            };
            if !crate::symbols::is_computed_key(name) && !is_number_name(&name.name) {
                self.cx.error(Diagnostic::error(
                    format!("object literal may only specify known properties, and '{}' does not exist in type '{shown}' (TypeScript's TS2353)", name.name),
                    name.span,
                ));
            }
        }
    }

    /// The key `key` of the record `obj`: an error when `obj` is a number-keyed record variable
    /// and the key a string that is not a number's name.
    pub(super) fn number_record_key(&mut self, obj: &hir::Expr, key: &RecordKey<'_>) {
        if !self.is_number_record(obj) {
            return;
        }
        let (name, span, member): (&str, Span, bool) = match key {
            RecordKey::Name(id) => (&id.name, id.span, true),
            RecordKey::Index(e) => match &e.kind {
                ast::ExprKind::Lit(ast::Lit::Str(s)) => (s, e.span, false),
                _ => return,
            },
            RecordKey::Computed(_) => return,
        };
        if is_number_name(name) {
            return;
        }
        let message = match member {
            true => format!(
                "property '{name}' does not exist on a number-keyed record (TypeScript's TS2339)"
            ),
            false => "an index of a number-keyed record must be a number (TypeScript's TS7015)"
                .to_string(),
        };
        self.cx.error(Diagnostic::error(message, span).with_note("the keys of `Record<number, V>` are numbers (stored as their strings): `r[1]`, `r[\"1\"]`"));
    }

    fn is_number_record(&self, h: &hir::Expr) -> bool {
        if self.f.local_marks.is_empty() {
            return false;
        }
        match &h.kind {
            H::Local(l, _) => self.local_marked(*l, crate::body::NUMBER_RECORD),
            H::Cast(x) | H::UnwrapSome(x, _) | H::Upcast(x) => self.is_number_record(x),
            _ => false,
        }
    }

    /// `Record<number, V>` or `{ [k: number]: V }`, possibly `| null` / `| undefined`, also
    /// through type aliases of the module.
    fn is_number_record_written(&self, t: &ast::TypeExpr, depth: usize) -> bool {
        if crate::record_keys::is_number_record_type(t) {
            return true;
        }
        let ast::TypeExprKind::Named { path, args } = &t.kind else {
            return false;
        };
        if depth > 8 || path.len() != 1 || !args.is_empty() {
            return false;
        }
        let marked = &self.cx.marked_aliases().1;
        if marked.is_empty() || !marked.contains(&path[0].name) {
            return false;
        }
        match self
            .cx
            .lookup_item_at(self.module, &path[0].name, path[0].span)
        {
            Some(crate::ctx::Item::Alias(a)) => {
                let decl = self.cx.aliases[a as usize].decl;
                decl.generics.is_empty() && self.is_number_record_written(&decl.ty, depth + 1)
            }
            _ => false,
        }
    }
}

/// How TypeScript shows the record type in TS2353.
fn number_record_shown(t: &ast::TypeExpr) -> String {
    match &t.kind {
        ast::TypeExprKind::Named { path, .. } if path.len() == 1 && path[0].name != "Record" => {
            path[0].name.clone()
        }
        _ => "Record<number, ...>".to_string(),
    }
}

/// Is `s` the name of a number, as TypeScript tells (`String(Number(s)) === s`)?
fn is_number_name(s: &str) -> bool {
    s.parse::<f64>()
        .is_ok_and(|n| velt_syntax::js_number(n) == s)
}

#[cfg(test)]
mod tests {
    use super::is_number_name;

    #[test]
    fn number_names_are_canonical() {
        for s in ["0", "1", "1.5", "-1", "1e+21", "NaN", "Infinity"] {
            assert!(is_number_name(s), "{s}");
        }
        for s in ["abc", "01", "1.50", "+1", "inf", "1e21", " 1", ""] {
            assert!(!is_number_name(s), "{s}");
        }
    }
}
