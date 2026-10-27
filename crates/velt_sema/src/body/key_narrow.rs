//! Narrowing of `r[k]` with a `const` key (`field_narrow`), as TypeScript narrows an element
//! access whose key is a `const`: after `if (!slots[name]) slots[name] = [];`, `slots[name]` is
//! not `undefined`, and in `for (const k in r)` neither is `r[k]` (the loop tests `r.__has(k)`
//! first, `object_keys`). The key is part of the field path (`\0` and the key local's index,
//! which no property name written in a program has), so the path is narrowed, joined and
//! dropped like `r.name`: assigning `r` or `r[k]`, or `delete r[k]`, drops it. A narrowed read
//! is checked again where it happens (`UnwrapSome` of a value that is not a local), as for
//! fields: a call in between may have deleted the key.

use velt_syntax::ast;

use super::FnCx;

impl FnCx<'_, '_> {
    /// `object[k]` where `k` names a `const` local (in parentheses or not): the object and the
    /// path element that stands for the key (the key itself when `k` holds one literal).
    pub(crate) fn keyed_view<'e>(
        &mut self,
        e: &'e ast::Expr,
    ) -> Option<(&'e ast::Expr, ast::Ident)> {
        let ast::ExprKind::Index {
            object,
            index,
            optional: false,
        } = &e.kind
        else {
            return None;
        };
        // A `const` holding one literal key is that key (`o[K]` reads as `o.k`, `member`).
        if let Some(name) = self.single_key(index) {
            let span = index.span;
            return Some((object, ast::Ident { name, span }));
        }
        let prop = self.key_prop(index)?;
        Some((object, prop))
    }

    /// The path element of key `index` when it names a `const` local (`for (const k in r)`,
    /// `const name = …`): nothing can make it another key between a test and a read.
    pub(crate) fn key_prop(&mut self, index: &ast::Expr) -> Option<ast::Ident> {
        let mut inner = index;
        while let ast::ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        if !matches!(inner.kind, ast::ExprKind::Ident(_)) {
            return None;
        }
        let l = self.named_local(inner)?;
        if self.f.locals[l.0 as usize].mutable {
            return None;
        }
        Some(ast::Ident {
            name: format!("\0{}", l.0),
            span: index.span,
        })
    }
}
