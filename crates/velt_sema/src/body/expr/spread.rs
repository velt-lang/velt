//! Spread in literals (docs/reference/types.md "Objects, arrays, tuples and maps"), desugared without new HIR:
//! - **Object spread** `{ ...a, b: 1 }` is a compile-time merged struct: the fields of each spread
//!   source, then the explicit properties, where a later name overrides an earlier one but keeps
//!   its position (JS key order). Typed by context it fills that struct (extra spread fields are
//!   ignored); otherwise it is an anonymous object. A struct/object *local* is consumed field by
//!   field (partial moves: fields that are overridden stay in it and are dropped with it); a
//!   class instance or a projected place is copied from (Copy fields) or cloned (the others);
//!   any other value is bound to a temporary first. Fields private to another type are skipped.
//! - **Array spread** `[a, ...xs, b]` →
//!   `{ let out = with_capacity(len); out.push(a); for (e of xs) out.push(<share of e>); ...; out }`
//!   (Copy elements are copied), each element converted to the literal's element type
//!   (`const ns: Named[] = [...cs]` makes interface values of the `C`s).
//!
//! Spread sources are evaluated before the other elements of the literal.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::places::{is_place, set_place_mode};
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{
    self, BinOp, DefId, ExprKind as H, Intrinsic, Pat, PatKind, StmtKind as S, TyId, TyKind,
    UseMode,
};

pub(super) fn has_spread(props: &[ast::ObjectProp]) -> bool {
    props
        .iter()
        .any(|p| matches!(p, ast::ObjectProp::Spread(_)))
}

/// A field value of a spread object literal.
enum Value<'e> {
    /// Read out of a spread source.
    Read(hir::Expr),
    /// An explicit property (`key: value` or shorthand `key`).
    Prop(ast::Ident, Option<&'e ast::Expr>),
}

impl FnCx<'_, '_> {
    /// Object literal with `...` properties; `target` = struct typed by name or context.
    pub(super) fn spread_object(
        &mut self,
        props: &[ast::ObjectProp],
        target: Option<(DefId, Vec<Option<TyId>>)>,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let mut lets = vec![];
        let mut entries: Vec<(String, Value)> = vec![];
        let mut explicit: Vec<String> = vec![];
        for p in props {
            let new: Vec<(String, Value)> = match p {
                ast::ObjectProp::Spread(e) => self
                    .spread_source(e, &mut lets)
                    .into_iter()
                    .map(|(n, h)| (n, Value::Read(h)))
                    .collect(),
                ast::ObjectProp::KeyValue(k, v) => {
                    vec![(k.name.clone(), Value::Prop(k.clone(), Some(v)))]
                }
                ast::ObjectProp::Shorthand(k) => {
                    vec![(k.name.clone(), Value::Prop(k.clone(), None))]
                }
            };
            for (name, v) in new {
                if let Value::Prop(k, _) = &v {
                    if explicit.contains(&name) {
                        self.cx.err(
                            format!("duplicate field `{name}` in object literal"),
                            k.span,
                        );
                        continue;
                    }
                    explicit.push(name.clone());
                }
                match entries.iter_mut().find(|(n, _)| *n == name) {
                    Some(slot) => slot.1 = v,
                    None => entries.push((name, v)),
                }
            }
        }
        let lit = match target {
            Some((d, slots)) => self.spread_struct(d, slots, entries, exp, span),
            None => self.spread_anon(entries, span),
        };
        self.with_lets(lets, lit)
    }

    /// `{ stmts; value }` (or just `value` when there are no statements).
    pub(super) fn with_lets(&mut self, lets: Vec<hir::Stmt>, value: hir::Expr) -> hir::Expr {
        if lets.is_empty() {
            return value;
        }
        let (ty, span) = (value.ty, value.span);
        let block = hir::Block {
            stmts: lets,
            value: Some(Box::new(value)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }

    /// A fresh temporary initialized with `init` (appended to `lets`), read as `Local(Borrow)`.
    fn temp(&mut self, name: &str, init: hir::Expr, lets: &mut Vec<hir::Stmt>) -> hir::Expr {
        let (ty, span) = (init.ty, init.span);
        let l = self.new_local(name, ty, false, span, LocalKind::Temp);
        lets.push(hir::Stmt {
            kind: S::Let {
                local: l,
                init: Some(init),
            },
            span,
        });
        self.mk(H::Local(l, UseMode::Borrow), ty, span)
    }

    /// The accessible fields of spread source `e`, each read per the module docs.
    pub(super) fn spread_source(
        &mut self,
        e: &ast::Expr,
        lets: &mut Vec<hir::Stmt>,
    ) -> Vec<(String, hir::Expr)> {
        let h = self.expr(e, None, Want::Borrow);
        let Some((d, args)) = self.adt_of(h.ty) else {
            if !self.cx.ty.is_bottom(h.ty) {
                let tn = self.cx.display(h.ty);
                self.cx.error(
                    Diagnostic::error(
                        format!("cannot spread a value of type `{tn}` into an object"),
                        e.span,
                    )
                    .with_note("object spread works on structs, classes and object literals"),
                );
            }
            return vec![];
        };
        let is_class = self.is_class_def(d);
        let (mut base, moves) = match &h.kind {
            H::Local(..) => (h, !is_class),
            _ if is_place(&h) => (h, false),
            _ => (self.temp("<spread>", h, lets), !is_class),
        };
        set_place_mode(&mut base, UseMode::Borrow);
        let fields = self.cx.adt(d).map(|a| a.fields.clone()).unwrap_or_default();
        let mut out = vec![];
        for (i, f) in fields.iter().enumerate() {
            if f.private_to.is_some_and(|o| !self.private_allowed(o)) {
                continue;
            }
            let fty = self.cx.ty.subst(f.ty, &args);
            out.push((
                f.name.clone(),
                self.field_read(&base, i as u32, fty, moves, e.span),
            ));
        }
        out
    }

    /// Field `index` of `base`: copied, moved out (`moves`), or cloned.
    fn field_read(
        &mut self,
        base: &hir::Expr,
        index: u32,
        ty: TyId,
        moves: bool,
        span: Span,
    ) -> hir::Expr {
        let copy = self.cx.is_copy(ty);
        let mode = match (copy, moves) {
            (true, _) => UseMode::Copy,
            (false, true) => UseMode::Move,
            (false, false) => UseMode::Borrow,
        };
        let field = self.mk(
            H::Field {
                base: Box::new(base.clone()),
                index,
                mode,
            },
            ty,
            span,
        );
        if mode != UseMode::Borrow {
            return field;
        }
        self.intrinsic(Intrinsic::Share, vec![field], ty, span)
    }

    fn spread_anon(&mut self, entries: Vec<(String, Value)>, span: Span) -> hir::Expr {
        let mut fields = vec![];
        let mut hs = vec![];
        for (name, v) in entries {
            let h = match v {
                Value::Read(h) => h,
                Value::Prop(k, value) => self.prop_value(&k, value, None),
            };
            fields.push((name, h.ty));
            hs.push(h);
        }
        let (def, type_args) = self.anon_def(&fields);
        let ty = self.cx.ty.intern(TyKind::Adt(def, type_args.clone()));
        let kind = H::AdtLit {
            def,
            type_args,
            fields: hs,
        };
        self.mk(kind, ty, span)
    }

    fn spread_struct(
        &mut self,
        d: DefId,
        mut slots: Vec<Option<TyId>>,
        entries: Vec<(String, Value)>,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let a = self.cx.adt(d).expect("ICE: struct");
        let (sname, fields) = (a.name.clone(), a.fields.clone());
        let mut values: Vec<Option<hir::Expr>> = fields.iter().map(|_| None).collect();
        for (name, v) in entries {
            let i = fields.iter().position(|f| f.name == name);
            let h = match (v, i) {
                (Value::Read(h), Some(_)) => h,
                // Extra fields of a spread source are not part of the target type.
                (Value::Read(_), None) => continue,
                (Value::Prop(k, value), Some(i)) => {
                    self.check_private(fields[i].private_to, &k.name, k.span);
                    self.cx
                        .rec_ref(k.span, crate::ide::record::Target::Field(d, i as u32));
                    let expected = self.cx.ty.subst_known(fields[i].ty, &slots);
                    self.prop_value(&k, value, Some(expected))
                }
                (Value::Prop(k, value), None) => {
                    self.cx
                        .err(format!("no field `{}` on type `{sname}`", k.name), k.span);
                    self.prop_value(&k, value, None);
                    continue;
                }
            };
            let i = i.expect("ICE: field index");
            self.cx.match_ty(fields[i].ty, h.ty, &mut slots);
            values[i] = Some(h);
        }
        self.finish_struct(d, slots, values, exp, span)
    }

    /// Array literal with `...xs` elements (see the module docs).
    pub(super) fn spread_array(
        &mut self,
        elems: &[ast::Expr],
        exp_elem: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let mut lets = vec![];
        let mut sources: Vec<Option<(hir::Expr, TyId)>> = vec![];
        let mut elem = exp_elem;
        for e in elems {
            let ast::ExprKind::Spread(inner) = &e.kind else {
                sources.push(None);
                continue;
            };
            let h = self.expr(inner, None, Want::Borrow);
            let Some(et) = self.cx.ty.array_elem(h.ty) else {
                if !self.cx.ty.is_bottom(h.ty) {
                    let tn = self.cx.display(h.ty);
                    self.cx.err(
                        format!("cannot spread a value of type `{tn}` into an array"),
                        inner.span,
                    );
                }
                sources.push(None);
                continue;
            };
            elem.get_or_insert(et);
            let mut src = if is_place(&h) {
                h
            } else {
                self.temp("<spread>", h, &mut lets)
            };
            set_place_mode(&mut src, UseMode::Borrow);
            sources.push(Some((src, et)));
        }
        let Some(elem) = elem else {
            return self.error_expr(span);
        };
        let arr_ty = self.cx.ty.array(elem);
        let cap = self.spread_capacity(elems, &sources, span);
        let init = self.intrinsic(Intrinsic::ArrayWithCapacity, vec![cap], arr_ty, span);
        let out_l = self.new_local("<array>", arr_ty, true, span, LocalKind::Temp);
        lets.push(hir::Stmt {
            kind: S::Let {
                local: out_l,
                init: Some(init),
            },
            span,
        });
        for (e, src) in elems.iter().zip(sources) {
            let stmt = match (src, &e.kind) {
                (Some((src, et)), _) => self.push_all(out_l, arr_ty, src, (et, elem), e.span),
                (None, ast::ExprKind::Spread(_)) => continue,
                (None, _) => {
                    let v = self.expr_coerce(e, elem, Want::Move);
                    self.push_stmt(out_l, arr_ty, v)
                }
            };
            lets.push(stmt);
        }
        let out = self.mk(H::Local(out_l, UseMode::Move), arr_ty, span);
        self.with_lets(lets, out)
    }

    /// `n_plain + xs.length + ...` as a `usize`.
    fn spread_capacity(
        &mut self,
        elems: &[ast::Expr],
        sources: &[Option<(hir::Expr, TyId)>],
        span: Span,
    ) -> hir::Expr {
        let usize_ = self.cx.ty.usize;
        let plain = elems
            .iter()
            .filter(|e| !matches!(e.kind, ast::ExprKind::Spread(_)))
            .count();
        let mut cap = self.mk(H::Lit(hir::Lit::Int(plain as u128)), usize_, span);
        for (src, _) in sources.iter().flatten() {
            let len = self.intrinsic(Intrinsic::ArrayLen, vec![src.clone()], usize_, span);
            let kind = H::Binary {
                op: BinOp::Add,
                lhs: Box::new(cap),
                rhs: Box::new(len),
            };
            cap = self.mk(kind, usize_, span);
        }
        cap
    }

    fn push_stmt(&mut self, out: hir::LocalId, arr_ty: TyId, v: hir::Expr) -> hir::Stmt {
        let span = v.span;
        let target = self.mk(H::Local(out, UseMode::BorrowMut), arr_ty, span);
        let unit = self.cx.ty.unit;
        let call = self.intrinsic(Intrinsic::ArrayPush, vec![target, v], unit, span);
        hir::Stmt {
            kind: S::Expr(call),
            span,
        }
    }

    /// `for (const e of src) out.push(e / share of e);`, each element converted from the
    /// source's element type to the literal's (`(from, to)` in `elems`).
    fn push_all(
        &mut self,
        out: hir::LocalId,
        arr_ty: TyId,
        src: hir::Expr,
        elems: (TyId, TyId),
        span: Span,
    ) -> hir::Stmt {
        let (elem, to) = elems;
        let copy = self.cx.is_copy(elem);
        let mode = if copy { UseMode::Copy } else { UseMode::Borrow };
        let e = self.new_local("<elem>", elem, false, span, LocalKind::Elem);
        let read = self.mk(H::Local(e, mode), elem, span);
        let value = if copy {
            read
        } else {
            self.intrinsic(Intrinsic::Share, vec![read], elem, span)
        };
        let value = self.coerce(value, to);
        let push = self.push_stmt(out, arr_ty, value);
        let binding = Pat {
            kind: PatKind::Binding(e, mode),
            ty: elem,
            span,
        };
        hir::Stmt {
            kind: S::ForOf {
                label: None,
                binding,
                iter: src,
                body: hir::Block {
                    stmts: vec![push],
                    value: None,
                    span,
                },
                consume: false,
            },
            span,
        }
    }
}
