//! Array spread `[a, ...xs, b]` (docs/reference/types.md "Objects, arrays, tuples and maps"),
//! desugared without new HIR into
//! `{ let out = with_capacity(len); out.push(a); for (e of xs) out.push(<share of e>); ...; out }`
//! (Copy elements are copied), each element converted to the literal's element type
//! (`const ns: Named[] = [...cs]` makes interface values of the `C`s). A string or a `Map` is the
//! array of its characters or entries (bound to a temporary first). A source that is an iterable
//! (`[...gen()]`) is a `for...of` pushing its values at its position (`body/consume.rs`). Spread
//! sources are evaluated before the other elements of the literal.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

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
        for e in elems {
            let ast::ExprKind::Spread(inner) = &e.kind else {
                sources.push(None);
                continue;
            };
            let mut h = self.expr(inner, None, Want::Borrow);
            if self.cx.ty.array_elem(h.ty).is_none() {
                match self.spread_iterable(h, inner) {
                    // A string's characters, a `Map`'s entries: spread as that array.
                    Some(c) if c.is_fresh() => h = c.into_fresh(),
                    Some(c) => {
                        elem.get_or_insert(c.elem);
                        sources.push(Some(Src::Iter(c)));
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
            let mut src = if is_place(&h) {
                h
            } else {
                self.temp("<spread>", h, &mut lets)
            };
            set_place_mode(&mut src, UseMode::Borrow);
            sources.push(Some(Src::Array(src, et)));
        }
        let Some(elem) = elem else {
            return self.error_expr(span);
        };
        for (e, src) in elems.iter().zip(&mut sources) {
            let (et, is_array) = match src {
                Some(Src::Array(_, et)) => (*et, true),
                Some(Src::Iter(c)) => (c.elem, false),
                None => continue,
            };
            // Integers spread into a float array are numbers too (`[...[1, 2]]` as `number[]`,
            // `Math.max(...ints())`). An array's elements convert like any other value of the
            // literal, fresh ones too (#268).
            let fits = et == elem
                || (self.cx.ty.is_int(et) && self.float_elem(elem).is_some())
                || (is_array && (self.converts_to(et, elem) || self.widens(et, elem)));
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
        for (e, src) in elems.iter().zip(sources) {
            let stmt = match (src, &e.kind) {
                (Some(Src::Array(src, et)), _) => {
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
                    let v = self.expr_coerce(e, elem, Want::Move);
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
    /// source's element type to the literal's `elem` (integers to a float `elem`, `C`s to
    /// interface values in `const ns: Named[] = [...cs]`).
    /// The float type of an array element type `number` or `number | null` (integers spread
    /// into it convert, as JS numbers).
    fn float_elem(&self, elem: TyId) -> Option<TyId> {
        let inner = self.cx.ty.opt_payload(elem).unwrap_or(elem);
        self.cx.ty.is_float(inner).then_some(inner)
    }

    fn push_all(
        &mut self,
        out: hir::LocalId,
        arr_ty: TyId,
        src: hir::Expr,
        (src_elem, elem): (TyId, TyId),
        span: Span,
    ) -> hir::Stmt {
        let copy = self.cx.is_copy(src_elem);
        self.reject_promise_spread(src_elem, span);
        let mode = if copy { UseMode::Copy } else { UseMode::Borrow };
        let e = self.new_local("<elem>", src_elem, false, span, LocalKind::Elem);
        let read = self.mk(H::Local(e, mode), src_elem, span);
        let float = self
            .float_elem(elem)
            .filter(|_| self.cx.ty.is_int(src_elem));
        let value = if let Some(f) = float {
            // Into `number[]` or `(number | null)[]`: the number, then wrapped.
            self.mk(H::Cast(Box::new(read)), f, span)
        } else if copy {
            read
        } else {
            self.intrinsic(Intrinsic::Share, vec![read], src_elem, span)
        };
        let value = self.coerce(value, elem);
        let push = self.push_stmt(out, arr_ty, value);
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
}
