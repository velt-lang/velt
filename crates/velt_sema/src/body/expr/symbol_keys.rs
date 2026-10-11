//! Symbol keys in expressions (`crate::symbols`): `o[KEY]` with a symbol constant reads the
//! member the symbol names, `{ [KEY]: v }` sets it, and `KEY in o` tests for it, all resolved
//! while checking (no lookup at run time).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::ctx::Item;
use super::in_keys::InKey;
use crate::hir::{self, DefId, ExprKind as H, PatKind as P, TyId, TyKind};
use crate::object_copies::Seen;
use crate::symbols::Presence;

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
                let key = self.cx.key_name(sym);
                self.cx.note_spelling(&key, &format!("[{}]", x.name));
                Some(key)
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
        let name = self.cx.member_key(self.module, &k);
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
    /// `+`), which JavaScript rejects with a `TypeError` (TypeScript: TS2731); true if `h` is one
    /// or may hold one (a union with a `symbol` member, `string | symbol`).
    pub(crate) fn reject_symbol_text(&mut self, h: &hir::Expr) -> bool {
        let inner = self.cx.ty.opt_payload(h.ty).unwrap_or(h.ty);
        let members = self.cx.union_members(inner).unwrap_or_else(|| vec![inner]);
        let symbol = |t: &TyId| {
            matches!(
                self.cx.ty.kind(self.cx.ty.opt_payload(*t).unwrap_or(*t)),
                TyKind::Symbol
            )
        };
        if !members.iter().any(symbol) {
            return false;
        }
        let msg = match members.len() {
            1 => "a symbol cannot be converted to a string implicitly: JavaScript throws a `TypeError`".to_string(),
            _ => format!(
                "a `{}` may hold a symbol, which cannot be converted to a string implicitly: JavaScript throws a `TypeError`",
                self.cx.display(h.ty)
            ),
        };
        self.cx.error(
            Diagnostic::error(msg, h.span)
                .with_note("write `String(s)`, `s.toString()` or `s.description`"),
        );
        true
    }

    /// `key in o` with a key known while compiling (a symbol constant, a well-known symbol or a
    /// string literal): whether the value has that member, known from its type (on a union,
    /// from the member it holds), and for an optional member of an object type or interface
    /// (`[KEY]?: T`, `a?: T`) or a record's entry, whether it is present (tested at run time).
    /// A key known only at run time is `in_keys.rs`'s.
    pub(crate) fn key_in(&mut self, lhs: &ast::Expr, rhs: &ast::Expr, span: Span) -> hir::Expr {
        let Some(key) = self.in_key_of(lhs) else {
            return self.dynamic_key_in(lhs, rhs, span);
        };
        let s = self.expr(rhs, None, Want::Borrow);
        if self.cx.ty.is_bottom(s.ty) {
            return self.error_expr(span);
        }
        let primitive = !matches!(self.cx.typeof_tag(s.ty), "object" | "function")
            && self.cx.union_def(s.ty).is_none();
        if self.cx.ty.opt_payload(s.ty).is_some()
            || primitive
            || self.primitive_member(&s).is_some()
        {
            // TypeScript rejects both (TS18047, TS2322), and JavaScript throws a `TypeError`.
            let tn = self.cx.display(s.ty);
            self.cx.err(
                format!("the right operand of `in` must be an object, found `{tn}`"),
                rhs.span,
            );
            return self.error_expr(span);
        }
        let sty = s.ty;
        let union = self.cx.union_def(sty).map(|(d, _)| d);
        let members = self.cx.union_members(sty).unwrap_or_else(|| vec![sty]);
        let mut found = Vec::with_capacity(members.len());
        for &m in &members {
            match self.in_test(m, &key, lhs.span, true) {
                Some(p) => found.push(p),
                None => return self.error_expr(span),
            }
        }
        let mut arms = vec![];
        let b = self.cx.ty.bool_;
        for (v, (&m, (p, held))) in members.iter().zip(&found).enumerate() {
            if *p == Presence::No {
                if let InKey::Str(k) = &key {
                    // A copy made for a narrower object type lacks the original's other fields.
                    let at = self.cx.object_copies.in_key(k);
                    self.cx.object_copies.observe(m, span, Seen::In(at));
                }
            }
            let (pat, body) = match p {
                Presence::No if held.is_empty() => continue,
                Presence::Yes => {
                    let wild = self.pat(P::Wildcard, m, span);
                    (wild, self.mk(H::Lit(hir::Lit::Bool(true)), b, span))
                }
                _ => {
                    let (l, mode) = self.option_binding(&s, m, "<in>", false);
                    let bind = self.pat(P::Binding(l, mode), m, span);
                    let obj = self.mk(H::Local(l, hir::UseMode::Borrow), m, span);
                    let test = match p {
                        Presence::Optional(flag) => self.present_test(obj, key.name(), *flag, span),
                        Presence::Entry => {
                            let lit = ast::Expr {
                                id: ast::NodeId(u32::MAX),
                                kind: ast::ExprKind::Lit(ast::Lit::Str(key.name().to_string())),
                                span: lhs.span,
                            };
                            self.record_call(obj, "__has", &[lit], span)
                        }
                        _ => self.mk(H::Lit(hir::Lit::Bool(false)), b, span),
                    };
                    (bind, self.class_or(l, m, held, test, span))
                }
            };
            let pat = match union {
                Some(def) => self.pat(
                    P::Variant {
                        def,
                        variant: v as u32,
                        args: vec![pat],
                    },
                    sty,
                    span,
                ),
                None => pat,
            };
            arms.push(hir::Arm {
                pat,
                guard: None,
                body,
            });
        }
        if arms.len() < members.len() || arms.is_empty() {
            arms.push(hir::Arm {
                pat: self.pat(P::Wildcard, sty, span),
                guard: None,
                body: self.mk(H::Lit(hir::Lit::Bool(false)), b, span),
            });
        }
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, b, span)
    }

    /// `test`, or `true` when the value bound to `l` (type `t`) is an instance of one of the
    /// classes `held` (`match (l) { InstanceOf(C) => true, ..., _ => test }`).
    fn class_or(
        &mut self,
        l: hir::LocalId,
        t: TyId,
        held: &[DefId],
        test: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        if held.is_empty() {
            return test;
        }
        let b = self.cx.ty.bool_;
        let mut arms: Vec<hir::Arm> = held
            .iter()
            .map(|&c| hir::Arm {
                pat: self.pat(P::InstanceOf(c), t, span),
                guard: None,
                body: self.mk(H::Lit(hir::Lit::Bool(true)), b, span),
            })
            .collect();
        arms.push(hir::Arm {
            pat: self.pat(P::Wildcard, t, span),
            guard: None,
            body: test,
        });
        let obj = self.mk(H::Local(l, hir::UseMode::Borrow), t, span);
        let kind = H::Match {
            scrutinee: Box::new(obj),
            arms,
        };
        self.mk(kind, b, span)
    }

    /// Is optional field `key` of `obj` present: its presence flag (`flag`), or not null?
    fn present_test(&mut self, obj: hir::Expr, key: &str, flag: bool, span: Span) -> hir::Expr {
        let prop = ast::Ident {
            name: key.to_string(),
            span,
        };
        let Some(field) = self.field_access(obj, &prop, Want::Borrow, span) else {
            return self.error_expr(span);
        };
        if flag {
            let b = self.cx.ty.bool_;
            return self.intrinsic(hir::Intrinsic::FieldPresent, vec![field], b, span);
        }
        let payload = self.cx.ty.opt_payload(field.ty).unwrap_or(field.ty);
        let wild = self.pat(P::Wildcard, payload, span);
        let some = self.pat(P::Some(Box::new(wild)), field.ty, span);
        self.bool_match(field, Some(some), span)
    }

    /// Whether a (non-null) value of type `t` has member `key` (`None`: unknown, a well-known
    /// symbol other than an iterator one on a built-in or standard library type). For
    /// `Symbol.iterator` and `Symbol.asyncIterator` the type's methods answer: a sync generator
    /// has no `[Symbol.asyncIterator]`, as in JavaScript.
    pub(crate) fn key_presence(&mut self, t: TyId, key: &str) -> Option<Presence> {
        let declared = self.cx.declared_presence(t, key);
        if !key.starts_with("[Symbol.") {
            // A symbol the program made: only its own types have such members.
            return Some(declared.unwrap_or(Presence::No));
        }
        if matches!(declared, Some(Presence::Yes | Presence::Optional(_))) {
            return declared;
        }
        let iterator = key == ast::SYMBOL_ITERATOR || key == ast::SYMBOL_ASYNC_ITERATOR;
        if iterator {
            return Some(match self.method_exists(t, key) {
                true => Presence::Yes,
                false => Presence::No,
            });
        }
        // A type of the program's own: objects have no well-known symbol members of their own
        // (`Object.prototype` has none) unless they declare them.
        declared
    }
}
