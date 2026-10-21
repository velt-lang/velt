//! Spreads of a source that may be `undefined` or `null` (`...o` with `o: T | undefined`), for
//! `spread.rs`: JavaScript spreads nothing then.

use velt_common::Span;
use velt_syntax::ast;

use super::spread::Src;
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, UseMode};

impl FnCx<'_, '_> {
    /// `...h` with `h` an object that may be `undefined` or `null` (`T | undefined`, `T | null`;
    /// JavaScript spreads nothing then): its fields, each `undefined` while the source is
    /// missing and copied only when it is there (`Src::guard`). `Err` gives back any other
    /// value.
    pub(super) fn optional_source(
        &mut self,
        h: hir::Expr,
        span: Span,
        lets: &mut Vec<hir::Stmt>,
    ) -> Result<Vec<(String, hir::Expr, Src)>, hir::Expr> {
        let Some(core) = self.cx.ty.core(h.ty) else {
            return Err(h);
        };
        let plain = self.cx.ty.opt_payload(h.ty) == Some(core);
        let Some((d, args)) = self.adt_of(core).filter(|_| plain) else {
            return Err(h);
        };
        if self.record_args(core).is_some() || self.is_class_def(d) {
            return Err(h);
        }
        let opt = self.temp("<spread>", h, lets);
        let there = self.null_test(opt.clone(), false, span);
        let base = self.mk(H::UnwrapSome(Box::new(opt), UseMode::Borrow), core, span);
        let (fields, kind) = match self.cx.adt(d) {
            Some(a) => (a.fields.clone(), a.kind),
            None => return Ok(vec![]),
        };
        let hidden = self.cx.hidden_keys();
        let bool_ = self.cx.ty.bool_;
        let mut out = vec![];
        for (i, f) in fields.iter().enumerate() {
            if f.name.starts_with(ast::PRIVATE_NAME_PREFIX)
                || f.private_to.is_some_and(|o| !self.private_allowed(o))
                || hidden.contains(&f.name)
            {
                continue;
            }
            let fty = self.cx.subst(f.ty, &args);
            let rty = self.cx.ty.option_of(fty, hir::Nothing::Undefined);
            let read = self.field_read(&base, i as u32, fty, false, span);
            let read = self.coerce(read, rty);
            let missing = self.nothing_value(hir::Nothing::Undefined, rty, span);
            let value = H::If {
                cond: Box::new(there.clone()),
                then: Box::new(read),
                els: Box::new(missing),
            };
            let value = self.mk(value, rty, span);
            // An optional field is there only while present itself.
            let field = H::Field {
                base: Box::new(base.clone()),
                index: i as u32,
                mode: UseMode::Borrow,
            };
            let field = self.mk(field, fty, span);
            let own = match (f.optional, crate::anon::has_presence(&self.cx.ty, kind, f)) {
                (_, true) => self.intrinsic(hir::Intrinsic::FieldPresent, vec![field], bool_, span),
                (true, false) => self.null_test(field, false, span),
                (false, false) => self.mk(H::Lit(hir::Lit::Bool(true)), bool_, span),
            };
            let no = self.mk(H::Lit(hir::Lit::Bool(false)), bool_, span);
            let guard = H::If {
                cond: Box::new(there.clone()),
                then: Box::new(own),
                els: Box::new(no),
            };
            let src = Src {
                optional: true,
                presence: true,
                own: false,
                place: None,
                guard: Some(self.mk(guard, bool_, span)),
            };
            out.push((f.name.clone(), value, src));
        }
        Ok(out)
    }
}
