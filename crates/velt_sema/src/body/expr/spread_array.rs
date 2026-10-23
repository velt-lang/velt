//! Array spread `[a, ...xs, b]` (docs/reference/types.md "Objects, arrays, tuples and maps"),
//! desugared without new HIR into
//! `{ let out = with_capacity(len); out.push(a); for (e of xs) out.push(<share of e>); ...; out }`
//! (Copy elements are copied), each element converted to the literal's element type
//! (`const ns: Named[] = [...cs]` makes interface values of the `C`s). A string or a `Map` is the
//! array of its characters or entries (bound to a temporary first). A source that is an iterable
//! (`[...gen()]`) is a `for...of` pushing its values at its position (`body/consume.rs`).
//!
//! Everything is evaluated in written order, as in JS (`eval_effects.rs` tells what can be
//! moved). A spread array that is not a place is bound before the array is made (its length
//! sizes it). When that would move it past an element with effects (`[g(), ...f()]`), or move
//! its effects past an element that reads what they may change (`[n, ...bump()]`), those
//! elements are bound before it, in order. After an iterable (`[...gen(), ...f()]`), which runs
//! where it is written, and after a spread of a place that a later effect could change
//! (`[...xs, ...f()]`), it is bound where it is pushed (unless it is a constant,
//! `[...gen(), ...[1, 2]]`), and so is everything after it; the first allocation holds the
//! rest, and such a source makes room for exactly its length before its pushes (one exact
//! reallocation, `Intrinsic::ArrayReserve`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::eval_effects::Effects;
use crate::body::consume::{Consumable, Synth};
use crate::body::places::{is_place, set_place_mode};
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{
    self, BinOp, ExprKind as H, Intrinsic, Pat, PatKind, StmtKind as S, TyId, UseMode,
};

/// A source of array spread.
enum Src {
    /// An array (a place, or a temporary bound first) and its element type.
    Array(hir::Expr, TyId),
    /// An array value bound to a temporary after the elements with effects before it, before
    /// the array is made, and its element type.
    Unbound(hir::Expr, TyId),
    /// An array value bound to a temporary where it is pushed, after an iterable (see the
    /// module docs), and its element type.
    Late(hir::Expr, TyId),
    /// An iterable, consumed by a `for...of` at its position.
    Iter(Consumable),
}

impl FnCx<'_, '_> {
    /// Array literal with `...xs` elements (see the module docs).
    pub(super) fn spread_array(
        &mut self,
        elems: &[ast::Expr],
        exp_elem: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let mut lets = vec![];
        let mut sources: Vec<Option<Src>> = vec![];
        let mut elem = exp_elem;
        // An earlier element is an iterable (or see the module docs): a later one is evaluated
        // in place.
        let mut in_place = false;
        // An earlier spread of a place, read where it is pushed.
        let mut place_read = false;
        // Elements that are not constants not bound yet, and those bound before a spread
        // (`Unbound`).
        let mut pending: Vec<(usize, Effects)> = vec![];
        let mut bind = vec![false; elems.len()];
        for (i, e) in elems.iter().enumerate() {
            let ast::ExprKind::Spread(inner) = &e.kind else {
                if !in_place {
                    let fx = self.effects(e);
                    if fx != Effects::Constant {
                        pending.push((i, fx));
                    }
                }
                sources.push(None);
                continue;
            };
            let mut h = self.expr(inner, None, Want::Borrow);
            h = self.read_shared(h);
            if self.cx.ty.array_elem(h.ty).is_none() {
                match self.spread_iterable(h, inner) {
                    // A string's characters, a `Map`'s entries: spread as that array.
                    Some(c) if c.is_fresh() => h = c.into_fresh(),
                    Some(c) => {
                        elem.get_or_insert(c.elem);
                        sources.push(Some(Src::Iter(c)));
                        in_place = true;
                        continue;
                    }
                    None => {
                        sources.push(None);
                        continue;
                    }
                }
            }
            let et = self.cx.ty.array_elem(h.ty).expect("ICE: spread array");
            elem.get_or_insert(et);
            if is_place(&h) {
                place_read = true;
            }
            let fx = match is_place(&h) {
                true => Effects::Constant,
                false => self.hir_effects(&h),
            };
            let writes_before = pending.iter().any(|p| p.1 == Effects::Writes);
            let writes = fx == Effects::Writes || writes_before;
            let mut src = if is_place(&h) {
                h
            } else if fx == Effects::Constant {
                self.temp("<spread>", h, &mut lets)
            } else if in_place || (place_read && writes) {
                in_place = true;
                sources.push(Some(Src::Late(h, et)));
                continue;
            } else if writes_before
                || bind.contains(&true)
                || (fx == Effects::Writes && !pending.is_empty())
            {
                pending.drain(..).for_each(|(k, _)| bind[k] = true);
                bind[i] = true;
                sources.push(Some(Src::Unbound(h, et)));
                continue;
            } else {
                self.temp("<spread>", h, &mut lets)
            };
            set_place_mode(&mut src, UseMode::Borrow);
            sources.push(Some(Src::Array(src, et)));
        }
        let Some(elem) = elem else {
            return self.error_expr(span);
        };
        let mut plain: Vec<Option<hir::Expr>> = elems.iter().map(|_| None).collect();
        let elem = match exp_elem {
            Some(_) => elem,
            None => self.spread_plain_union(elems, &mut plain, elem, span),
        };
        for (e, src) in elems.iter().zip(&mut sources) {
            let (et, is_array) = match src {
                Some(Src::Array(_, et) | Src::Unbound(_, et) | Src::Late(_, et)) => (*et, true),
                Some(Src::Iter(c)) => (c.elem, false),
                None => continue,
            };
            // Integers spread into a float array are numbers too (`[...[1, 2]]` as `number[]`,
            // `Math.max(...ints())`). An array's elements convert like any other value of the
            // literal, fresh ones too (#268).
            // Function types that differ only in their error-set variables are one
            // (`throws::vars`).
            let fits = match is_array {
                true => self.spread_fits(et, elem),
                false => {
                    et == elem
                        || (self.cx.ty.is_int(et) && self.float_elem(elem).is_some())
                        || self.cx.unify_err_vars(et, elem)
                }
            };
            if !fits {
                // With the sets written in: the source's errors are added to the sites of the
                // element type, and the program is checked again with them.
                self.cx.fn_throws_differ(et, elem);
            }
            if !fits && !self.cx.ty.has_error(et) {
                let (from, to) = (self.cx.display(et), self.cx.display(elem));
                self.cx.err(
                    format!("cannot spread `{from}` elements into an array of `{to}`"),
                    e.span,
                );
                // Reported: its loop is not built (nor its conversion reported again).
                *src = None;
            }
        }
        // In written order: the elements with effects before a spread, and its value.
        for (i, e) in elems.iter().enumerate().filter(|&(i, _)| bind[i]) {
            match sources[i].take() {
                Some(Src::Unbound(h, et)) => {
                    let mut src = self.temp("<spread>", h, &mut lets);
                    set_place_mode(&mut src, UseMode::Borrow);
                    sources[i] = Some(Src::Array(src, et));
                }
                Some(other) => sources[i] = Some(other),
                None if matches!(e.kind, ast::ExprKind::Spread(_)) => {}
                None => {
                    let v = match plain[i].take() {
                        Some(h) => self.coerce(h, elem),
                        None => self.expr_coerce(e, elem, Want::Move),
                    };
                    let mode = self.use_mode(v.ty, Want::Move);
                    let mut t = self.temp("<elem>", v, &mut lets);
                    set_place_mode(&mut t, mode);
                    plain[i] = Some(t);
                }
            }
        }
        let arr_ty = self.cx.ty.array(elem);
        let srcs: Vec<Option<hir::Expr>> = sources
            .iter()
            .map(|s| match s {
                Some(Src::Array(h, _)) => Some(h.clone()),
                _ => None,
            })
            .collect();
        let cap = self.spread_capacity(elems, &srcs, span);
        let init = self.intrinsic(Intrinsic::ArrayWithCapacity, vec![cap], arr_ty, span);
        let iterables = sources.iter().any(|s| matches!(s, Some(Src::Iter(_))));
        // A spread iterable pushes from synthesized source, which names the array.
        let syn = iterables.then(|| {
            self.push_scope_until(span.hi);
            Synth::new(span, self.f.locals.len())
        });
        let out_l = match &syn {
            Some(syn) => self.hidden_local(syn.ident(&syn.array), init, true, &mut lets),
            None => {
                let out_l = self.new_local("<array>", arr_ty, true, span, LocalKind::Temp);
                lets.push(hir::Stmt {
                    kind: S::Let {
                        local: out_l,
                        init: Some(init),
                    },
                    span,
                });
                out_l
            }
        };
        for ((e, src), checked) in elems.iter().zip(sources).zip(plain) {
            let stmt = match (src, &e.kind) {
                (Some(Src::Array(src, et)), _) => {
                    self.push_all(out_l, arr_ty, src, (et, elem), e.span)
                }
                (Some(Src::Late(h, et) | Src::Unbound(h, et)), _) => {
                    let mut src = self.temp("<spread>", h, &mut lets);
                    set_place_mode(&mut src, UseMode::Borrow);
                    // Its length is known now: room for exactly it (the first allocation
                    // holds the rest).
                    let reserve = self.reserve_for(out_l, arr_ty, &src, e.span);
                    lets.push(reserve);
                    self.push_all(out_l, arr_ty, src, (et, elem), e.span)
                }
                (Some(Src::Iter(c)), _) => {
                    let syn = syn.as_ref().expect("ICE: spread names");
                    let mut v = syn.name(&syn.value);
                    if let Some(f) = self.float_elem(elem).filter(|_| c.elem != elem) {
                        // An integer into a float array: `<value#N> as f64`.
                        let ty = self.cx.display(f);
                        v = syn.expr(ast::ExprKind::Cast {
                            expr: Box::new(v),
                            ty: syn.named_type(&ty),
                        });
                    }
                    let push = syn.method(syn.name(&syn.array), "push", vec![v]);
                    self.consume(c, syn, vec![syn.expr_stmt(push)], &mut lets);
                    continue;
                }
                (None, ast::ExprKind::Spread(_)) => continue,
                (None, _) => {
                    let v = match checked {
                        Some(h) => self.coerce(h, elem),
                        None => self.expr_coerce(e, elem, Want::Move),
                    };
                    self.push_stmt(out_l, arr_ty, v)
                }
            };
            lets.push(stmt);
        }
        if syn.is_some() {
            self.pop_scope();
        }
        let out = self.mk(H::Local(out_l, UseMode::Move), arr_ty, span);
        self.with_lets(lets, out)
    }

    /// A spread source that is not an array: an iterable (`[...gen()]`), else an error.
    fn spread_iterable(&mut self, h: hir::Expr, inner: &ast::Expr) -> Option<Consumable> {
        let ty = h.ty;
        if self.is_consumable(ty) {
            return self.consumable(h);
        }
        if !self.cx.ty.is_bottom(ty) {
            let tn = self.cx.display(ty);
            self.cx.error(
                Diagnostic::error(
                    format!("cannot spread a value of type `{tn}` into an array"),
                    inner.span,
                )
                .with_note("what `for...of` iterates can be spread: arrays, strings, `Map`s, generators and iterables (values with a `[Symbol.iterator]()` method)"),
            );
        }
        None
    }

    /// `n_plain + xs.length + ...` as a `usize`.
    fn spread_capacity(
        &mut self,
        elems: &[ast::Expr],
        sources: &[Option<hir::Expr>],
        span: Span,
    ) -> hir::Expr {
        let usize_ = self.cx.ty.usize;
        let plain = elems
            .iter()
            .filter(|e| !matches!(e.kind, ast::ExprKind::Spread(_)))
            .count();
        let mut cap = self.mk(H::Lit(hir::Lit::Int(plain as u128)), usize_, span);
        for src in sources.iter().flatten() {
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

    /// `out.reserve(src.length);` (`Intrinsic::ArrayReserve`: one exact reallocation).
    fn reserve_for(
        &mut self,
        out: hir::LocalId,
        arr_ty: TyId,
        src: &hir::Expr,
        span: Span,
    ) -> hir::Stmt {
        let (usize_, unit) = (self.cx.ty.usize, self.cx.ty.unit);
        let len = self.intrinsic(Intrinsic::ArrayLen, vec![src.clone()], usize_, span);
        let target = self.mk(H::Local(out, UseMode::BorrowMut), arr_ty, span);
        let call = self.intrinsic(Intrinsic::ArrayReserve, vec![target, len], unit, span);
        hir::Stmt {
            kind: S::Expr(call),
            span,
        }
    }

    fn push_stmt(&mut self, out: hir::LocalId, arr_ty: TyId, v: hir::Expr) -> hir::Stmt {
        let target = self.mk(H::Local(out, UseMode::BorrowMut), arr_ty, v.span);
        self.push_onto(target, v)
    }

    /// `target.push(v);` (`target` used mutably).
    pub(super) fn push_onto(&mut self, target: hir::Expr, v: hir::Expr) -> hir::Stmt {
        let span = v.span;
        let unit = self.cx.ty.unit;
        let call = self.intrinsic(Intrinsic::ArrayPush, vec![target, v], unit, span);
        hir::Stmt {
            kind: S::Expr(call),
            span,
        }
    }

    /// Do the elements `et` of a spread array convert to the element type `elem`?
    pub(super) fn spread_fits(&mut self, et: TyId, elem: TyId) -> bool {
        et == elem
            || (self.cx.ty.is_int(et) && self.float_elem(elem).is_some())
            || self.cx.unify_err_vars(et, elem)
            || self.converts_to(et, elem)
            || self.widens(et, elem)
    }

    /// The float type of an array element type `number` or `number | null` (integers spread
    /// into it convert, as JS numbers).
    fn float_elem(&self, elem: TyId) -> Option<TyId> {
        let inner = self.cx.ty.core(elem).unwrap_or(elem);
        self.cx.ty.is_float(inner).then_some(inner)
    }

    /// `for (const e of src) out.push(e / share of e);`, each element converted from the
    /// source's element type to the literal's `elem` (integers to a float `elem`, `C`s to
    /// interface values in `const ns: Named[] = [...cs]`).
    fn push_all(
        &mut self,
        out: hir::LocalId,
        arr_ty: TyId,
        src: hir::Expr,
        types: (TyId, TyId),
        span: Span,
    ) -> hir::Stmt {
        let target = self.mk(H::Local(out, UseMode::BorrowMut), arr_ty, span);
        self.push_all_to(target, src, types, span)
    }

    /// `for (const e of src) target.push(e / share of e);` (see `push_all`).
    pub(super) fn push_all_to(
        &mut self,
        target: hir::Expr,
        src: hir::Expr,
        (src_elem, elem): (TyId, TyId),
        span: Span,
    ) -> hir::Stmt {
        let mode = self.elem_read_mode(src_elem);
        let e = self.new_local("<elem>", src_elem, false, span, LocalKind::Elem);
        let read = self.mk(H::Local(e, mode), src_elem, span);
        let value = self.spread_elem_value(read, (src_elem, elem));
        let push = self.push_onto(target, value);
        let binding = Pat {
            kind: PatKind::Binding(e, mode),
            ty: src_elem,
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
    /// How an element of type `t` of a spread source is read: copied, or borrowed and shared.
    pub(super) fn elem_read_mode(&mut self, t: TyId) -> UseMode {
        if self.cx.is_copy(t) {
            UseMode::Copy
        } else {
            UseMode::Borrow
        }
    }

    /// The element `read` (read with `elem_read_mode`) of a spread source, converted from the
    /// source's element type to `elem`: integers into a float `elem`, a share of a value that
    /// is not Copy.
    pub(super) fn spread_elem_value(
        &mut self,
        read: hir::Expr,
        (src_elem, elem): (TyId, TyId),
    ) -> hir::Expr {
        let span = read.span;
        let float = self
            .float_elem(elem)
            .filter(|_| self.cx.ty.is_int(src_elem));
        let value = if let Some(f) = float {
            // Into `number[]` or `(number | null)[]`: the number, then wrapped.
            self.mk(H::Cast(Box::new(read)), f, span)
        } else if self.cx.is_copy(src_elem) {
            read
        } else {
            self.intrinsic(Intrinsic::Share, vec![read], src_elem, span)
        };
        self.coerce(value, elem)
    }
}
