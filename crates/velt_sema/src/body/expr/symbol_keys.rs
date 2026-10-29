//! Symbol keys in expressions (`crate::symbols`): `o[KEY]` with a symbol constant reads the
//! member the symbol names, `{ [KEY]: v }` sets it, and `KEY in o` tests for it, all resolved
//! while checking (no lookup at run time).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::in_keys::InKey;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
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
                _ => self.enum_key_of(e),
            },
            _ => None,
        }
    }

    /// The member name an enum member names as a key (`Flags.SKIP` for
    /// `SKIP = "__v_skip"`: `__v_skip`; `crate::enum_keys`), if `Flags` is an enum not hidden by
    /// a local.
    pub(crate) fn enum_key_of(&mut self, e: &ast::Expr) -> Option<String> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &e.kind
        else {
            return None;
        };
        let ast::ExprKind::Ident(x) = &object.kind else {
            return None;
        };
        if self.local_hides(&x.name) {
            return None;
        }
        let path = format!("{}.{}", x.name, prop.name);
        let key = self.cx.enum_key(self.module, &path)?;
        self.cx.note_spelling(&key, &format!("[{path}]"));
        Some(key)
    }

    /// `props` with each `[E.M]: v` whose key is an enum member (which the parser writes as a
    /// key known only at run time, `...__computed_key(E.M, v)`) as the property its value
    /// names; `None` if there is none.
    pub(crate) fn enum_computed_keys(
        &mut self,
        props: &[ast::ObjectProp],
    ) -> Option<Vec<ast::ObjectProp>> {
        let mut out = Vec::with_capacity(props.len());
        let mut any = false;
        for p in props {
            let entry = match p {
                ast::ObjectProp::Spread(e) => super::record_literal::computed_entry(e),
                _ => None,
            };
            match entry.and_then(|(k, v)| Some((self.enum_key_of(k)?, k.span, v))) {
                Some((name, span, v)) => {
                    any = true;
                    out.push(ast::ObjectProp::KeyValue(
                        ast::Ident { name, span },
                        v.clone(),
                    ));
                }
                None => out.push(p.clone()),
            }
        }
        any.then_some(out)
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
        let inner = self.cx.ty.core(h.ty).unwrap_or(h.ty);
        let members = self.cx.union_members(inner).unwrap_or_else(|| vec![inner]);
        let symbol = |t: &TyId| {
            matches!(
                self.cx.ty.kind(self.cx.ty.core(*t).unwrap_or(*t)),
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
        self.instance_arg(rhs);
        // `"a" in #tmp`, built by a destructuring default for an optional field with a presence
        // flag (`pattern_defaults.rs`); source code cannot write it (no node id).
        if let (ast::NodeId(u32::MAX), ast::ExprKind::Lit(ast::Lit::Str(name))) =
            (lhs.id, &lhs.kind)
        {
            let obj = self.expr(rhs, None, Want::Borrow);
            return self.present_test(obj, name, true, span);
        }
        if let Some(h) = self.dynamic_in(lhs, rhs, span) {
            return h;
        }
        if let Some(h) = self.generic_in(lhs, rhs, span) {
            return h;
        }
        let Some(key) = self.in_key_of(lhs) else {
            return self.dynamic_key_in(lhs, rhs, span);
        };
        if matches!(key, InKey::Symbol(_)) {
            // `KEY in r` on a string-keyed record: its symbol table, tested at run time.
            let str_ = self.cx.ty.str_;
            let t = self.trial_expr(rhs).ty;
            if self.record_args(t).is_some_and(|(k, _)| k == str_) {
                return self.dynamic_key_in(lhs, rhs, span);
            }
        }
        if matches!(key, InKey::Str(_)) {
            if self.typed_proxy_once(rhs) {
                // `"k" in f()` of a typed proxy: the operand once, then as below.
                return self.operand_once(rhs, span, |cx, t| cx.key_in(lhs, t, span));
            }
            if let Some(h) = self.proxy_in(lhs, rhs, span) {
                return h;
            }
        }
        // `"toString" in r` on a record that may have no prototype: tested at run time.
        if let (true, InKey::Str(k)) = (self.cx.objects.null_proto_records, &key) {
            let target = match super::in_keys::OBJECT_PROTOTYPE.contains(&k.as_str()) {
                true => Some(self.trial_expr(rhs).ty),
                false => None,
            };
            let str_ = self.cx.ty.str_;
            if target
                .and_then(|t| self.record_args(t))
                .is_some_and(|(k, _)| k == str_)
            {
                return self.dynamic_key_in(lhs, rhs, span);
            }
        }
        let s = self.type_test_operand(rhs);
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
        // The members a test narrowed the operand to (`typeof o === "object" && "k" in o`):
        // the others cannot be there, and are not asked about.
        let live = self.narrowed_variants(&s);
        let dead = |v: usize| live.as_ref().is_some_and(|vs| !vs.contains(&(v as u32)));
        let mut found = Vec::with_capacity(members.len());
        for (v, &m) in members.iter().enumerate() {
            if dead(v) {
                found.push((Presence::No, vec![]));
                continue;
            }
            match self.in_test(m, &key, lhs.span, true) {
                Some(p) => found.push(p),
                None => return self.error_expr(span),
            }
        }
        let mut arms = vec![];
        let b = self.cx.ty.bool_;
        for (v, (&m, (p, held))) in members.iter().zip(&found).enumerate() {
            if dead(v) {
                continue;
            }
            if *p == Presence::No {
                if let InKey::Str(k) = &key {
                    // A copy made for a narrower object type lacks the original's other fields.
                    let at = self.cx.object_copies.in_key(k);
                    self.cx.object_copies.observe(m, span, Seen::In(at));
                }
            }
            if *p == Presence::Optional(false) {
                // Not-null stands for present: wrong once the field may hold `undefined`.
                let test = (key.name().to_string(), m, span);
                self.cx.object_copies.in_optional.push(test);
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
                            // A record's accessors too, in a program that defines them.
                            let keyed = self.cx.objects.record_accessors
                                && self.has_record_method("__hasKeyed");
                            let has = match keyed {
                                true => "__hasKeyed",
                                false => "__has",
                            };
                            self.record_call(obj, has, &[lit], span)
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
        if self.cx.dynamic(field.ty).is_some() {
            // `k?: any` left out holds the dynamic `undefined` (`object.rs`).
            let b = self.cx.ty.bool_;
            let undef = self.dynamic_nothing(hir::Nothing::Undefined, field.ty, span);
            let eq = self.intrinsic(hir::Intrinsic::AnyStrictEq, vec![field, undef], b, span);
            let not = H::Unary {
                op: hir::UnOp::Not,
                expr: Box::new(eq),
            };
            return self.mk(not, b, span);
        }
        let payload = self.cx.ty.core(field.ty).unwrap_or(field.ty);
        let wild = self.pat(P::Wildcard, payload, span);
        let some = self.present_pat(wild, field.ty, span);
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
