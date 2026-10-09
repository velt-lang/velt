//! Object spread in literals (docs/reference/types.md "Objects, arrays, tuples and maps"),
//! desugared without new HIR: `{ ...a, b: 1 }` is a compile-time merged struct: the fields of
//! each spread source, then the explicit properties, where a later name overrides an earlier one
//! but keeps its position (JS key order). Typed by context it fills that struct (extra spread
//! fields are ignored); otherwise it is an anonymous object. A struct/object *local* is consumed
//! field by field (partial moves: fields that are overridden stay in it and are dropped with
//! it); a class instance or a projected place is copied from (Copy fields) or cloned (the
//! others); any other value is bound to a temporary first. Fields private to another type are
//! skipped. An optional source field (`a?: T`) is copied only while present, as JavaScript copies
//! only the keys an object has. Array spread is in `spread_array.rs`.
//!
//! Spread sources are evaluated before the other elements of the literal.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::places::{is_place, set_place_mode};
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, DefId, ExprKind as H, Intrinsic, StmtKind as S, TyId, TyKind, UseMode};

pub(super) fn has_spread(props: &[ast::ObjectProp]) -> bool {
    props
        .iter()
        .any(|p| matches!(p, ast::ObjectProp::Spread(_)))
}

/// Where a spread field's value comes from.
#[derive(Clone)]
struct Src {
    /// The source field is optional (`a?: T`): it may be absent.
    optional: bool,
    /// It keeps "absent" apart from a present `null` (`a?: T | null`, `hir::FieldDef::presence`).
    presence: bool,
    /// The source field as a place, for the presence or null test of an optional one.
    place: Option<hir::Expr>,
}

/// A field value of a spread object literal.
enum Value<'e> {
    /// Read out of a spread source.
    Read(hir::Expr, Src),
    /// An optional source field over an earlier value: the earlier value stays when the field
    /// is absent, as a JavaScript spread copies only the keys an object has.
    Over(hir::Expr, Src, Box<Value<'e>>),
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
                    .spread_fields(e, &mut lets)
                    .into_iter()
                    .map(|(n, h, src)| (n, Value::Read(h, src)))
                    .collect(),
                ast::ObjectProp::KeyValue(k, v) => {
                    vec![(k.name.clone(), Value::Prop(k.clone(), Some(v)))]
                }
                ast::ObjectProp::Shorthand(k) => {
                    vec![(k.name.clone(), Value::Prop(k.clone(), None))]
                }
                ast::ObjectProp::Method(_) => vec![],
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
                    Some(slot) => {
                        let none = Src {
                            optional: false,
                            presence: false,
                            place: None,
                        };
                        let prev = std::mem::replace(
                            &mut slot.1,
                            Value::Read(self.error_expr(span), none),
                        );
                        slot.1 = match v {
                            Value::Read(h, src) if src.optional => {
                                Value::Over(h, src, Box::new(prev))
                            }
                            v => v,
                        };
                    }
                    None => entries.push((name, v)),
                }
            }
        }
        let lit = match target {
            Some((d, slots)) => self.spread_struct(d, slots, entries, exp, span, &mut lets),
            None => self.spread_anon(entries, span, &mut lets),
        };
        self.with_lets(lets, lit)
    }

    /// `{ stmts; value }` (or just `value` when there are no statements).
    pub(crate) fn with_lets(&mut self, lets: Vec<hir::Stmt>, value: hir::Expr) -> hir::Expr {
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
    pub(super) fn temp(
        &mut self,
        name: &str,
        init: hir::Expr,
        lets: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
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

    /// The accessible fields of spread source `e`, each read per the module docs, with whether
    /// the field is optional (`a?: T`: absent when null).
    pub(super) fn spread_source(
        &mut self,
        e: &ast::Expr,
        lets: &mut Vec<hir::Stmt>,
    ) -> Vec<(String, hir::Expr, bool)> {
        self.spread_fields(e, lets)
            .into_iter()
            .map(|(n, h, src)| (n, h, src.optional && !src.presence))
            .collect()
    }

    /// [`spread_source`](Self::spread_source), with whether each field is optional.
    fn spread_fields(
        &mut self,
        e: &ast::Expr,
        lets: &mut Vec<hir::Stmt>,
    ) -> Vec<(String, hir::Expr, Src)> {
        let h = self.expr(e, None, Want::Borrow);
        let seen = crate::object_copies::Seen::Spread;
        self.cx.object_copies.observe(h.ty, e.span, seen);
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
        let (fields, kind) = self
            .cx
            .adt(d)
            .map(|a| (a.fields.clone(), a.kind))
            .unwrap_or((vec![], crate::hir::AdtKind::Struct));
        let mut out = vec![];
        for (i, f) in fields.iter().enumerate() {
            // ES private fields (`#x`) are never copied, as in JavaScript.
            if f.name.starts_with(ast::PRIVATE_NAME_PREFIX)
                || f.private_to.is_some_and(|o| !self.private_allowed(o))
            {
                continue;
            }
            let fty = self.cx.subst(f.ty, &args);
            let presence = crate::anon::has_presence(&self.cx.ty, kind, f);
            // An optional field is read only when present, so it is shared rather than moved
            // (a conditional move would leave the source half-owned).
            let read = self.field_read(&base, i as u32, fty, moves && !f.optional, e.span);
            let place = f.optional.then(|| {
                let field = H::Field {
                    base: Box::new(base.clone()),
                    index: i as u32,
                    mode: UseMode::Borrow,
                };
                self.mk(field, fty, e.span)
            });
            let src = Src {
                optional: f.optional,
                presence,
                place,
            };
            out.push((f.name.clone(), read, src));
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

    fn spread_anon(
        &mut self,
        entries: Vec<(String, Value)>,
        span: Span,
        lets: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        let mut fields: Vec<crate::anon::ShapeField> = vec![];
        let mut hs = vec![];
        let mut posts = vec![];
        for (i, (name, v)) in entries.into_iter().enumerate() {
            // A field that may still be absent stays optional (`{ ...p }` with `p: Partial<T>`),
            // and keeps "absent" apart from `null` when a source does (`a?: T | null`).
            let (absent, presence) = (may_be_absent(&v), any_presence(&v));
            let h = if absent && presence {
                let rt = last_read_ty(&v).expect("ICE: an absent field is read");
                posts.push((i as u32, v));
                self.intrinsic(hir::Intrinsic::FieldAbsent, vec![], rt, span)
            } else {
                self.spread_value(v, None, span, lets)
            };
            let (declared, optional) = match self.cx.ty.opt_payload(h.ty) {
                _ if absent && presence => (h.ty, true),
                Some(p) if absent => (p, true),
                _ => (h.ty, false),
            };
            fields.push(crate::anon::ShapeField {
                name,
                ty: declared,
                readonly: false,
                optional,
            });
            hs.push(h);
        }
        let (def, type_args) = self.cx.anon_def_with(&fields, self.module);
        let ty = self.cx.ty.intern(TyKind::Adt(def, type_args.clone()));
        let kind = H::AdtLit {
            def,
            type_args,
            fields: hs,
        };
        let lit = self.mk(kind, ty, span);
        self.apply_posts(lit, posts, span, lets)
    }

    fn spread_struct(
        &mut self,
        d: DefId,
        mut slots: Vec<Option<TyId>>,
        entries: Vec<(String, Value)>,
        exp: Option<TyId>,
        span: Span,
        lets: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        let a = self.cx.adt(d).expect("ICE: struct");
        let (sname, fields, kind) = (a.name.clone(), a.fields.clone(), a.kind);
        let mut values: Vec<Option<hir::Expr>> = fields.iter().map(|_| None).collect();
        let mut posts = vec![];
        for (name, v) in entries {
            let i = fields.iter().position(|f| f.name == name);
            let h = match (v, i) {
                // Extra fields of a spread source are not part of the target type.
                (Value::Read(..) | Value::Over(..), None) => continue,
                // A target field that keeps "absent" apart from `null` (`a?: T | null`): left
                // absent in the literal, then set from each source that has it, in order.
                (v @ (Value::Read(..) | Value::Over(..)), Some(i))
                    if may_be_absent(&v)
                        && crate::anon::has_presence(&self.cx.ty, kind, &fields[i]) =>
                {
                    let expected = self.cx.subst_known(fields[i].ty, &slots);
                    posts.push((i as u32, v));
                    self.intrinsic(hir::Intrinsic::FieldAbsent, vec![], expected, span)
                }
                (v @ (Value::Read(..) | Value::Over(..)), Some(i)) => {
                    let expected = self.cx.subst_known(fields[i].ty, &slots);
                    self.spread_value(v, Some(expected), span, lets)
                }
                (Value::Prop(k, value), Some(i)) => {
                    self.check_private(fields[i].private_to, &k.name, k.span);
                    self.cx
                        .rec_ref(k.span, crate::ide::record::Target::Field(d, i as u32));
                    let expected = self.cx.subst_known(fields[i].ty, &slots);
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
        let lit = self.finish_struct(d, slots, values, exp, span);
        self.apply_posts(lit, posts, span, lets)
    }

    /// `lit`, with the fields in `posts` set afterwards from their sources in order: an explicit
    /// property or a field every source has always, an optional source field only when it is
    /// present (a presence flag, or not `null`). The literal goes to a temporary in `lets`.
    fn apply_posts(
        &mut self,
        lit: hir::Expr,
        posts: Vec<(u32, Value)>,
        span: Span,
        lets: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        if posts.is_empty() {
            return lit;
        }
        let ty = lit.ty;
        let field_tys: Vec<TyId> = match self.cx.ty.kind(ty).clone() {
            TyKind::Adt(d, args) => {
                let tys: Vec<TyId> = self
                    .cx
                    .adt(d)
                    .map(|a| a.fields.iter().map(|f| f.ty).collect())
                    .unwrap_or_default();
                tys.into_iter().map(|t| self.cx.subst(t, &args)).collect()
            }
            _ => return lit,
        };
        let l = self.new_local("<spread>", ty, true, span, LocalKind::Temp);
        lets.push(hir::Stmt {
            kind: S::Let {
                local: l,
                init: Some(lit),
            },
            span,
        });
        for (i, v) in posts {
            let fty = field_tys[i as usize];
            for part in chain(v) {
                let stmt = self.post_assign(l, ty, i, fty, part, span);
                lets.push(stmt);
            }
        }
        self.mk(H::Local(l, UseMode::Move), ty, span)
    }

    /// `tmp.f = value` for one part of a field's spread chain, guarded when the part may be
    /// absent.
    fn post_assign(
        &mut self,
        l: hir::LocalId,
        ty: TyId,
        index: u32,
        fty: TyId,
        part: Value,
        span: Span,
    ) -> hir::Stmt {
        let (value, guard) = match part {
            Value::Prop(k, value) => (self.prop_value(&k, value, Some(fty)), None),
            Value::Read(h, src) | Value::Over(h, src, _) => {
                let guard = match src.place {
                    Some(p) if src.presence => {
                        let b = self.cx.ty.bool_;
                        Some(self.intrinsic(hir::Intrinsic::FieldPresent, vec![p], b, span))
                    }
                    Some(p) if src.optional => Some(self.null_test(p, false, span)),
                    _ => None,
                };
                (self.coerce(h, fty), guard)
            }
        };
        let base = self.mk(H::Local(l, UseMode::Borrow), ty, span);
        let place = H::Field {
            base: Box::new(base),
            index,
            mode: UseMode::Borrow,
        };
        let place = self.mk(place, fty, span);
        let unit = self.cx.ty.unit;
        let assign = H::Assign {
            place: Box::new(place),
            value: Box::new(value),
        };
        let assign = self.mk(assign, unit, span);
        let Some(cond) = guard else {
            return hir::Stmt {
                kind: S::Expr(assign),
                span,
            };
        };
        let then = hir::Block {
            stmts: vec![hir::Stmt {
                kind: S::Expr(assign),
                span,
            }],
            value: None,
            span,
        };
        hir::Stmt {
            kind: S::If {
                cond,
                then,
                els: None,
            },
            span,
        }
    }

    /// The HIR value of a spread field (`expected`: the target field's type, if known).
    fn spread_value(
        &mut self,
        v: Value,
        expected: Option<TyId>,
        span: Span,
        lets: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        match v {
            Value::Read(h, _) => h,
            Value::Prop(k, value) => self.prop_value(&k, value, expected),
            Value::Over(h, src, prev) => {
                let prev = match *prev {
                    // A property under an optional field is evaluated whether or not the field
                    // is present, as in JavaScript: into a temporary, after the spread sources.
                    Value::Prop(k, value) => {
                        let p = self.prop_value(&k, value, expected);
                        let mode = if self.cx.is_copy(p.ty) {
                            UseMode::Copy
                        } else {
                            UseMode::Move
                        };
                        let mut t = self.temp("<spread>", p, lets);
                        set_place_mode(&mut t, mode);
                        t
                    }
                    prev => self.spread_value(prev, expected, span, lets),
                };
                if self.cx.ty.opt_payload(h.ty).is_none() {
                    return h;
                }
                match src.place {
                    // A present `null` overrides; only an absent field keeps the earlier value.
                    Some(p) if src.presence => {
                        let b = self.cx.ty.bool_;
                        let cond = self.intrinsic(hir::Intrinsic::FieldPresent, vec![p], b, span);
                        let ty = h.ty;
                        let prev = self.coerce(prev, ty);
                        let kind = H::If {
                            cond: Box::new(cond),
                            then: Box::new(h),
                            els: Box::new(prev),
                        };
                        self.mk(kind, ty, span)
                    }
                    _ => self.nullish_exprs(h, prev, span),
                }
            }
        }
    }
}

/// The parts of a field's spread value, earliest first (an `Over` unfolds into what it is
/// over, then itself).
fn chain(v: Value) -> Vec<Value> {
    match v {
        Value::Over(h, src, prev) => {
            let mut out = chain(*prev);
            out.push(Value::Read(h, src));
            out
        }
        v => vec![v],
    }
}

/// Every part of `v` is an optional source field, so the field may be absent.
fn may_be_absent(v: &Value) -> bool {
    match v {
        Value::Read(_, src) => src.optional,
        Value::Over(_, src, prev) => src.optional && may_be_absent(prev),
        Value::Prop(..) => false,
    }
}

/// Some part of `v` keeps "absent" apart from `null` (`a?: T | null`).
fn any_presence(v: &Value) -> bool {
    match v {
        Value::Read(_, src) => src.presence,
        Value::Over(_, src, prev) => src.presence || any_presence(prev),
        Value::Prop(..) => false,
    }
}

/// The type of the latest source read in `v`.
fn last_read_ty(v: &Value) -> Option<TyId> {
    match v {
        Value::Read(h, _) | Value::Over(h, _, _) => Some(h.ty),
        Value::Prop(..) => None,
    }
}
