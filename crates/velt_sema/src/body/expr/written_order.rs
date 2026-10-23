//! Object literals evaluate their properties in written order, as JS does, although a literal
//! of a struct holds its fields in declaration order (`AdtLit`), a spread's source is bound
//! before the literal is made and its fields are read where they land (`spread.rs`). Parts are
//! bound to temporaries in written order only when an effect could otherwise be seen out of
//! order (`eval_effects.rs`): literals without effects, and literals whose parts land in written
//! order, cost nothing.
//!
//! - In a literal of a struct type, `{ y: f(), x: g() }` for `{ x; y }`: each value that is not
//!   a constant, up to the last one with effects, is bound in written order
//!   (`bind_in_written_order`).
//! - In a literal with spreads, `{ a: f(), ...g() }`, `{ ...o, a: mutate(o) }` (`a` lands
//!   before `o`'s other fields), `{ ...o, b: k, a: bump() }`: each property that is not a
//!   constant and each field a spread reads, up to the last part with effects, is evaluated
//!   where it is written (`order_written`): a spread copies its source's fields before later
//!   properties run.

use velt_common::Span;
use velt_syntax::ast;

use super::eval_effects::Effects;
use super::spread::Value;
use crate::body::places::set_place_mode;
use crate::body::{FnCx, Want};
use crate::hir::{self, DefId, ExprKind as H, Intrinsic, StmtKind as S, TyId, UseMode};

/// A part of a spread literal, in written order.
pub(super) struct Part<'e> {
    pub(super) kind: PartKind<'e>,
    /// For a property, its effects; for a spread, those of evaluating its source (bound before
    /// the literal is made): `Writes` or `Reads`, as its fields are read.
    pub(super) effects: Effects,
    /// The number of statements bound before the literal when it is written (after its own,
    /// for a spread).
    pub(super) at: usize,
}

pub(super) enum PartKind<'e> {
    /// `key: value` or shorthand `key`.
    Prop(&'e ast::Ident, Option<&'e ast::Expr>),
    /// A spread, told by its span (the span of the reads of its fields).
    Spread(Span),
}

/// Where a part is evaluated in the literal as built, for comparing with written order: the
/// statements bound before the literal (an explicit property under a spread's optional field,
/// `Value::Over`), the literal's fields, or the fields set after it (`apply_posts`); then the
/// position among those.
type Rank = (u8, usize);

impl<'e> Part<'e> {
    pub(super) fn prop(
        key: &'e ast::Ident,
        value: Option<&'e ast::Expr>,
        effects: Effects,
        at: usize,
    ) -> Self {
        let kind = PartKind::Prop(key, value);
        Part { kind, effects, at }
    }
}

impl<'e> FnCx<'_, '_> {
    /// The [`Part`] of a spread told by `span` whose source bound `stmts`, `at` statements in.
    pub(super) fn spread_part(&mut self, span: Span, stmts: &[hir::Stmt], at: usize) -> Part<'e> {
        let writes = stmts.iter().any(|s| match &s.kind {
            S::Let { init: Some(h), .. } | S::Expr(h) => self.hir_effects(h) == Effects::Writes,
            _ => true,
        });
        let effects = match writes {
            true => Effects::Writes,
            false => Effects::Reads,
        };
        let kind = PartKind::Spread(span);
        Part { kind, effects, at }
    }

    /// Binds the parts of a spread literal that would otherwise be evaluated out of written
    /// order (see the module docs) into `lets`; `entries` then read them.
    pub(super) fn order_written(
        &mut self,
        parts: Vec<Part<'e>>,
        entries: &mut [(String, Value<'e>)],
        target: Option<&(DefId, Vec<Option<TyId>>)>,
        lets: &mut Vec<hir::Stmt>,
    ) {
        let Some(last) = parts.iter().rposition(|p| p.effects == Effects::Writes) else {
            return;
        };
        let fields = target.and_then(|(d, _)| self.cx.adt(*d).map(|a| (a.fields.clone(), a.kind)));
        let ranks: Vec<Option<Rank>> = (0..entries.len())
            .map(|e| self.entry_rank(entries, e, fields.as_ref()))
            .collect();
        if self.lands_in_order(&parts, entries, &ranks) {
            return;
        }
        let mut inserts: Vec<(usize, Vec<hir::Stmt>)> = vec![];
        for (k, part) in parts.into_iter().enumerate().take(last + 1) {
            let mut stmts = vec![];
            match part.kind {
                // A spread's source, bound already, comes before its fields.
                PartKind::Spread(_) if k == last => {}
                PartKind::Spread(span) => {
                    // A field the target type does not have is not read.
                    let read = entries.iter_mut().zip(&ranks).filter(|(_, r)| r.is_some());
                    for ((_, v), _) in read {
                        self.bind_reads(v, span, &mut stmts);
                    }
                }
                PartKind::Prop(..) if part.effects == Effects::Constant => {}
                PartKind::Prop(key, value) => {
                    let exp = target.and_then(|(_, slots)| {
                        let f = fields.as_ref()?.0.iter().find(|f| f.name == key.name)?;
                        Some(self.cx.subst_known(f.ty, slots))
                    });
                    let found = entries.iter_mut().find_map(|(_, v)| find_prop(v, key));
                    match found {
                        Some(v) => {
                            let h = self.prop_value(key, value, exp);
                            let mode = self.use_mode(h.ty, Want::Move);
                            let mut t = self.temp("<prop>", h, &mut stmts);
                            set_place_mode(&mut t, mode);
                            *v = Value::Held(key.clone(), value, t);
                        }
                        // Overridden by a later spread: evaluated for its effects only.
                        None if part.effects == Effects::Writes => {
                            let h = self.prop_value(key, value, exp);
                            let span = h.span;
                            stmts.push(hir::Stmt {
                                kind: S::Expr(h),
                                span,
                            });
                        }
                        None => {}
                    }
                }
            }
            if !stmts.is_empty() {
                inserts.push((part.at, stmts));
            }
        }
        insert_at(lets, inserts);
    }

    /// Does every part land in written order where it matters: nothing that is not a constant
    /// after one with effects, nor one with effects after anything that is not a constant?
    fn lands_in_order(
        &mut self,
        parts: &[Part<'e>],
        entries: &[(String, Value<'e>)],
        ranks: &[Option<Rank>],
    ) -> bool {
        // The latest rank of the parts so far that are not constants, and of those with effects.
        let (mut all, mut writes): (Option<Rank>, Option<Rank>) = (None, None);
        for part in parts {
            let (lo, hi, fx) = match part.kind {
                PartKind::Spread(span) => {
                    // A source with effects is bound before the literal, so before any part.
                    if part.effects == Effects::Writes && all.is_some() {
                        return false;
                    }
                    let mut range: Option<(Rank, Rank)> = None;
                    for (e, (_, v)) in entries.iter().enumerate() {
                        let Some(r) = ranks[e].filter(|_| reads_from(v, span)) else {
                            continue;
                        };
                        range = Some(range.map_or((r, r), |(lo, hi)| (lo.min(r), hi.max(r))));
                    }
                    let Some((lo, hi)) = range else { continue };
                    (lo, hi, Effects::Reads)
                }
                PartKind::Prop(..) if part.effects == Effects::Constant => continue,
                PartKind::Prop(key, _) => {
                    let at = entries.iter().enumerate().find_map(|(e, (_, v))| {
                        let r = ranks[e]?;
                        match v {
                            Value::Prop(k, _) if k.span == key.span => Some(r),
                            Value::Over(..) if holds_prop(v, key) => {
                                // Under a spread's field: evaluated into a temporary first,
                                // unless the field is set after the literal.
                                Some(if r.0 == 2 { r } else { (0, e) })
                            }
                            _ => None,
                        }
                    });
                    match at {
                        Some(r) => (r, r, part.effects),
                        // Overridden by a later spread: evaluated for its effects only.
                        None if part.effects == Effects::Writes => return false,
                        None => continue,
                    }
                }
            };
            let ok = match fx {
                Effects::Writes => all.is_none_or(|m| m < lo),
                _ => writes.is_none_or(|m| m < lo),
            };
            if !ok {
                return false;
            }
            all = all.max(Some(hi));
            if fx == Effects::Writes {
                writes = writes.max(Some(hi));
            }
        }
        true
    }

    /// Where entry `e` is evaluated (see [`Rank`]); `None` when it is not (a spread field the
    /// target type does not have). `fields`: the target type's fields and kind.
    fn entry_rank(
        &mut self,
        entries: &[(String, Value<'e>)],
        e: usize,
        fields: Option<&(Vec<crate::defs::FieldInfo>, hir::AdtKind)>,
    ) -> Option<Rank> {
        let (name, v) = &entries[e];
        let absent = super::spread::may_be_absent(v);
        let (post, at) = match fields {
            Some((fs, kind)) => {
                let i = fs.iter().position(|f| f.name == *name)?;
                (
                    absent && crate::anon::has_presence(&self.cx.ty, *kind, &fs[i]),
                    i,
                )
            }
            None => (absent && super::spread::any_presence(v), e),
        };
        Some(if post { (2, e) } else { (1, at) })
    }

    /// The reads of spread `span`'s fields in `v` bound to temporaries (appended to `stmts`),
    /// with what tells whether an optional one is there, so a later effect can't change them.
    fn bind_reads(&mut self, v: &mut Value<'e>, span: Span, stmts: &mut Vec<hir::Stmt>) {
        let (h, src) = match v {
            Value::Read(h, src) => (h, src),
            Value::Over(h, src, prev) => {
                self.bind_reads(prev, span, stmts);
                (h, src)
            }
            Value::Prop(..) | Value::Held(..) => return,
        };
        if h.span != span {
            return;
        }
        let bool_ = self.cx.ty.bool_;
        if let Some(g) = src.guard.take() {
            let mut t = self.temp("<present>", g, stmts);
            set_place_mode(&mut t, UseMode::Copy);
            src.guard = Some(t);
        } else if src.presence && src.place.is_some() {
            let p = src.place.take().expect("ICE: a place");
            let present = self.intrinsic(Intrinsic::FieldPresent, vec![p], bool_, span);
            let mut t = self.temp("<present>", present, stmts);
            set_place_mode(&mut t, UseMode::Copy);
            src.guard = Some(t);
        }
        let read = std::mem::replace(h, self.error_expr(span));
        let read = self.shared_read(read);
        let mode = self.use_mode(read.ty, Want::Move);
        let mut t = self.temp("<spread>", read, stmts);
        if src.place.is_some() {
            // An optional field is tested for `null` where it lands: test the copy.
            src.place = Some(t.clone());
        }
        set_place_mode(&mut t, mode);
        *h = t;
    }

    /// A field moved out of a spread source read as a share instead: binding it early must not
    /// leave the source half moved while a later property uses it.
    fn shared_read(&mut self, mut h: hir::Expr) -> hir::Expr {
        let H::Field { mode, .. } = &mut h.kind else {
            return h;
        };
        if *mode != UseMode::Move {
            return h;
        }
        *mode = UseMode::Borrow;
        let (ty, span) = (h.ty, h.span);
        self.intrinsic(Intrinsic::Share, vec![h], ty, span)
    }

    /// `values` of a struct literal of `props` by field index: when they would be evaluated out
    /// of written order (see the module docs), each value that is not a constant, up to the
    /// last one with effects, is bound in written order, by the statements returned.
    pub(super) fn bind_in_written_order(
        &mut self,
        values: &mut [Option<hir::Expr>],
        fields: &[crate::defs::FieldInfo],
        props: &[ast::ObjectProp],
    ) -> Vec<hir::Stmt> {
        let mut stmts = vec![];
        let written: Vec<usize> = (self.props(props).iter())
            .filter_map(|(k, _)| fields.iter().position(|f| f.name == k.name))
            .collect();
        let effects: Vec<Effects> = (values.iter())
            .map(|v| match v {
                Some(h) => self.hir_effects(h),
                None => Effects::Constant,
            })
            .collect();
        let Some(last) = written.iter().rposition(|&i| effects[i] == Effects::Writes) else {
            return stmts;
        };
        // Values are evaluated in declaration order: the field index is the rank.
        let (mut all, mut writes) = (None, None);
        let in_order = written.iter().all(|&i| {
            let ok = match effects[i] {
                Effects::Constant => return true,
                Effects::Writes => all.is_none_or(|m| m < i),
                Effects::Reads => writes.is_none_or(|m| m < i),
            };
            all = all.max(Some(i));
            if effects[i] == Effects::Writes {
                writes = writes.max(Some(i));
            }
            ok
        });
        if in_order {
            return stmts;
        }
        for &i in &written[..=last] {
            if effects[i] == Effects::Constant {
                continue;
            }
            let Some(h) = values[i].take() else { continue };
            let mode = self.use_mode(h.ty, Want::Move);
            let mut t = self.temp("<field>", h, &mut stmts);
            set_place_mode(&mut t, mode);
            values[i] = Some(t);
        }
        stmts
    }

    /// `no field `k` on type `sname``, unless the parser may have dropped its declaration.
    pub(super) fn no_such_field(&mut self, d: DefId, k: &ast::Ident, sname: &str) {
        if !crate::collect::exports::def_member_maybe_dropped(self.cx, d, &k.name) {
            let msg = format!("no field `{}` on type `{sname}`", k.name);
            self.cx.err(msg, k.span);
        }
    }
}

/// `stmts` with each batch of `inserts` (positions in `stmts` as it is, ascending) put in at
/// its position.
pub(super) fn insert_at(stmts: &mut Vec<hir::Stmt>, inserts: Vec<(usize, Vec<hir::Stmt>)>) {
    if inserts.is_empty() {
        return;
    }
    let old = std::mem::take(stmts);
    let mut inserts = inserts.into_iter().peekable();
    for (i, s) in old.into_iter().enumerate() {
        while let Some((_, batch)) = inserts.next_if(|(at, _)| *at <= i) {
            stmts.extend(batch);
        }
        stmts.push(s);
    }
    for (_, batch) in inserts {
        stmts.extend(batch);
    }
}

/// Does `v` read a field of the spread told by `span`?
fn reads_from(v: &Value, span: Span) -> bool {
    match v {
        Value::Read(h, _) => h.span == span,
        Value::Over(h, _, prev) => h.span == span || reads_from(prev, span),
        Value::Prop(..) | Value::Held(..) => false,
    }
}

/// Is the explicit property keyed `key` (told by its span) under the spread fields of `v`?
fn holds_prop(v: &Value, key: &ast::Ident) -> bool {
    match v {
        Value::Prop(k, _) => k.span == key.span,
        Value::Over(_, _, prev) => holds_prop(prev, key),
        Value::Read(..) | Value::Held(..) => false,
    }
}

/// The explicit property keyed `key` in `v` or in what `v` is over.
fn find_prop<'a, 'e>(v: &'a mut Value<'e>, key: &ast::Ident) -> Option<&'a mut Value<'e>> {
    match v {
        Value::Prop(k, _) if k.span == key.span => Some(v),
        Value::Over(_, _, prev) => find_prop(prev, key),
        _ => None,
    }
}
