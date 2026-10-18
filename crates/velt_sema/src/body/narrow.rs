//! Flow narrowing of locals by the conditions of `if` / `while` / `&&` / `||` / ternaries:
//! - `T | null` locals by null tests (`x != null`, `x == null`) and truthiness (`if (x)`,
//!   `if (!x) return;`): a narrowed local reads as its payload (`ExprKind::UnwrapSome`);
//! - union locals by `typeof x === "tag"`, `x instanceof C` and `x == literal`: a local narrowed
//!   to one member reads as that member (`ExprKind::UnwrapVariant`);
//! - base class and interface locals (or such a union member) by `x instanceof C`: the local
//!   reads as the subclass `C` (`ExprKind::Downcast`).
//!
//! `!`, `&&` and `||` combine facts; narrowing lasts until the local is reassigned.

use velt_syntax::ast;

use super::FnCx;
use crate::hir::{DefId, LocalId, Nothing, TyId, TyKind};

/// What a condition establishes about a local.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Fact {
    /// The `T | null` local is not null.
    NonNull(LocalId),
    /// The `T | null | undefined` local is not this nothing (`x !== undefined`, `x !== null`);
    /// with both excluded it is narrowed as by `NonNull`.
    Excludes(LocalId, Nothing),
    /// The union local (or, for `U | null`, its payload when not null) is one of these variants.
    Members(LocalId, Vec<u32>),
    /// The class or interface local (or the union member it is narrowed to) holds an instance
    /// of this class type (`instanceof` on a base class or interface value; read through
    /// `ExprKind::Downcast`).
    Class(LocalId, TyId),
}

impl FnCx<'_, '_> {
    /// Facts that hold when `cond` is true / when it is false.
    pub fn narrowing(&mut self, cond: &ast::Expr) -> (Vec<Fact>, Vec<Fact>) {
        use ast::BinaryOp as B;
        if let Some(facts) = self.destructured_facts(cond, true) {
            return facts;
        }
        match &cond.kind {
            ast::ExprKind::Paren(inner) => self.narrowing(inner),
            ast::ExprKind::Unary {
                op: ast::UnaryOp::Not,
                expr,
            } => {
                let (t, f) = self.narrowing(expr);
                (f, t)
            }
            ast::ExprKind::Binary {
                op: B::And,
                lhs,
                rhs,
            } => {
                let (mut t, _) = self.narrowing(lhs);
                // `n instanceof Add && n.left instanceof Num`: the right side is read as narrowed
                // by the left one. A variable the right side assigns no longer holds what the
                // left side tested (`x instanceof D && (x = b) && …`).
                self.drop_assigned_facts(&mut t, rhs);
                let rhs_t = self.narrowing_under(&t, rhs).0;
                t.extend(rhs_t);
                (t, vec![])
            }
            ast::ExprKind::Binary {
                op: B::Or,
                lhs,
                rhs,
            } => {
                let (lhs_t, mut f) = self.narrowing(lhs);
                self.drop_assigned_facts(&mut f, rhs);
                let (rhs_t, rhs_f) = self.narrowing_under(&f, rhs);
                // True when either side is: what both sides establish (the right one read
                // after the left one failed), `x instanceof S || x instanceof S2` narrowing to
                // their nearest common class (#747).
                let mut right = f.clone();
                right.extend(rhs_t);
                let t = self.join_facts(&lhs_t, &right);
                f.extend(rhs_f);
                (t, f)
            }
            ast::ExprKind::Binary {
                op: op @ (B::Eq | B::NotEq | B::StrictEq | B::StrictNotEq),
                lhs,
                rhs,
            } => {
                let (t, f) = self.eq_facts(lhs, rhs, op.is_strict());
                if op.loose() == B::Eq {
                    (t, f)
                } else {
                    (f, t)
                }
            }
            ast::ExprKind::InstanceOf { expr, ty } => self.instanceof_facts(expr, ty),
            // `isFoo(x)` of a function declared `(v: …): v is Foo`.
            ast::ExprKind::Call {
                callee,
                args,
                optional: false,
                ..
            } => match self.dynamic_is_array_facts(callee, args) {
                Some(facts) => facts,
                None => self.predicate_facts(callee, args, cond.span),
            },
            // `#x in o` is `o instanceof C`, `C` the class declaring `#x`.
            ast::ExprKind::Binary {
                op: B::In,
                lhs,
                rhs,
            } => match self.brand_class(lhs, false) {
                Some(class) => self.class_facts(rhs, class),
                None => self.key_in_facts(lhs, rhs),
            },
            // `if (r.done)` on a union discriminated by a `bool` literal field.
            ast::ExprKind::Member {
                optional: false, ..
            }
            | ast::ExprKind::Index {
                optional: false, ..
            } if self.bool_discriminant(cond) => {
                let t = ast::SignedLit {
                    lit: ast::Lit::Bool(true),
                    negative: false,
                };
                self.discriminant_facts(cond, &t).unwrap_or_default()
            }
            ast::ExprKind::Ident(_)
            | ast::ExprKind::Member { .. }
            | ast::ExprKind::Index {
                optional: false, ..
            }
            | ast::ExprKind::Assign { op: None, .. } => (self.truthy_facts(cond), vec![]),
            _ => (vec![], vec![]),
        }
    }

    /// The facts that hold when either `a` or `b` does: per local, null tests both make, the
    /// union members either allows, and the nearest class both `instanceof` tests imply.
    fn join_facts(&mut self, a: &[Fact], b: &[Fact]) -> Vec<Fact> {
        let mut out = vec![];
        for fa in a {
            match fa {
                Fact::NonNull(_) | Fact::Excludes(..) => {
                    if b.contains(fa) && !out.contains(fa) {
                        out.push(fa.clone());
                    }
                }
                Fact::Members(l, va) => {
                    let mut vs = va.clone();
                    let mut seen = false;
                    for fb in b {
                        if let Fact::Members(m, vb) = fb {
                            if m == l {
                                seen = true;
                                vs.extend(vb.iter().copied());
                            }
                        }
                    }
                    if seen
                        && !out
                            .iter()
                            .any(|f| matches!(f, Fact::Members(m, _) if m == l))
                    {
                        vs.sort_unstable();
                        vs.dedup();
                        out.push(Fact::Members(*l, vs));
                    }
                }
                Fact::Class(l, ta) => {
                    let tb = b.iter().find_map(|f| match f {
                        Fact::Class(m, t) if m == l => Some(*t),
                        _ => None,
                    });
                    let done = out.iter().any(|f| matches!(f, Fact::Class(m, _) if m == l));
                    if let (Some(tb), false) = (tb, done) {
                        let c = if *ta == tb {
                            Some(tb)
                        } else {
                            self.common_class(*l, *ta, tb)
                        };
                        // `u instanceof Map || u instanceof Box` on an `unknown`: read through
                        // `any` either way (`dynamic_narrow`).
                        let c = c.or_else(|| self.dynamic_join(*l));
                        if let Some(c) = c {
                            out.push(Fact::Class(*l, c));
                        }
                    }
                }
            }
        }
        // A class fact on a union local holds for the one member it is narrowed to.
        let wide: Vec<LocalId> = out
            .iter()
            .filter_map(|f| match f {
                Fact::Members(l, vs) if vs.len() > 1 => Some(*l),
                _ => None,
            })
            .collect();
        out.retain(|f| !matches!(f, Fact::Class(l, _) if wide.contains(l)));
        out
    }

    /// The nearest class that class types `a` and `b` both extend, when it narrows local `l`
    /// (it is not the class `l` is declared with).
    fn common_class(&mut self, l: LocalId, a: TyId, b: TyId) -> Option<TyId> {
        let mut cur = a;
        let mut found = None;
        for _ in 0..64 {
            let (d, _) = self.cx.class_of(cur)?;
            if self.cx.is_instance_of(b, d) {
                found = Some((cur, d));
                break;
            }
            cur = self.cx.base_of(cur)?;
        }
        let (c, d) = found?;
        if c == a || c == b {
            // One test's class extends the other's: the latter was a valid target already.
            return Some(c);
        }
        if self.token_path(l).is_some() {
            return None;
        }
        let ty = self.local_ty(l);
        let ty = self.cx.ty.core(ty).unwrap_or(ty);
        let declared = self.cx.union_members(ty).unwrap_or_else(|| vec![ty]);
        let is_declared = declared
            .iter()
            .any(|t| self.cx.class_of(*t).is_some_and(|(e, _)| e == d));
        (!is_declared).then_some(c)
    }

    /// Drops from `facts` those about a variable `e` assigns, or about a field path of one.
    fn drop_assigned_facts(&mut self, facts: &mut Vec<Fact>, e: &ast::Expr) {
        let mut names = super::assigned::Assigned::new();
        super::assigned::assigned_in_expr(e, &mut names);
        if names.is_empty() {
            return;
        }
        let mut gone = vec![];
        for name in names.into_keys() {
            let found = self
                .f
                .scopes
                .iter()
                .rev()
                .find_map(|s| s.names.get(name).copied());
            if let Some(l) = found {
                gone.push(l);
                gone.extend(self.field_tokens_of(l));
            }
        }
        facts.retain(|f| {
            let l = match f {
                Fact::NonNull(l)
                | Fact::Members(l, _)
                | Fact::Class(l, _)
                | Fact::Excludes(l, _) => l,
            };
            !gone.contains(l)
        });
    }

    /// [`narrowing`](Self::narrowing) of `cond` with `facts` assumed.
    fn narrowing_under(&mut self, facts: &[Fact], cond: &ast::Expr) -> (Vec<Fact>, Vec<Fact>) {
        self.push_scope();
        for f in facts {
            self.narrow(f);
        }
        let out = self.narrowing(cond);
        self.pop_scope();
        out
    }

    /// Facts of `done == b` (`if (done)` for `b` true) where `done` is the discriminant binding
    /// of `const { value, done } = r` (`Frame::destructured_discriminants`): the bindings not
    /// null when it holds / does not hold `b`.
    fn destructured_facts(&mut self, e: &ast::Expr, b: bool) -> Option<(Vec<Fact>, Vec<Fact>)> {
        if !matches!(e.kind, ast::ExprKind::Ident(_)) {
            return None;
        }
        let l = self.named_local(e)?;
        let facts = self.f.destructured_discriminants.get(&l)?;
        let when = |v: bool| -> Vec<Fact> {
            facts
                .iter()
                .filter(|(w, _)| *w == v)
                .map(|(_, l)| Fact::NonNull(*l))
                .collect()
        };
        Some((when(b), when(!b)))
    }

    /// Facts of a nullable tested for truthiness (`if (user)`): when true, it is not null.
    fn truthy_facts(&mut self, e: &ast::Expr) -> Vec<Fact> {
        if let ast::ExprKind::Member {
            object,
            prop,
            optional: true,
        } = &e.kind
        {
            // `a?.b` is truthy: `a` is not null, and `a.b` is truthy (`if (def?.transform)`).
            let plain = ast::Expr {
                kind: ast::ExprKind::Member {
                    object: object.clone(),
                    prop: prop.clone(),
                    optional: false,
                },
                ..e.clone()
            };
            let mut facts = self.truthy_facts(object);
            facts.extend(self.truthy_facts(&plain));
            return facts;
        }
        if let Some(l) = self.nullable_local(e) {
            return vec![Fact::NonNull(l)];
        }
        if super::expr::member::member_view(e).is_some() {
            return self.field_token(e).map(Fact::NonNull).into_iter().collect();
        }
        vec![]
    }

    /// Facts of `lhs == rhs` (true / false).
    fn eq_facts(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        strict: bool,
    ) -> (Vec<Fact>, Vec<Fact>) {
        if is_null(lhs) || is_null(rhs) {
            let (other, word) = match nothing_word(rhs) {
                Some(w) => (lhs, w),
                None => (rhs, nothing_word(lhs).unwrap_or("null")),
            };
            // `x === null` on a `T | undefined` is constant: it narrows nothing.
            if strict && self.lacks_nothing(other, word) {
                return (vec![], vec![]);
            }
            // `x === null` / `x === undefined` on a `T | null | undefined` rules out one
            // nothing when false; both ruled out narrow the local to its value.
            if strict && self.three_state_operand(other) {
                let which = if word == "null" {
                    Nothing::Null
                } else {
                    Nothing::Undefined
                };
                let token = match self.nullable_local(other) {
                    Some(l) => Some(l),
                    None => self.field_token(other),
                };
                return match token {
                    Some(l) => (vec![], vec![Fact::Excludes(l, which)]),
                    None => (vec![], vec![]),
                };
            }
            return match self.nullable_local(other) {
                Some(l) => (vec![], vec![Fact::NonNull(l)]),
                None => match self.field_token(other) {
                    Some(t) => (vec![], vec![Fact::NonNull(t)]),
                    None => (vec![], vec![]),
                },
            };
        }
        if let Some((operand, tag)) = typeof_compare(lhs, rhs) {
            return self.typeof_facts(operand, tag);
        }
        let (other, lit) = match (literal_of(lhs), literal_of(rhs)) {
            (None, Some(l)) => (lhs, l),
            (Some(l), None) => (rhs, l),
            _ => return (vec![], vec![]),
        };
        if let ast::Lit::Bool(b) = lit.lit {
            if let Some(facts) = self.destructured_facts(other, b) {
                return facts;
            }
        }
        if let Some(facts) = self.discriminant_facts(other, &lit) {
            return facts;
        }
        let Some((l, nullable, u)) = self.union_local(other) else {
            return (vec![], vec![]);
        };
        let Some((v, _)) = self.lit_member_quiet(u, lit) else {
            return (vec![], vec![]);
        };
        let mut t = vec![Fact::Members(l, vec![v])];
        if nullable {
            t.push(Fact::NonNull(l));
        }
        (t, vec![])
    }

    /// Facts of `x.kind == lit` on a discriminated union local `x`: the members whose
    /// discriminant is `lit` / the others.
    fn discriminant_facts(
        &mut self,
        e: &ast::Expr,
        lit: &ast::SignedLit,
    ) -> Option<(Vec<Fact>, Vec<Fact>)> {
        let (object, prop) = super::expr::member::member_view(e)?;
        let l = self.named_local(object)?;
        let ty = self.local_ty(l);
        let nullable = self.cx.ty.opt_payload(ty).is_some();
        let u = self.cx.ty.core(ty).unwrap_or(ty);
        let values = self.cx.discriminant_values(u, &prop.name)?;
        let (yes, no): (Vec<u32>, Vec<u32>) = (0..values.len() as u32)
            .partition(|v| crate::literals::lit_matches(&values[*v as usize], lit));
        let mut t = vec![Fact::Members(l, yes)];
        if nullable {
            // `x.kind` could only be read once `x` was known to be non-null.
            t.push(Fact::NonNull(l));
        }
        Some((t, vec![Fact::Members(l, no)]))
    }

    /// Is `e` (`x.done`) a discriminant of a union local whose values are `bool` literals?
    fn bool_discriminant(&mut self, e: &ast::Expr) -> bool {
        let Some((object, prop)) = super::expr::member::member_view(e) else {
            return false;
        };
        let Some(l) = self.named_local(object) else {
            return false;
        };
        let ty = self.local_ty(l);
        let u = self.cx.ty.core(ty).unwrap_or(ty);
        self.cx
            .discriminant_values(u, &prop.name)
            .is_some_and(|vs| {
                vs.iter()
                    .all(|v| matches!(v, crate::hir::LitValue::Bool(_)))
            })
    }

    /// Facts of `KEY in x` / `"k" in x` on a union local: the members that may have the member
    /// when true (an optional one or a record's entry may), the members that may lack it when
    /// false.
    fn key_in_facts(&mut self, lhs: &ast::Expr, rhs: &ast::Expr) -> (Vec<Fact>, Vec<Fact>) {
        use crate::symbols::Presence;
        let Some(key) = self.in_key_of(lhs) else {
            return (vec![], vec![]);
        };
        let Some((l, false, u)) = self.local_with_members(rhs) else {
            return (vec![], vec![]);
        };
        let Some(members) = self.cx.union_members(u) else {
            return (vec![], vec![]);
        };
        let (mut yes, mut no) = (vec![], vec![]);
        for (i, m) in members.into_iter().enumerate() {
            // A value whose class is known only at run time may have it (`in_test`).
            let (p, held) = match self.in_test(m, &key, lhs.span, false) {
                Some((p, held)) => (Some(p), !held.is_empty()),
                None => (None, false),
            };
            if p != Some(Presence::No) || held {
                yes.push(i as u32);
            }
            if p != Some(Presence::Yes) {
                no.push(i as u32);
            }
        }
        (vec![Fact::Members(l, yes)], vec![Fact::Members(l, no)])
    }

    /// Facts of `typeof x === tag`.
    fn typeof_facts(&mut self, operand: &ast::Expr, tag: &str) -> (Vec<Fact>, Vec<Fact>) {
        // `any` / `unknown`: read as the primitive type the tag names.
        if let Some(l) = self.own_dynamic_local(operand) {
            {
                let t = match tag {
                    "number" => self.cx.ty.f64,
                    "string" => self.cx.ty.str_,
                    "boolean" => self.cx.ty.bool_,
                    // TypeScript's `Function` (issue #926): called through the descriptor.
                    "function" => return (self.dynamic_to_function_fact(operand), vec![]),
                    // TypeScript's `object | null` (issue #926), which is `object` here.
                    "object" => return (self.dynamic_to_object_fact(operand), vec![]),
                    _ => return (vec![], vec![]),
                };
                return (vec![Fact::Class(l, t)], vec![]);
            }
        }
        let Some((l, nullable, u)) = self.local_with_members(operand) else {
            return (vec![], vec![]);
        };
        let pred = |cx: &crate::ctx::Ctx, t: TyId| cx.typeof_tag(t) == tag;
        let nothing = self.cx.ty.nothing_of(self.local_ty(l));
        // A `T | null | undefined`: `"object"` also holds for its `null`, `"undefined"` only for
        // its `undefined`; the false branch rules that nothing out.
        if nothing == Some(Nothing::Both) {
            return match tag {
                "undefined" => (vec![], vec![Fact::Excludes(l, Nothing::Undefined)]),
                "object" => {
                    let (mut t, mut f) = self.split_facts(l, false, u, &pred, false);
                    t.push(Fact::Excludes(l, Nothing::Undefined));
                    f.push(Fact::Excludes(l, Nothing::Null));
                    (t, f)
                }
                _ => self.split_facts(l, nullable, u, &pred, false),
            };
        }
        let none_matches = match tag {
            "object" => nothing.is_none_or(|n| n.has_null()),
            "undefined" => nothing.is_some_and(|n| n.has_undefined()),
            _ => false,
        };
        self.split_facts(l, nullable, u, &pred, none_matches)
    }

    fn instanceof_facts(&mut self, e: &ast::Expr, ty: &ast::TypeExpr) -> (Vec<Fact>, Vec<Fact>) {
        if self.is_promise_ctor(ty) {
            let Some((l, nullable, u)) = self.local_with_members(e) else {
                return (vec![], vec![]);
            };
            let pred = |cx: &crate::ctx::Ctx, t: TyId| cx.ty.promise_payload(t).is_some();
            return self.split_facts(l, nullable, u, &pred, false);
        }
        match self.instanceof_class_quiet(ty) {
            Some(class) => self.class_facts(e, class),
            None => (vec![], vec![]),
        }
    }

    /// Facts for `e instanceof class`.
    pub(super) fn class_facts(&mut self, e: &ast::Expr, class: DefId) -> (Vec<Fact>, Vec<Fact>) {
        use super::expr::downcast::Instance;
        let found = match self.local_with_members(e) {
            Some(x) => Some(x),
            None => self
                .readonly_field_path(e)
                .map(|(token, t)| match self.cx.ty.core(t) {
                    Some(p) => (token, true, p),
                    None => (token, false, t),
                }),
        };
        let Some((l, nullable, u)) = found else {
            self.note_mutable_test(e);
            return (vec![], vec![]);
        };
        let members = self.cx.union_members(u);
        let parts = members.clone().unwrap_or_else(|| vec![u]);
        let kinds: Vec<Instance> = parts
            .iter()
            .map(|t| self.instance_kind(*t, class).unwrap_or(Instance::No))
            .collect();
        let (mut t, mut f) = (vec![], vec![]);
        if members.is_some() {
            let all = 0..kinds.len() as u32;
            let yes = all.clone().filter(|&i| kinds[i as usize] != Instance::No);
            let no = all.filter(|&i| kinds[i as usize] != Instance::Yes);
            t.push(Fact::Members(l, yes.collect()));
            f.push(Fact::Members(l, no.collect()));
        }
        if let Some(target) = kinds.iter().find_map(|k| match k {
            Instance::Maybe(target) => Some(*target),
            _ => None,
        }) {
            t.push(Fact::Class(l, target));
        }
        if nullable && kinds.iter().any(|k| *k != Instance::No) {
            t.push(Fact::NonNull(l));
        }
        (t, f)
    }

    /// Facts for a test that holds for the members satisfying `pred` (and for `null` iff
    /// `null_matches`).
    pub(super) fn split_facts(
        &mut self,
        l: LocalId,
        nullable: bool,
        u: TyId,
        pred: &dyn Fn(&crate::ctx::Ctx, TyId) -> bool,
        null_matches: bool,
    ) -> (Vec<Fact>, Vec<Fact>) {
        let (mut t, mut f) = (vec![], vec![]);
        if let Some(members) = self.cx.union_members(u) {
            let (yes, no): (Vec<u32>, Vec<u32>) =
                (0..members.len() as u32).partition(|&i| pred(self.cx, members[i as usize]));
            t.push(Fact::Members(l, yes));
            f.push(Fact::Members(l, no));
        } else if self.cx.ty.opt_payload(self.local_ty(l)).is_none()
            && typeof_exact(self.cx, u)
            && pred(self.cx, u)
        {
            // A test every value passes (`typeof x === "string"` on a `string`, which may be a
            // `string | bigint`): nothing is left where it fails, as in TypeScript. Only for
            // types whose `typeof` the static type fixes.
            f.push(Fact::Members(l, vec![]));
        }
        if nullable {
            if null_matches {
                f.push(Fact::NonNull(l));
            } else if self.cx.union_def(u).is_some() || pred(self.cx, u) {
                t.push(Fact::NonNull(l));
            }
        }
        (t, f)
    }

    /// A loop may run its body again after an assignment in it: narrowing of every local the
    /// loop assigns does not hold inside (or after) it.
    pub(crate) fn unnarrow_assigned_in(&mut self, lp: &ast::Stmt) {
        let mut names = super::assigned::Assigned::new();
        super::assigned::assigned_in_stmt(lp, &mut names);
        for name in names.into_keys() {
            let found = self
                .f
                .scopes
                .iter()
                .rev()
                .find_map(|s| s.names.get(name).copied());
            if let Some(l) = found {
                self.unnarrow(l);
            }
        }
    }

    /// A named local of the current function: (local, is `T | null`, `T`).
    pub(super) fn local_with_members(&mut self, e: &ast::Expr) -> Option<(LocalId, bool, TyId)> {
        let l = self.named_local(e)?;
        let ty = self.local_ty(l);
        Some(match self.cx.ty.core(ty) {
            Some(p) => (l, true, p),
            None => (l, false, ty),
        })
    }

    /// Like [`local_with_members`](Self::local_with_members), for union-typed locals only.
    fn union_local(&mut self, e: &ast::Expr) -> Option<(LocalId, bool, TyId)> {
        let found = self.local_with_members(e);
        found.filter(|(_, _, u)| self.cx.union_def(*u).is_some())
    }

    /// `x` / `this` naming an `Option`-typed local of the current function.
    /// Is `e` (a variable or a path of fields) of an option type without nothing `word`
    /// (`"null"` / `"undefined"`)?
    fn lacks_nothing(&mut self, e: &ast::Expr, word: &str) -> bool {
        let ty = match self.named_local(e) {
            Some(l) => Some(self.local_ty(l)),
            None => self.peek_ty(e),
        };
        ty.and_then(|t| self.cx.ty.nothing_of(t))
            .is_some_and(|n| match word {
                "undefined" => !n.has_undefined(),
                _ => !n.has_null(),
            })
    }

    fn three_state_operand(&mut self, e: &ast::Expr) -> bool {
        let ty = match self.named_local(e) {
            Some(l) => Some(self.local_ty(l)),
            None => self.peek_ty(e),
        };
        ty.is_some_and(|t| self.is_three_state(t))
    }

    fn nullable_local(&mut self, e: &ast::Expr) -> Option<LocalId> {
        let l = self.named_local(e)?;
        self.cx.ty.opt_payload(self.local_ty(l)).map(|_| l)
    }

    /// `x` / `this` naming a local of the current function. Inside a closure, a variable of an
    /// enclosing function names the closure's capture of it (created here if needed), so
    /// `(t) => want == null || t === want` narrows `want` in the closure body.
    pub(crate) fn named_local(&mut self, e: &ast::Expr) -> Option<LocalId> {
        let (name, span) = match &e.kind {
            ast::ExprKind::Ident(i) => (i.name.as_str(), i.span),
            ast::ExprKind::This => ("this", e.span),
            ast::ExprKind::Paren(inner) => return self.named_local(inner),
            // `(x = next()) != null` tests the value just stored in `x`.
            ast::ExprKind::Assign {
                op: None, target, ..
            } => return self.named_local(target),
            _ => return None,
        };
        let own = self
            .f
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.names.get(name).copied());
        match own {
            Some(l) => Some(l),
            None if self.is_local_name(name) => self.lookup_local(name, span),
            None => None,
        }
    }
}

/// `null` or `undefined` (possibly parenthesized): compared with either, a value of a type
/// with one nothing (`T | null`, `T | undefined`) tests for that nothing (issue #930).
pub(crate) fn is_null(e: &ast::Expr) -> bool {
    nothing_word(e).is_some()
}

/// `"null"` / `"undefined"` for those literals (possibly parenthesized).
pub(crate) fn nothing_word(e: &ast::Expr) -> Option<&'static str> {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Null) => Some("null"),
        ast::ExprKind::Lit(ast::Lit::Undefined) => Some("undefined"),
        ast::ExprKind::Paren(inner) => nothing_word(inner),
        _ => None,
    }
}

/// `typeof x` compared with a string literal (either order): (x, tag).
pub(crate) fn typeof_compare<'e>(
    lhs: &'e ast::Expr,
    rhs: &'e ast::Expr,
) -> Option<(&'e ast::Expr, &'e str)> {
    let operand = |e: &'e ast::Expr| match &e.kind {
        ast::ExprKind::Unary {
            op: ast::UnaryOp::TypeOf,
            expr,
        } => Some(&**expr),
        ast::ExprKind::Paren(inner) => match &inner.kind {
            ast::ExprKind::Unary {
                op: ast::UnaryOp::TypeOf,
                expr,
            } => Some(&**expr),
            _ => None,
        },
        _ => None,
    };
    let tag = |e: &'e ast::Expr| match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Str(s)) => Some(s.as_str()),
        _ => None,
    };
    match (operand(lhs), tag(rhs), tag(lhs), operand(rhs)) {
        (Some(x), Some(t), _, _) | (_, _, Some(t), Some(x)) => Some((x, t)),
        _ => None,
    }
}

/// A literal operand (`5`, `-2.5`, `"a"`, `true`) as a pattern literal.
pub(crate) fn literal_of(e: &ast::Expr) -> Option<ast::SignedLit> {
    match &e.kind {
        ast::ExprKind::Lit(l @ (ast::Lit::Int { .. } | ast::Lit::Float { .. })) => {
            Some(ast::SignedLit {
                lit: l.clone(),
                negative: false,
            })
        }
        ast::ExprKind::Lit(l @ (ast::Lit::Str(_) | ast::Lit::Bool(_))) => Some(ast::SignedLit {
            lit: l.clone(),
            negative: false,
        }),
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Neg,
            expr,
        } => match &expr.kind {
            ast::ExprKind::Lit(l @ (ast::Lit::Int { .. } | ast::Lit::Float { .. })) => {
                Some(ast::SignedLit {
                    lit: l.clone(),
                    negative: true,
                })
            }
            _ => None,
        },
        ast::ExprKind::Paren(inner) => literal_of(inner),
        _ => None,
    }
}

/// Does the static type `t` fix the run-time `typeof` of its values? Not for `object`,
/// `Function`, `any` / `unknown` or an interface value (which may hold a function), a type
/// parameter (any instantiation), an enum (a number or a string) or a deferred type.
fn typeof_exact(cx: &crate::ctx::Ctx, t: TyId) -> bool {
    let t = cx.brand_base(t).unwrap_or(t);
    match cx.ty.kind(t) {
        TyKind::Int(_)
        | TyKind::Float(_)
        | TyKind::Str
        | TyKind::Bool
        | TyKind::Symbol
        | TyKind::Literal(_)
        | TyKind::FnPtr { .. }
        | TyKind::Closure(_)
        | TyKind::Array(_)
        | TyKind::Tuple(_)
        | TyKind::Map(..)
        | TyKind::Promise(..) => true,
        TyKind::Adt(d, _) => cx.adt(*d).is_some(),
        _ => false,
    }
}
