//! `f.length` and `f.name` of a function value (#1588, hir_encodings.md "Function values"). A
//! value of a function type may hold any function of that type, so both are looked up at run
//! time by the value's code (`Intrinsic::FnLength`, `Intrinsic::FnName`) in what
//! `hir::Program::fn_shapes` says of each function:
//! - `length` counts the parameters before the first with a default or a rest parameter; an
//!   optional one counts, as TypeScript leaves no trace of it in the JavaScript.
//! - `name` is a declared function's own, a function literal's given name, or the one JavaScript
//!   infers from where the literal stands (`ast::Module::fn_names`).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::places::set_place_mode;
use crate::body::{FnCx, Want};
use crate::ctx::Ctx;
use crate::hir::{self, DefId, Intrinsic, TyKind, UseMode};

/// What `length` counts of parameters given as `(has a default, optional, rest)`: the parser
/// gives an optional parameter a default, which leaves no trace in the JavaScript.
fn js_length(params: impl Iterator<Item = (bool, bool, bool)>) -> u32 {
    let counted = |&(default, optional, rest): &(bool, bool, bool)| !rest && (!default || optional);
    params.take_while(counted).count() as u32
}

impl Ctx<'_> {
    /// Notes `length` of the function declared at `span`.
    pub(crate) fn note_fn_length(&mut self, span: Span, params: &[ast::Param]) {
        let own = params.iter().filter(|p| p.name.name != "this");
        let length = js_length(own.map(|p| (p.default.is_some(), p.optional, p.rest)));
        let (name, wraps) = (None, None);
        let shape = hir::FnShape {
            name,
            length,
            wraps,
        };
        self.fn_shapes.entry(span).or_insert(shape);
    }
}

impl FnCx<'_, '_> {
    /// Notes what the function literal `e` says of itself. What sema makes from a literal to
    /// fit a function type has its span, and comes after it: the first note stays.
    pub(super) fn note_closure_shape(&mut self, e: &ast::Expr) {
        let ast::ExprKind::Arrow { params, .. } = &e.kind else {
            return;
        };
        if self.cx.fn_shapes.contains_key(&e.span) {
            return;
        }
        let own = params.iter().filter(|p| p.name.name != "this");
        let length = js_length(own.map(|p| (p.default.is_some(), p.optional, p.rest)));
        let name = self.cx.modules[self.module].ast.fn_name(e.span);
        let name = name.map(str::to_string);
        let wraps = None;
        let shape = hir::FnShape {
            name,
            length,
            wraps,
        };
        self.cx.fn_shapes.insert(e.span, shape);
    }

    /// The wrapper at `span` stands for function `wrapped` (the function `callee` names when it
    /// isn't given), when that is known here (`callback::wrap`).
    pub(super) fn note_wrapper_shape(
        &mut self,
        span: Span,
        wrapped: Option<DefId>,
        callee: &ast::Expr,
    ) {
        let named = |cx: &Ctx, module, name: &ast::Ident| match cx.lookup_item(module, &name.name) {
            Some(crate::ctx::Item::Def(d))
                if matches!(cx.info[d.0 as usize], crate::defs::DefInfo::Fn(_)) =>
            {
                Some(d)
            }
            _ => None,
        };
        let wrapped = wrapped.or(match &callee.kind {
            ast::ExprKind::Ident(name) => named(self.cx, self.module, name),
            _ => None,
        });
        if let Some(d) = wrapped {
            self.note_wraps(span, d);
        }
    }

    /// The wrapper at `span` stands for function `d`.
    pub(super) fn note_wraps(&mut self, span: Span, d: DefId) {
        let (name, length, wraps) = (None, 0, Some(d));
        let shape = hir::FnShape {
            name,
            length,
            wraps,
        };
        self.cx.fn_shapes.entry(span).or_insert(shape);
    }

    /// `f.length` or `f.name` of the function value `obj`; `None` for anything else. Both are
    /// read-only, as a write to one reports.
    pub(super) fn fn_own_member(
        &mut self,
        obj: &hir::Expr,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        let is_fn = matches!(
            self.cx.ty.kind(obj.ty),
            TyKind::FnPtr { .. } | TyKind::Closure(_)
        );
        if !is_fn || want == Want::BorrowMut {
            return None;
        }
        let (op, ty) = match prop.name.as_str() {
            "length" => (Intrinsic::FnLength, self.cx.ty.f64),
            "name" => (Intrinsic::FnName, self.cx.ty.str_),
            _ => return None,
        };
        let mut f = obj.clone();
        set_place_mode(&mut f, UseMode::Borrow);
        Some(self.intrinsic(op, vec![f], ty, span))
    }
}
