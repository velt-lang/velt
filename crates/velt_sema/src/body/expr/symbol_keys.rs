//! Symbol keys in expressions (`crate::symbols`): `o[KEY]` with a symbol constant reads the
//! member the symbol names, `{ [KEY]: v }` sets it, and `KEY in o` tests for it, all resolved
//! while checking (no lookup at run time).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::ctx::{Ctx, Item};
use crate::hir::{self, TyKind};

/// The well-known symbols (std/symbol.vlt `SymbolConstructor`), which name members as
/// `[Symbol.<name>]`.
const WELL_KNOWN: [&str; 15] = [
    "asyncDispose",
    "asyncIterator",
    "dispose",
    "hasInstance",
    "isConcatSpreadable",
    "iterator",
    "match",
    "matchAll",
    "replace",
    "search",
    "species",
    "split",
    "toPrimitive",
    "toStringTag",
    "unscopables",
];

impl FnCx<'_, '_> {
    /// The member name `e` names as a key: the symbol a module constant holds (`KEY`, not
    /// hidden by a local), or a well-known symbol (`Symbol.iterator`).
    pub(crate) fn symbol_key_of(&mut self, e: &ast::Expr) -> Option<String> {
        match &e.kind {
            ast::ExprKind::Ident(x) if !self.local_hides(&x.name) => {
                let Some(Item::Def(d)) = self.cx.lookup_item_at(self.module, &x.name, x.span)
                else {
                    return None;
                };
                let sym = self.cx.known_symbol(d)?;
                Some(self.cx.key_name(sym))
            }
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } => match &object.kind {
                ast::ExprKind::Ident(s)
                    if s.name == "Symbol"
                        && !self.local_hides("Symbol")
                        && self.cx.names_global_symbol(self.module)
                        && WELL_KNOWN.contains(&prop.name.as_str()) =>
                {
                    Some(format!("[Symbol.{}]", prop.name))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Key `k` of an object literal as a member name: a computed key `[KEY]` becomes the name
    /// of the member `KEY` names.
    pub(crate) fn literal_key_name(&mut self, k: ast::Ident) -> ast::Ident {
        if !crate::symbols::is_computed_key(&k) {
            return k;
        }
        let name = self.cx.member_key(self.module, &k.name, k.span);
        ast::Ident { name, span: k.span }
    }

    /// `o[KEY]` with a symbol key that names a member of `o`'s type: that member.
    pub(crate) fn symbol_index(
        &mut self,
        obj: hir::Expr,
        key: String,
        index: &ast::Expr,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        let prop = ast::Ident {
            name: key,
            span: index.span,
        };
        if matches!(want, Want::BorrowMut) {
            let Some(place) = self.field_access(obj, &prop, want, span) else {
                return self.error_expr(span);
            };
            self.check_readonly(&place, &prop);
            return place;
        }
        self.member_of(obj, &prop, want, span)
    }

    /// Reports a symbol converted to a string implicitly (a template literal part, an operand of
    /// `+`), which JavaScript rejects with a `TypeError` (TypeScript: TS2731); true if `h` is one.
    pub(crate) fn reject_symbol_text(&mut self, h: &hir::Expr) -> bool {
        let inner = self.cx.ty.opt_payload(h.ty).unwrap_or(h.ty);
        if !matches!(self.cx.ty.kind(inner), TyKind::Symbol) {
            return false;
        }
        self.cx.error(
            Diagnostic::error(
                "a symbol cannot be converted to a string implicitly: JavaScript throws a `TypeError`",
                h.span,
            )
            .with_note("write `String(s)`, `s.toString()` or `s.description`"),
        );
        true
    }

    /// `key in o` with a symbol key (a symbol constant or a well-known symbol): whether the
    /// value has that member, known from its type (on a union, from the member it holds).
    pub(crate) fn key_in(&mut self, lhs: &ast::Expr, rhs: &ast::Expr, span: Span) -> hir::Expr {
        let Some(key) = self.symbol_key_of(lhs) else {
            self.expr(lhs, None, Want::Borrow);
            self.expr(rhs, None, Want::Borrow);
            self.cx.error(
                Diagnostic::error(
                    "the left operand of `in` must be a symbol the compiler knows",
                    lhs.span,
                )
                .with_note("Velt objects have fixed fields, so `in` tests a key known while compiling: a module constant initialized with `Symbol(\"...\")` or `Symbol.for(\"...\")`, a well-known symbol (`Symbol.iterator`), or a private name (`#x in o`)"),
            );
            return self.error_expr(span);
        };
        let s = self.expr(rhs, None, Want::Borrow);
        if self.cx.ty.is_bottom(s.ty) {
            return self.error_expr(span);
        }
        if self.cx.ty.opt_payload(s.ty).is_some() || self.primitive_member(&s).is_some() {
            // TypeScript rejects both (TS18047, TS2322), and JavaScript throws a `TypeError`.
            let tn = self.cx.display(s.ty);
            self.cx.err(
                format!("the right operand of `in` must be an object, found `{tn}`"),
                rhs.span,
            );
            return self.error_expr(span);
        }
        let pred = move |cx: &Ctx, t| cx.has_member(t, &key);
        self.type_test(s, &pred, false, span).expr
    }
}
