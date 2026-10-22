//! Classes as values (#943): a class named where a value of a construct signature type
//! (`new (buf: ArrayBuffer) => ArrayBufferView`, `crate::fn_types`) is expected, and `new C(…)`,
//! `v instanceof C` and `C.name` through such a value.
//!
//! The value is the object `{ #new: (#arg0, …) => new C(#arg0, …), #is: (#v) => #v instanceof
//! C, #name: "C" }`, checked as a literal of the construct signature's type: the constructor call
//! follows the signature's parameters (a class whose constructor doesn't take them is an error at
//! the class's name), and its instance converts to the signature's instance type. Its fields
//! are hidden, like a callable object's function (`crate::callables`).
//!
//! A class has one value per signature type, as JavaScript has one class object: a constant
//! of the module state, made when the class is first named as a value of that type (types that
//! differ only in their inferred error sets, `throws::vars`, share it) and initialized before
//! any module runs (it reads nothing), so `===`, `includes`, `Map` and `Set` keys see one
//! object. Only a signature type over type parameters (in a generic function) makes the
//! object at each use.
//!
//! - `new C(args)` with a variable, a module constant, a member of one or a class's static
//!   field `C` of that type calls `C.#new(args)`;
//! - `v instanceof C` calls `C.#is(v)` (`v` converts to `unknown`) and narrows `v` to the
//!   signature's class;
//! - `C.name` reads `C.#name`; `typeof C` is `"function"` (`Ctx::typeof_tag`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::callback::synth;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::defs::{BodyState, DefInfo, GlobalInfo, GlobalSrc};
use crate::fn_types::{IS, NAME, NEW};
use crate::hir::{self, AdtKind, DefId, TyId, TyKind};

fn ident(name: &str, span: Span) -> ast::Ident {
    ast::Ident {
        name: name.into(),
        span,
    }
}

/// The key of a hidden field: no source spells it (an empty span, as `crate::callables` keys).
fn hidden(name: &str, span: Span) -> ast::Ident {
    ident(name, Span::new(span.file, span.lo, span.lo))
}

fn name_expr(name: &str, span: Span) -> ast::Expr {
    synth(ast::ExprKind::Ident(ident(name, span)), span)
}

/// Class `name` as written after `new` or `instanceof`.
fn class_type(name: &str, span: Span) -> ast::TypeExpr {
    ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: vec![ident(name, span)],
            args: vec![],
        },
        span,
    }
}

/// `root.members…` as an expression.
fn member_chain(root: &ast::Ident, members: &[ast::Ident]) -> ast::Expr {
    let mut e = name_expr(&root.name, root.span);
    for m in members {
        let span = e.span.to(m.span);
        let kind = ast::ExprKind::Member {
            object: Box::new(e),
            prop: m.clone(),
            optional: false,
        };
        e = synth(kind, span);
    }
    e
}

fn arrow(params: Vec<ast::ArrowParam>, body: ast::Expr, span: Span) -> ast::Expr {
    synth(
        ast::ExprKind::Arrow {
            type_params: vec![],
            params,
            ret: None,
            throws: None,
            body: ast::ArrowBody::Expr(Box::new(body)),
            is_async: false,
        },
        span,
    )
}

fn param(name: ast::Ident, rest: bool) -> ast::ArrowParam {
    ast::ArrowParam {
        name,
        pattern: None,
        ty: None,
        default: None,
        optional: false,
        rest,
    }
}

impl FnCx<'_, '_> {
    /// Class `d`, named by `id`, as a value of type `exp` (module docs); `None` when `d` is not
    /// a class.
    pub(super) fn class_value(
        &mut self,
        d: DefId,
        id: &ast::Ident,
        exp: Option<TyId>,
        want: Want,
    ) -> Option<hir::Expr> {
        let a = self.cx.adt(d).filter(|a| a.kind == AdtKind::Class)?;
        let class = a.name.clone();
        let span = id.span;
        let target = exp
            .map(|t| self.cx.ty.core(t).unwrap_or(t))
            .filter(|t| self.cx.ctor_sig(*t).is_some());
        let Some(target) = target else {
            self.cx.error(
                Diagnostic::error(
                    format!("class `{}` used as a value needs a construct signature type", id.name),
                    span,
                )
                .with_note(format!(
                    "Velt passes a class as a value where a construct signature is expected: `const C: new (x: T) => {class} = {};` (#943)",
                    id.name
                )),
            );
            return Some(self.error_expr(span));
        };
        let f = self.cx.ctor_sig(target)?;
        if let Some(g) = self.class_value_global(d, target, f, span) {
            let read = self.global_read(g, want, span);
            return Some(self.coerce(read, target));
        }
        let lit = self.class_value_lit(&id.name, &class, f, span)?;
        Some(self.expr(&lit, Some(target), want))
    }

    /// The constant holding class `d`'s value of type `target` (module docs), made on first
    /// use; `None` for a type over type parameters, or a class not named at its module's top
    /// level.
    fn class_value_global(&mut self, d: DefId, target: TyId, f: TyId, span: Span) -> Option<DefId> {
        if self.cx.has_type_params(target) || self.cx.ty.has_error(target) {
            return None;
        }
        let a = self.cx.adt(d)?;
        let (module, name, qual, decl) = (a.module, a.name.clone(), a.qual_name.clone(), a.span);
        if !matches!(self.cx.lookup_item(module, &name), Some(Item::Def(x)) if x == d) {
            return None;
        }
        let key = match self.cx.fn_sites.use_vars {
            true => (d, self.cx.plain_type(target)),
            false => (d, target),
        };
        let values = &self.cx.objects.class_values;
        if let Some((_, g)) = values.iter().find(|(k, _)| *k == key) {
            return Some(*g);
        }
        let lit = self.class_value_lit(&name, &name, f, span)?;
        let info = GlobalInfo {
            name: format!("{name}#value"),
            qual_name: format!("{qual}#value"),
            module,
            span: Span::DUMMY,
            ty: target,
            init: None,
            state: BodyState::InProgress,
            src: GlobalSrc {
                ann: None,
                init: None,
                span: decl,
                owner: None,
                is_private: false,
            },
            mutable: false,
            stored: true,
        };
        let g = self
            .cx
            .alloc_def(Span::DUMMY, DefInfo::Global(Box::new(info)));
        self.cx.objects.class_values.push((key, g));
        let init = {
            let mut fcx = crate::body::driver::detached(self.cx, module, &[]);
            fcx.fn_name = format!("{qual}#value");
            fcx.expr(&lit, Some(target), Want::Move)
        };
        if let DefInfo::Global(info) = &mut self.cx.info[g.0 as usize] {
            info.init = Some(init);
            info.state = BodyState::Done;
        }
        Some(g)
    }

    /// The object literal of the value of the class named `class` here, called `shown`, whose
    /// `#new` has function type `f` (module docs).
    fn class_value_lit(
        &mut self,
        class: &str,
        shown: &str,
        f: TyId,
        span: Span,
    ) -> Option<ast::Expr> {
        let TyKind::FnPtr { params, rest, .. } = self.cx.ty.kind(f).clone() else {
            return None;
        };
        let n = params.len();
        let arg = |k: usize| ident(&format!("#arg{k}"), span);
        let ps = (0..n).map(|k| param(arg(k), rest && k + 1 == n)).collect();
        let args = (0..n)
            .map(|k| {
                let e = synth(ast::ExprKind::Ident(arg(k)), span);
                match rest && k + 1 == n {
                    true => synth(ast::ExprKind::Spread(Box::new(e)), span),
                    false => e,
                }
            })
            .collect();
        let new = synth(
            ast::ExprKind::New {
                class: class_type(class, span),
                args,
            },
            span,
        );
        let test = synth(
            ast::ExprKind::InstanceOf {
                expr: Box::new(name_expr("#v", span)),
                ty: class_type(class, span),
            },
            span,
        );
        let name = synth(ast::ExprKind::Lit(ast::Lit::Str(shown.into())), span);
        let lit = synth(
            ast::ExprKind::Object(vec![
                ast::ObjectProp::KeyValue(hidden(NEW, span), arrow(ps, new, span)),
                ast::ObjectProp::KeyValue(
                    hidden(IS, span),
                    arrow(vec![param(ident("#v", span), false)], test, span),
                ),
                ast::ObjectProp::KeyValue(hidden(NAME, span), name),
            ]),
            span,
        );
        Some(lit)
    }

    /// `X` after `new` or `instanceof` naming a variable or module constant of a construct
    /// signature type, or a member of a value (`new reg.ctor(x)`, `new Reg.DEF(x)` of a static
    /// field: a value can't name a class): that value as an expression.
    fn ctor_value_named(&mut self, ty: &ast::TypeExpr) -> Option<ast::Expr> {
        if let Some((e, _)) = self.ctor_value_sig(ty) {
            return Some(e);
        }
        let (root, members) = self.value_path(ty)?;
        Some(member_chain(root, members))
    }

    /// Facts of `e instanceof C` through a class value `C` (for flow narrowing): when true, `e`
    /// is an instance of the class the signature constructs (`C` holds a subclass of it), as
    /// TypeScript narrows; when false, nothing.
    pub(crate) fn ctor_value_facts(
        &mut self,
        e: &ast::Expr,
        ty: &ast::TypeExpr,
    ) -> (
        Vec<crate::body::narrow::Fact>,
        Vec<crate::body::narrow::Fact>,
    ) {
        let class = self
            .ctor_value_sig(ty)
            .and_then(|(_, f)| match self.cx.ty.kind(f) {
                TyKind::FnPtr { ret, .. } => self.cx.class_of(*ret).map(|(d, _)| d),
                _ => None,
            });
        match class {
            Some(c) => (self.class_facts(e, c).0, vec![]),
            None => (vec![], vec![]),
        }
    }

    /// [`Self::ctor_value_named`], with the signature's function type, when the value has one.
    fn ctor_value_sig(&mut self, ty: &ast::TypeExpr) -> Option<(ast::Expr, TyId)> {
        let ast::TypeExprKind::Named { path, args } = &ty.kind else {
            return None;
        };
        let ([name], true) = (path.as_slice(), args.is_empty()) else {
            let (root, members) = self.value_path(ty)?;
            let t = self.value_path_ty(root, members)?;
            let core = self.cx.ty.core(t).unwrap_or(t);
            let f = self.cx.ctor_sig(core)?;
            return Some((member_chain(root, members), f));
        };
        let t = match self.peek_local_ty(&name.name) {
            Some(t) => t,
            None => self.constant_ty(name)?,
        };
        let core = self.cx.ty.core(t).unwrap_or(t);
        let f = self.cx.ctor_sig(core)?;
        Some((name_expr(&name.name, name.span), f))
    }

    /// The type of module constant `name` (`None`: it names no constant).
    fn constant_ty(&mut self, name: &ast::Ident) -> Option<TyId> {
        match self.cx.lookup_item_at(self.module, &name.name, name.span)? {
            Item::Def(d) if matches!(self.cx.info[d.0 as usize], DefInfo::Global(_)) => {
                crate::body::driver::ensure_global(self.cx, d);
                Some(self.cx.global(d)?.ty)
            }
            _ => None,
        }
    }

    /// `X.a.b` as written after `new` or `instanceof`, when `X` is a value (a variable or
    /// module constant) or a class with a static field `a`: `X` and the members.
    fn value_path<'t>(
        &mut self,
        ty: &'t ast::TypeExpr,
    ) -> Option<(&'t ast::Ident, &'t [ast::Ident])> {
        let ast::TypeExprKind::Named { path, args } = &ty.kind else {
            return None;
        };
        let (true, [root, members @ ..]) = (args.is_empty(), path.as_slice()) else {
            return None;
        };
        if members.is_empty() {
            return None;
        }
        if self.peek_local_ty(&root.name).is_some() {
            return Some((root, members));
        }
        let Item::Def(d) = self.cx.lookup_item_at(self.module, &root.name, root.span)? else {
            return None;
        };
        let value = match &self.cx.info[d.0 as usize] {
            DefInfo::Global(_) => true,
            DefInfo::Adt(a) => a.statics.contains_key(&members[0].name),
            _ => false,
        };
        value.then_some((root, members))
    }

    /// The type of `root.members…` ([`Self::value_path`]), read without checking it.
    fn value_path_ty(&mut self, root: &ast::Ident, members: &[ast::Ident]) -> Option<TyId> {
        let (mut t, rest) = match self.peek_local_ty(&root.name) {
            Some(t) => (t, members),
            None => match self.constant_ty(root) {
                Some(t) => (t, members),
                None => {
                    let Item::Def(d) =
                        self.cx.lookup_item_at(self.module, &root.name, root.span)?
                    else {
                        return None;
                    };
                    let g = *self.cx.adt(d)?.statics.get(&members[0].name)?;
                    crate::body::driver::ensure_global(self.cx, g);
                    (self.cx.global(g)?.ty, &members[1..])
                }
            },
        };
        for m in rest {
            t = self.field_of(t, &m.name)?.1;
        }
        Some(t)
    }

    /// `new C(args)` through a class value `C` (module docs).
    pub(super) fn new_through_value(
        &mut self,
        class: &ast::TypeExpr,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let c = self.ctor_value_named(class)?;
        let v = self.expr(&c, None, Want::Borrow);
        Some(self.ctor_field_call(v, NEW, args, span))
    }

    /// `e instanceof C` through a class value `C` (module docs).
    pub(super) fn instanceof_value(
        &mut self,
        e: &ast::Expr,
        ty: &ast::TypeExpr,
        span: Span,
    ) -> Option<hir::Expr> {
        let c = self.ctor_value_named(ty)?;
        // Reading the variable `C` before `e` is evaluated changes nothing JS could observe.
        let v = self.expr(&c, None, Want::Borrow);
        Some(self.ctor_field_call(v, IS, std::slice::from_ref(e), span))
    }

    /// A call of the function in hidden field `field` of class value `v`.
    fn ctor_field_call(
        &mut self,
        v: hir::Expr,
        field: &str,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        if self.cx.ty.is_bottom(v.ty) {
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        let core = self.cx.ty.core(v.ty).unwrap_or(v.ty);
        if self.cx.ctor_sig(core).is_none() {
            let tn = self.cx.display(v.ty);
            let what = if field == NEW {
                "`new`"
            } else {
                "`instanceof`"
            };
            self.cx.err(
                format!("{what} needs a class, but this value's type `{tn}` is not a construct signature (`new (...) => T`)"),
                v.span,
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        match self.field_access(v, &hidden(field, span), Want::Borrow, span) {
            Some(f) => self.call_value(f, args, span),
            None => {
                self.check_args_loose(args);
                self.error_expr(span)
            }
        }
    }

    /// `C.name` of class `d` (without a static `name`): its name, as the class object has it.
    pub(super) fn class_name_read(
        &mut self,
        d: DefId,
        prop: &ast::Ident,
        span: Span,
    ) -> Option<hir::Expr> {
        let a = self.cx.adt(d).filter(|a| a.kind == AdtKind::Class)?;
        let name = (prop.name == "name").then(|| a.name.clone())?;
        Some(self.str_lit(&name, span))
    }

    /// `C.name` of a class value `C` of type `t`: its hidden name field.
    pub(super) fn ctor_member(&mut self, t: TyId, prop: &ast::Ident) -> Option<ast::Ident> {
        (prop.name == "name" && self.cx.ctor_sig(t).is_some()).then(|| hidden(NAME, prop.span))
    }
}
