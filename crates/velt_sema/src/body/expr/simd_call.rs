//! Static calls on the SIMD types (`velt/simd`): `u8x16.splat(x)` and the other static methods
//! their `extend` blocks declare, and `T.shuffle(a, b, …)`, whose literal lane indices become
//! part of the operation.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::ctx::Item;
use crate::hir::{self, Callee, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// The SIMD type `name` names in this module, where `item` is what it binds: the built-in
    /// type in std, elsewhere the `export type u8x16 = u8x16` of `velt/simd` it imports.
    pub(super) fn simd_type_named(&self, name: &str, item: Option<Item>) -> Option<hir::SimdTy> {
        match item {
            Some(Item::Alias(a)) => {
                let info = &self.cx.aliases[a as usize];
                let exported = self.cx.scopes[info.module].is_std && info.decl.generics.is_empty();
                exported
                    .then(|| hir::SimdTy::named(&info.decl.name.name))
                    .flatten()
            }
            None if self.cx.scopes[self.module].is_std => hir::SimdTy::named(name),
            _ => None,
        }
    }

    /// `u8x16.splat(x)`: a static method that an `extend` block of SIMD type `simd` declares.
    /// The SIMD types are primitives with constructors of their own, which `static_call` does
    /// not see.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn simd_static_call(
        &mut self,
        simd: hir::SimdTy,
        id: &ast::Ident,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        if let Some(first) = type_args.first() {
            let message = format!("`{}.{}` takes no type arguments", id.name, prop.name);
            self.cx.err(message, first.span);
        }
        let target = self.cx.ty.intern(TyKind::Simd(simd));
        if prop.name == "shuffle" {
            if let Some(lanes) = simd.lane_count() {
                return Some(self.simd_shuffle(simd, lanes, id, args, exp, span));
            }
        }
        let found = self.cx.extensions.iter().find_map(|x| {
            let m = x.methods.get(prop.name.as_str())?;
            (x.target == target).then_some(*m)
        });
        let Some(m) = found.filter(|m| m.is_static) else {
            let what = match found {
                Some(_) => "an instance method",
                None => "not a static method",
            };
            let mut d =
                Diagnostic::error(format!("`{}.{}` is {what}", id.name, prop.name), prop.span);
            // No extension of the type at all: its API (`velt/simd`) is not loaded, which a std
            // module that uses the built-in name without importing it gets.
            if !self.cx.extensions.iter().any(|x| x.target == target) {
                let name = simd.name();
                d = d.with_note(format!(
                    "the methods are loaded by importing the type: `import {{ {name} }} from \"velt/simd\";`"
                ));
            }
            self.cx.error(d);
            self.check_args_loose(args);
            return Some(self.error_expr(span));
        };
        self.cx
            .rec_ref(prop.span, crate::ide::record::Target::Def(m.def));
        let c = self.fn_callable(m.def, format!("`{}.{}`", id.name, prop.name), span);
        let slots = vec![None; c.slot_names.len()];
        let ck = self.check_call(&c, slots, args, exp, span);
        let kind = H::Call {
            callee: Callee::Def(m.def, ck.type_args),
            args: ck.args,
        };
        Some(self.mk(kind, ck.ret, span))
    }

    /// `T.shuffle(a, b, i0, …, iN)`: one lane index per lane of `T`, each an integer literal
    /// below twice the lane count (a lane of `a`, then of `b`). The indices become part of the
    /// operation (`Intrinsic::SimdShuffle`), as every backend needs a constant mask.
    fn simd_shuffle(
        &mut self,
        simd: hir::SimdTy,
        lanes: u32,
        id: &ast::Ident,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let what = format!("`{}.shuffle`", id.name);
        let wanted = 2 + lanes as usize;
        if args.len() != wanted {
            let message =
                format!("{what} takes {wanted} arguments: two vectors and {lanes} lane indices");
            self.cx.err(message, span);
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        let mut mask = [0u8; 16];
        let mut ok = true;
        for (k, arg) in args[2..].iter().enumerate() {
            match &arg.kind {
                ast::ExprKind::Lit(ast::Lit::Int { value, .. })
                    if *value < 2 * u128::from(lanes) =>
                {
                    mask[k] = *value as u8;
                }
                // Too large, or negative (`-1` is a literal too, below every lane).
                _ if negated_int(arg)
                    || matches!(arg.kind, ast::ExprKind::Lit(ast::Lit::Int { .. })) =>
                {
                    let below = if negated_int(arg) { ": below 0" } else { "" };
                    let message = format!(
                        "{what}: lane index out of range{below} (a lane of the first vector is 0–{}, of the second {}–{})",
                        lanes - 1,
                        lanes,
                        2 * lanes - 1
                    );
                    self.cx.err(message, arg.span);
                    ok = false;
                }
                _ => {
                    let d = Diagnostic::error(
                        format!("{what}: a lane index must be an integer literal"),
                        arg.span,
                    )
                    .with_note(
                        "the indices are compiled into the instruction, so they must be known when the program is compiled",
                    );
                    self.cx.error(d);
                    ok = false;
                }
            }
        }
        if !ok {
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        let i = hir::Intrinsic::SimdShuffle(simd, mask);
        let c = self.intrinsic_sig(i, span);
        let slots = vec![None; c.slot_names.len()];
        let ck = self.check_call(&c, slots, &args[..2], exp, span);
        let kind = H::Call {
            callee: Callee::Intrinsic(i),
            args: ck.args,
        };
        self.mk(kind, ck.ret, span)
    }
}

/// Whether `e` is a negated integer literal (`-1`).
fn negated_int(e: &ast::Expr) -> bool {
    matches!(&e.kind, ast::ExprKind::Unary { op: ast::UnaryOp::Neg, expr }
        if matches!(expr.kind, ast::ExprKind::Lit(ast::Lit::Int { .. })))
}
