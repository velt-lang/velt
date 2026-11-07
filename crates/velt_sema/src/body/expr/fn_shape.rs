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
use crate::defs::FnSource;
use crate::hir::{self, DefId, Intrinsic, TyKind, UseMode};

/// What `length` counts of parameters given as `(has a default, optional, rest)`: the parser
/// gives an optional parameter a default, which leaves no trace in the JavaScript.
fn js_length(params: impl Iterator<Item = (bool, bool, bool)>) -> u32 {
    let counted = |&(default, optional, rest): &(bool, bool, bool)| !rest && (!default || optional);
    params.take_while(counted).count() as u32
}

/// `length` of a function declared with `params`.
fn decl_length(params: &[ast::Param]) -> u32 {
    let own = params.iter().filter(|p| p.name.name != "this");
    js_length(own.map(|p| (p.default.is_some(), p.optional, p.rest)))
}

impl Ctx<'_> {
    /// Notes `length` of the function declared at `span`.
    pub(crate) fn note_fn_length(&mut self, span: Span, params: &[ast::Param]) {
        let length = decl_length(params);
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

    /// `length` or `name` of declared function `d`.
    fn declared_fn_member(&mut self, d: DefId, prop: &ast::Ident, span: Span) -> Option<hir::Expr> {
        let f = self.cx.fn_info(d);
        if prop.name == "name" {
            let name = hir::declared_fn_name(&f.name).to_string();
            let ty = self.cx.ty.str_;
            return Some(self.mk(hir::ExprKind::Lit(hir::Lit::Str(name)), ty, span));
        }
        let Some(FnSource::Decl(decl)) = f.source else {
            return None;
        };
        let length = decl_length(&decl.sig.params);
        let lit = hir::Lit::Float(f64::from(length));
        let ty = self.cx.ty.f64;
        Some(self.mk(hir::ExprKind::Lit(lit), ty, span))
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

    /// The closure `arrow` is method `prop` read as a value: it has the method's name.
    pub(super) fn note_method_value(&mut self, arrow: &ast::Expr, prop: &ast::Ident) {
        let ast::ExprKind::Arrow { params, .. } = &arrow.kind else {
            return;
        };
        let (name, wraps) = (Some(prop.name.clone()), None);
        let length = params.len() as u32;
        let shape = hir::FnShape {
            name,
            length,
            wraps,
        };
        let entry = self.cx.fn_shapes.entry(arrow.span);
        // `o.m.bind(o)` has its name already.
        entry.and_modify(|s| s.length = length).or_insert(shape);
    }

    /// The forwarder at `span` is `f.bind(…)`, of `length` parameters: `bound f`, as in Node, of
    /// a function that has its name here.
    pub(super) fn note_bound_name(&mut self, f: &ast::Expr, span: Span, length: usize) {
        let f = super::method_value::strip_parens(f);
        let name = match &f.kind {
            ast::ExprKind::Ident(id) => &id.name,
            ast::ExprKind::Member { prop, .. } => &prop.name,
            _ => return,
        };
        let name = Some(format!("bound {name}"));
        let (length, wraps) = (length as u32, None);
        let shape = hir::FnShape {
            name,
            length,
            wraps,
        };
        self.cx.fn_shapes.entry(span).or_insert(shape);
    }

    /// The hidden `local` holding `h` for the wrapper at `span` (`adapt_fn`): calls through it
    /// are calls of a closure literal, and the wrapper reads as what `h` is, when that is known.
    pub(super) fn note_adapted(&mut self, local: hir::LocalId, h: &hir::Expr, span: Span) {
        if let hir::ExprKind::Closure(c) = &h.kind {
            self.f.closure_consts.insert(local, *c);
        }
        if let hir::ExprKind::Closure(d) | hir::ExprKind::FnRef(d, _) = &h.kind {
            self.note_wraps(span, *d);
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

    /// `prop` of `obj`, a value with no such field: `f.length` or `f.name` of a function value
    /// (read-only, as a write reports), else the error.
    pub(super) fn fn_own_member(
        &mut self,
        obj: &hir::Expr,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        let h = self.fn_own_read(obj, prop, want, span);
        if h.is_none() {
            self.no_field(obj.ty, obj.span, prop);
        }
        h
    }

    fn fn_own_read(
        &mut self,
        obj: &hir::Expr,
        prop: &ast::Ident,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        if want == Want::BorrowMut || !matches!(prop.name.as_str(), "length" | "name") {
            return None;
        }
        // A declared generic function: no value to look at, and known here.
        if let hir::ExprKind::FnRef(d, _) = obj.kind {
            return self.declared_fn_member(d, prop, span);
        }
        let is_fn = matches!(
            self.cx.ty.kind(obj.ty),
            TyKind::FnPtr { .. } | TyKind::Closure(_)
        );
        if !is_fn {
            return None;
        }
        let (op, ty) = match prop.name.as_str() {
            "length" => (Intrinsic::FnLength, self.cx.ty.f64),
            _ => (Intrinsic::FnName, self.cx.ty.str_),
        };
        let mut f = obj.clone();
        set_place_mode(&mut f, UseMode::Borrow);
        Some(self.intrinsic(op, vec![f], ty, span))
    }
}
