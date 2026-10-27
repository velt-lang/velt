//! `Atomic<T>` (std/prelude/sync.vlt, docs/internals/design/atomics.md): `new Atomic<T>(x)`
//! is the struct holding `x`, and a call of one of its methods is an `Atomic*` intrinsic whose
//! orderings are integer literals (hir/intrinsic.rs), so lowering emits the one instruction
//! with the ordering the call site names. The methods' declarations give the signatures, their
//! defaults and the type errors for an ordering an operation cannot have.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, Conversion, DefId, ExprKind as H, IntTy, Intrinsic, Lit, TyId, TyKind};

/// The orderings, as `Atomic*` intrinsics number them.
const ORDERS: [&str; 5] = ["relaxed", "acquire", "release", "acqrel", "seqcst"];

/// The read-modify-write methods, as `AtomicRmw` numbers them.
const RMW: [&str; 8] = ["exchange", "add", "sub", "and", "or", "xor", "max", "min"];

impl FnCx<'_, '_> {
    /// `new Atomic<T>(x)` once `mutex_new` found the class is the prelude's `Atomic`.
    pub(super) fn atomic_new(
        &mut self,
        d: DefId,
        class: &ast::TypeExpr,
        targs: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        // `(x: T) -> Atomic<T>`: `AtomicStore`'s value parameter, and its receiver type.
        let mut c = self.intrinsic_sig(Intrinsic::AtomicStore, span);
        c.what = "`Atomic` constructor".into();
        c.ret = c.params[0].ty;
        c.params = vec![c.params[1].clone()];
        let mut slots = vec![None];
        match targs {
            [] => {}
            [t] => slots[0] = Some(self.resolve(t)),
            _ => self.cx.err("`Atomic` takes 1 type argument", class.span),
        }
        let ck = self.check_call(&c, slots, args, self.hint(exp), span);
        let t = ck.type_args.first().copied().unwrap_or(self.cx.ty.error);
        self.atomic_value_ty(t, class.span);
        let kind = H::AdtLit {
            def: d,
            type_args: vec![t],
            fields: ck.args,
            converted: Conversion::No,
        };
        self.mk(kind, ck.ret, span)
    }

    /// Reports a `T` an `Atomic<T>` cannot hold; whether it can.
    fn atomic_value_ty(&mut self, t: TyId, span: Span) -> bool {
        match self.cx.ty.kind(t) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Error => true,
            _ => {
                let shown = self.cx.display(t);
                let d = Diagnostic::error(
                    format!("`Atomic<T>` needs a number type or `bool`, found `{shown}`"),
                    span,
                )
                .with_note(
                    "the compiler picks the instruction by the type, so it must be known here; for other values, use a `Mutex<T>`",
                );
                self.cx.error(d);
                false
            }
        }
    }

    /// The call of `Atomic`'s method `def` with the checked arguments `args` (the receiver
    /// first, for an instance method) as its intrinsic; `None` when `def` is not one of
    /// `Atomic`'s methods. Inside `Atomic`'s own declarations, the calls stay calls.
    pub(super) fn atomic_call(
        &mut self,
        def: DefId,
        args: &mut Vec<hir::Expr>,
        ret: TyId,
        span: Span,
    ) -> Option<hir::Expr> {
        let info = self.cx.fn_info(def);
        if info.owner.is_none() || info.owner != self.cx.atomic_ty() {
            return None;
        }
        let name = info.name.rsplit('.').next().unwrap_or("").to_string();
        let (i, values, op) = match name.as_str() {
            "load" => (Intrinsic::AtomicLoad, 0, None),
            "store" => (Intrinsic::AtomicStore, 1, None),
            "compareExchange" => (Intrinsic::AtomicCas, 2, None),
            "compareExchangeWeak" => (Intrinsic::AtomicCasWeak, 2, None),
            "fence" => (Intrinsic::AtomicFence, 0, None),
            "spinHint" => (Intrinsic::AtomicSpinHint, 0, None),
            n => {
                let op = RMW.iter().position(|m| *m == n)?;
                (Intrinsic::AtomicRmw, 1, Some(op as u8))
            }
        };
        let is_static = matches!(i, Intrinsic::AtomicFence | Intrinsic::AtomicSpinHint);
        let recv = usize::from(!is_static);
        if args.len() < recv + values {
            return None;
        }
        if !is_static {
            let t = self.cx.atomic_value(args[0].ty)?;
            if !self.atomic_value_ty(t, span) {
                return Some(self.error_expr(span));
            }
            let kind = self.cx.ty.kind(t);
            let int = matches!(kind, TyKind::Int(_));
            let float = matches!(kind, TyKind::Float(_));
            let wrong = match op.map(|o| RMW[o as usize]) {
                Some("add" | "sub" | "max" | "min") => (!int && !float).then_some("a number"),
                Some("and" | "or" | "xor") => float.then_some("an integer or `bool`"),
                _ => None,
            };
            if let Some(needs) = wrong {
                let shown = self.cx.display(args[0].ty);
                self.cx.err(
                    format!("`{name}` needs an `Atomic` of {needs}, found `{shown}`"),
                    span,
                );
                return Some(self.error_expr(span));
            }
        }
        let mut orders = Vec::new();
        for a in &args[recv + values..] {
            // A `string` is not an ordering: `check_call` reported it.
            if matches!(self.cx.ty.kind(a.ty), TyKind::Error) || a.ty == self.cx.ty.str_ {
                return Some(self.error_expr(span));
            }
            let Some(o) = self.ordering_of(a) else {
                self.cx.error(
                    Diagnostic::error("an ordering must be a string literal", a.span).with_note(
                        "the compiler picks the instruction for it: write \"relaxed\", \"acquire\", \"release\", \"acqrel\" or \"seqcst\"",
                    ),
                );
                return Some(self.error_expr(span));
            };
            orders.push(o);
        }
        // An omitted failure ordering is the success ordering without its release part.
        if matches!(i, Intrinsic::AtomicCas | Intrinsic::AtomicCasWeak) && orders.len() == 2 {
            let explicit_failure = !is_default(&args[recv + values + 1], span);
            if !explicit_failure {
                orders[1] = match orders[0] {
                    0 | 2 => 0,
                    1 | 3 => 1,
                    o => o,
                };
            }
        }
        let mut out: Vec<hir::Expr> = args.drain(..recv + values).collect();
        if let Some(op) = op {
            out.push(self.u8_lit(op, span));
        }
        for o in orders {
            out.push(self.u8_lit(o, span));
        }
        Some(self.intrinsic(i, out, ret, span))
    }

    /// The ordering an argument names (its index in `ORDERS`): a string literal, or a value
    /// whose type is one string literal type. A union of string literals is an enum, so a
    /// literal passed as one is the variant of its own literal type.
    fn ordering_of(&mut self, a: &hir::Expr) -> Option<u8> {
        let s = match &a.kind {
            H::Lit(Lit::Str(s)) => s.clone(),
            H::Variant { args, .. } if args.len() == 1 => self.single_string_literal(args[0].ty)?,
            _ => self.single_string_literal(a.ty)?,
        };
        ORDERS.iter().position(|o| *o == s).map(|o| o as u8)
    }

    fn single_string_literal(&mut self, t: TyId) -> Option<String> {
        match self.string_literals(t)?.as_slice() {
            [s] => Some(s.clone()),
            _ => None,
        }
    }

    fn u8_lit(&mut self, v: u8, span: Span) -> hir::Expr {
        let u8_ = self.cx.ty.intern(TyKind::Int(IntTy::U8));
        self.mk(H::Lit(Lit::Int(u128::from(v))), u8_, span)
    }
}

/// Is `a` the default a left-out argument of the call at `call` gets (placed with the call's
/// span, `placed_default`)?
fn is_default(a: &hir::Expr, call: Span) -> bool {
    a.span == call
}
