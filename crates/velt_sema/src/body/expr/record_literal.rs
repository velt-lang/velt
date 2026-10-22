//! Object literals where a `Record<K, V>` is expected (docs/internals/design/record.md):
//! `{ let t = new Record(); t.__set("a", ..); t.__extend(spread); t }`. A closed record's literal
//! must have every key. Where the expected record's type arguments are not known yet (a generic
//! callee such as `Object.values({ a: 1 })`), an unknown key type is `string`, as in TypeScript
//! (an object's keys are strings), and an unknown value type is the first value's (widened), which
//! every other value must have. A spread of an `any` or `object` (`{ ...a }`) makes the literal
//! a `Record<string, any>`, filled at run time by the prelude's `__spreadAny` with the keys of the
//! value held (`Intrinsic::AnyKeys`, then a member read of each).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::record::RecordKey;
use super::setters::synth;
use crate::body::{FnCx, LocalKind};
use crate::hir::{self, DefId, ExprKind as H, StmtKind as S, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// Reports why an object literal cannot build a `Record<k, ...>`, if it cannot: a bad key, an
    /// enum key, or a type-parameter key without a spread (which would bring every key).
    fn record_literal_unsupported(
        &mut self,
        k: TyId,
        props: &[ast::ObjectProp],
        span: Span,
    ) -> bool {
        if !self.check_record_key(k, span) {
            return true;
        }
        let enum_keys =
            matches!(self.cx.ty.kind(k), TyKind::Adt(..)) && self.cx.union_def(k).is_none();
        if enum_keys {
            let kn = self.cx.display(k);
            self.cx.err(
                format!("a `Record` keyed by the enum `{kn}` cannot be written as an object literal yet"),
                span,
            );
            return true;
        }
        let spread = props.iter().any(is_spread);
        !spread
            && self.reject_generic_key(
                k,
                "cannot build a {R} from an object literal without a spread",
                span,
            )
    }

    /// An object literal where `Record<K, V>` (`d`) is expected (`K` or `V` may be unknown:
    /// the error type).
    pub(super) fn record_literal(
        &mut self,
        d: DefId,
        k: TyId,
        v: TyId,
        props: &[ast::ObjectProp],
        span: Span,
    ) -> hir::Expr {
        let error = self.cx.ty.error;
        // `{ ...a }` of an `any`: a `Record<string, any>`.
        let dynamic = props.iter().any(|p| match p {
            ast::ObjectProp::Spread(e) if computed_entry(e).is_none() => {
                self.trial_type(e).is_some_and(|t| self.spreads_dynamic(t))
            }
            _ => false,
        });
        let (k, v) = match (dynamic, self.any_ty()) {
            (true, Some(any)) if v == error => (if k == error { self.cx.ty.str_ } else { k }, any),
            _ => (k, v),
        };
        let spread = props.iter().any(is_spread);
        if (k != error && v != error) || (spread && v != error) || props.is_empty() {
            return self.record_literal_with(d, k, v, props, vec![], span);
        }
        let k = if k == error { self.cx.ty.str_ } else { k };
        if v != error {
            return self.record_literal_with(d, k, v, props, vec![], span);
        }
        // `{ ...r, a: 1 }` spreading a record: its value type (the first such record's).
        let spread_v = props.iter().find_map(|p| match p {
            ast::ObjectProp::Spread(e) if computed_entry(e).is_none() => self
                .trial_type(e)
                .and_then(|t| self.record_args(self.cx.ty.core(t).unwrap_or(t)))
                .map(|(_, v)| v),
            _ => None,
        });
        if let Some(v) = spread_v {
            return self.record_literal_with(d, k, v, props, vec![], span);
        }
        if !props.iter().any(|p| !is_spread(p)) {
            // Only spreads (`{ ...a, ...b }` where a `Record` of unknown value type is built).
            self.cx.error(
                Diagnostic::error("cannot infer the value type of this object literal", span)
                    .with_note("annotate it (`const o: Record<string, V> = { ... }`)"),
            );
            return self.error_expr(span);
        }
        // The value type is the first value's; the spreads must have it too (`__extend`).
        let (v, pre) = self.record_literal_values(props, span);
        self.record_literal_with(d, k, v, props, pre, span)
    }

    /// The values of a record literal (with at least one key; spreads are skipped) whose value type
    /// is not known yet, checked: the first one gives the type, and the others must have it
    /// (a value that has not is reported, and replaced by an error expression).
    fn record_literal_values(
        &mut self,
        props: &[ast::ObjectProp],
        span: Span,
    ) -> (TyId, Vec<Option<hir::Expr>>) {
        let mut v = None;
        let mut pre = vec![];
        let mut other = None;
        for p in props {
            let h = match p {
                ast::ObjectProp::KeyValue(name, value) => self.prop_value(name, Some(value), v),
                ast::ObjectProp::Shorthand(name) => self.prop_value(name, None, v),
                ast::ObjectProp::Spread(e) if computed_entry(e).is_some() => {
                    let (_, value) = computed_entry(e).expect("ICE: a computed entry");
                    self.expr(value, v, crate::body::Want::Move)
                }
                ast::ObjectProp::Spread(_) | ast::ObjectProp::Method(_) => {
                    pre.push(None);
                    continue;
                }
            };
            // `{ a: "x", b: "y" }` holds `string`s: literal types widen, as in array literals.
            let vt = match v {
                Some(vt) => vt,
                None => *v.insert(self.cx.widened(h.ty)),
            };
            let h = self.try_coerce(h, vt).unwrap_or_else(|h| {
                other.get_or_insert(h.ty);
                self.error_expr(h.span)
            });
            pre.push(Some(h));
        }
        let v = v.unwrap_or(self.cx.ty.error);
        if let Some(other) = other {
            self.mixed_record_values(v, other, span);
        }
        (v, pre)
    }

    /// Reports a record literal whose values have the types `v` and `other`.
    fn mixed_record_values(&mut self, v: TyId, other: TyId, span: Span) {
        let (vn, on) = (self.cx.display(v), self.cx.display(other));
        self.cx.error(
            Diagnostic::error(
                format!("the values of this object literal have different types (`{vn}` and `{on}`), but a `Record` has one value type"),
                span,
            )
            .with_note("`Object.values` and `Object.entries` return one value type: list the keys with `Object.keys`, or give the values one type (a union, or `Record<string, JsonValue>`)"),
        );
    }

    /// [`Self::record_literal`] with known `K` and `V`; `pre`: the values, already checked
    /// (by property; empty: none are).
    fn record_literal_with(
        &mut self,
        d: DefId,
        k: TyId,
        v: TyId,
        props: &[ast::ObjectProp],
        pre: Vec<Option<hir::Expr>>,
        span: Span,
    ) -> hir::Expr {
        if self.record_literal_unsupported(k, props, span) {
            return self.error_expr(span);
        }
        let keys = self.record_keys(k);
        let ty = self.cx.ty.intern(TyKind::Adt(d, vec![k, v]));
        let new = self.mk(
            H::New {
                def: d,
                type_args: vec![k, v],
                args: vec![],
            },
            ty,
            span,
        );
        let t = self.new_local("<record>", ty, false, span, LocalKind::Temp);
        let mut stmts = vec![hir::Stmt {
            kind: S::Let {
                local: t,
                init: Some(new),
            },
            span,
        }];
        let (seen, complete) =
            self.record_literal_entries((t, ty), k, props, pre, &mut stmts, span);
        if let (Some(keys), false) = (&keys, complete) {
            self.record_missing_keys(k, keys, &seen, span);
        }
        let value = self.mk(H::Local(t, UseMode::Move), ty, span);
        let block = hir::Block {
            stmts,
            value: Some(Box::new(value)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }

    /// The calls that fill the record in the temporary `t` (of type `ty`) from `props`, onto
    /// `stmts`. Returns the keys set, and whether a spread may have set every key.
    fn record_literal_entries(
        &mut self,
        (t, ty): (hir::LocalId, TyId),
        k: TyId,
        props: &[ast::ObjectProp],
        mut pre: Vec<Option<hir::Expr>>,
        stmts: &mut Vec<hir::Stmt>,
        span: Span,
    ) -> (Vec<String>, bool) {
        let mut seen: Vec<String> = vec![];
        let mut complete = false;
        for (i, p) in props.iter().enumerate() {
            let recv = self.mk(H::Local(t, UseMode::Borrow), ty, span);
            let call = match p {
                ast::ObjectProp::Spread(e) if computed_entry(e).is_some() => {
                    let (key, value) = computed_entry(e).expect("ICE: a computed entry");
                    let key = RecordKey::Computed(key);
                    match pre.get_mut(i).and_then(Option::take) {
                        Some(h) => self.record_set_checked(recv, key, h, e.span),
                        None => self.record_call_keyed(
                            recv,
                            "__set",
                            key,
                            std::slice::from_ref(value),
                            e.span,
                        ),
                    }
                }
                ast::ObjectProp::Spread(e)
                    if self.trial_type(e).is_some_and(|t| self.spreads_dynamic(t)) =>
                {
                    complete = true;
                    match self.spread_any(recv, ty, e) {
                        Some(call) => call,
                        None => continue,
                    }
                }
                ast::ObjectProp::Spread(e) => {
                    // `...r` of a `Record | null` (or `| undefined`) copies nothing when `r` is
                    // nothing, as in JavaScript.
                    let nullable = self
                        .trial_type(e)
                        .is_some_and(|t| self.cx.ty.core(t).is_some());
                    complete = !nullable;
                    let m = if nullable {
                        "__extendMaybe"
                    } else {
                        "__extend"
                    };
                    self.record_call(recv, m, std::slice::from_ref(e), e.span)
                }
                ast::ObjectProp::KeyValue(name, _) | ast::ObjectProp::Shorthand(name) => {
                    let pre = pre.get_mut(i).and_then(Option::take);
                    match self.record_literal_entry(recv, k, p, name, pre, &mut seen, span) {
                        Some(call) => call,
                        None => continue,
                    }
                }
                ast::ObjectProp::Method(_) => continue,
            };
            stmts.push(hir::Stmt {
                kind: S::Expr(call),
                span,
            });
        }
        (seen, complete)
    }

    /// Reports the `keys` of the closed key type `k` that a literal (at `span`) left out.
    fn record_missing_keys(&mut self, k: TyId, keys: &[String], seen: &[String], span: Span) {
        let missing: Vec<String> = keys
            .iter()
            .filter(|k| !seen.contains(k))
            .map(|k| format!("\"{k}\""))
            .collect();
        if missing.is_empty() {
            return;
        }
        let kn = self.cx.display(k);
        self.cx.err(
            format!(
                "missing key {} in a `Record<{kn}, ...>` literal",
                missing.join(", ")
            ),
            span,
        );
    }

    /// The `__set` call for key `name` of a record literal (`None`: reported, or not a key of a
    /// closed record); `pre`: its value, already checked.
    #[allow(clippy::too_many_arguments)] // the entry, its literal and the keys seen so far
    fn record_literal_entry(
        &mut self,
        recv: hir::Expr,
        k: TyId,
        p: &ast::ObjectProp,
        name: &ast::Ident,
        pre: Option<hir::Expr>,
        seen: &mut Vec<String>,
        span: Span,
    ) -> Option<hir::Expr> {
        // `[KEY]` with a module constant holding a string: the string is the key.
        let resolved;
        let name = if crate::symbols::is_computed_key(name) {
            resolved = ast::Ident {
                name: self.cx.member_key(self.module, name),
                span: name.span,
            };
            &resolved
        } else {
            name
        };
        if self.record_keys(k).is_some() && !self.record_has_key(k, &name.name, name.span) {
            return None;
        }
        if seen.contains(&name.name) {
            self.cx.err(
                format!("duplicate key `{}` in object literal", name.name),
                name.span,
            );
            return None;
        }
        seen.push(name.name.clone());
        let key = RecordKey::Name(name);
        if let Some(h) = pre {
            return Some(self.record_set_checked(recv, key, h, span));
        }
        let value = match p {
            ast::ObjectProp::KeyValue(_, value) => value.clone(),
            _ => synth(ast::ExprKind::Ident(name.clone()), name.span),
        };
        Some(self.record_call_keyed(recv, "__set", key, std::slice::from_ref(&value), span))
    }

    /// Is `t` a dynamic type an object spread takes (`any`, `object`; also `| null`)? An
    /// `unknown` is not one (TypeScript's TS2698).
    pub(super) fn spreads_dynamic(&self, t: TyId) -> bool {
        let t = self.cx.ty.core(t).unwrap_or(t);
        self.cx
            .dynamic(t)
            .is_some_and(|d| d != crate::any::Dynamic::Unknown)
    }

    /// `__spreadAny(r, e)` for the spread `...e` of an `any` into the record literal whose
    /// temporary `recv` (of type `ty`) reads; `None` when reported.
    fn spread_any(&mut self, recv: hir::Expr, ty: TyId, e: &ast::Expr) -> Option<hir::Expr> {
        let value_any = self.record_args(ty).is_some_and(|(k, v)| {
            k == self.cx.ty.str_ && self.cx.dynamic(v) == Some(crate::any::Dynamic::Any)
        });
        let H::Local(t, _) = recv.kind else {
            return None;
        };
        if !value_any {
            let tn = self.cx.display(ty);
            self.cx.error(
                Diagnostic::error(
                    format!("spreading an `any` into a `{tn}` is not supported yet (#924)"),
                    e.span,
                )
                .with_note(
                    "build a `Record<string, any>` (`const o: Record<string, any> = { ...a }`)",
                ),
            );
            return None;
        }
        let name = format!("<spread@{}>", e.span.lo);
        if let Some(scope) = self.f.scopes.last_mut() {
            scope.names.insert(name.clone(), t);
        }
        let ident = |n: &str| {
            synth(
                ast::ExprKind::Ident(ast::Ident {
                    name: n.to_string(),
                    span: e.span,
                }),
                e.span,
            )
        };
        let call = synth(
            ast::ExprKind::Call {
                callee: Box::new(ident(self.proxy_op("__spreadAny"))),
                type_args: vec![],
                args: vec![ident(&name), e.clone()],
                optional: false,
            },
            e.span,
        );
        // The keys and values are read through the value's descriptor.
        self.note_dyn_members();
        Some(self.expr(&call, None, crate::body::Want::Move))
    }
}

/// The key and value of `{ [key]: value }` with a key known only at run time, which the parser
/// writes as a spread of a call of the prelude's `__computed_key` (`ast::COMPUTED_KEY_FN`).
pub(crate) fn computed_entry(e: &ast::Expr) -> Option<(&ast::Expr, &ast::Expr)> {
    let ast::ExprKind::Call {
        callee,
        args,
        type_args,
        ..
    } = &e.kind
    else {
        return None;
    };
    match (&callee.kind, args.as_slice()) {
        (ast::ExprKind::Ident(f), [key, value])
            if f.name == ast::COMPUTED_KEY_FN && type_args.is_empty() =>
        {
            Some((key, value))
        }
        _ => None,
    }
}

/// Is `p` a spread (`...e`, not a computed-key entry)?
fn is_spread(p: &ast::ObjectProp) -> bool {
    matches!(p, ast::ObjectProp::Spread(e) if computed_entry(e).is_none())
}

/// Does an object literal have a property with a computed key known only at run time?
pub(crate) fn has_computed_entry(props: &[ast::ObjectProp]) -> bool {
    props
        .iter()
        .any(|p| matches!(p, ast::ObjectProp::Spread(e) if computed_entry(e).is_some()))
}
