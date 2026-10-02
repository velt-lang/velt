//! Object literals where a `Record<K, V>` is expected (docs/internals/design/record.md):
//! `{ let t = new Record(); t.__set("a", ..); t.__extend(spread); t }`. A closed record's literal
//! must have every key. Where the expected record's type arguments are not known yet (a generic
//! callee such as `Object.values({ a: 1 })`), an unknown key type is `string`, as in TypeScript
//! (an object's keys are strings), and an unknown value type is the first value's (widened), which
//! every other value must have.

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
        let spread = props
            .iter()
            .any(|p| matches!(p, ast::ObjectProp::Spread(_)));
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
        let spread = props
            .iter()
            .any(|p| matches!(p, ast::ObjectProp::Spread(_)));
        if (k != error && v != error) || spread || props.is_empty() {
            return self.record_literal_with(d, k, v, props, vec![], span);
        }
        let k = if k == error { self.cx.ty.str_ } else { k };
        if v != error {
            return self.record_literal_with(d, k, v, props, vec![], span);
        }
        let (v, pre) = self.record_literal_values(props, span);
        self.record_literal_with(d, k, v, props, pre, span)
    }

    /// The values of a record literal (with no spread and at least one key) whose value type
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
            let (name, value) = match p {
                ast::ObjectProp::KeyValue(name, value) => (name, Some(value)),
                ast::ObjectProp::Shorthand(name) => (name, None),
                ast::ObjectProp::Spread(_) => {
                    pre.push(None);
                    continue;
                }
            };
            let h = self.prop_value(name, value, v);
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
        mut pre: Vec<Option<hir::Expr>>,
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
        let mut seen: Vec<String> = vec![];
        let mut complete = false;
        for (i, p) in props.iter().enumerate() {
            let recv = self.mk(H::Local(t, UseMode::Borrow), ty, span);
            let call = match p {
                ast::ObjectProp::Spread(e) => {
                    complete = true;
                    self.record_call(recv, "__extend", std::slice::from_ref(e), e.span)
                }
                ast::ObjectProp::KeyValue(name, _) | ast::ObjectProp::Shorthand(name) => {
                    let pre = pre.get_mut(i).and_then(Option::take);
                    match self.record_literal_entry(recv, k, p, name, pre, &mut seen, span) {
                        Some(call) => call,
                        None => continue,
                    }
                }
            };
            stmts.push(hir::Stmt {
                kind: S::Expr(call),
                span,
            });
        }
        if let (Some(keys), false) = (&keys, complete) {
            let missing: Vec<&String> = keys.iter().filter(|k| !seen.contains(k)).collect();
            if !missing.is_empty() {
                let list: Vec<String> = missing.iter().map(|k| format!("\"{k}\"")).collect();
                let kn = self.cx.display(k);
                self.cx.err(
                    format!(
                        "missing key {} in a `Record<{kn}, ...>` literal",
                        list.join(", ")
                    ),
                    span,
                );
            }
        }
        let value = self.mk(H::Local(t, UseMode::Move), ty, span);
        let block = hir::Block {
            stmts,
            value: Some(Box::new(value)),
            span,
        };
        self.mk(H::Block(block), ty, span)
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
}
