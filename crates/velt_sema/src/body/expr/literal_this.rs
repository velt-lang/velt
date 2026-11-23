//! `this` in object-literal methods, getters and setters (https://veltlang.com/docs/reference/types/ "`this` in
//! object-literal methods"). The parser gives a method whose body uses `this` a leading
//! parameter named `this` (`ast::METHOD_THIS`): `{ m(x) { return this.n + x; } }` is
//! `{ m: (this, x) => { ... } }`. Where the literal implements an interface
//! (`iface_object.rs`), or has no expected type and becomes the value of an interface made from
//! its own members (`literal_iface.rs`), the stored function takes that interface value as its
//! first parameter and the slot method passes the object it was called on. Elsewhere `this` in
//! such a method is an error: the function would otherwise read another object (an arrow's
//! `this` is the enclosing one).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, DefId, TyId, TyKind};

/// Does the object-literal method `e` (an arrow, or a generator function expression) take the
/// object as its `this` parameter?
pub(crate) fn takes_this(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Arrow { params, .. } => {
            params.first().is_some_and(|p| ast::is_method_this(&p.name))
        }
        ast::ExprKind::Function(d) => fn_takes_this(d),
        _ => false,
    }
}

/// Does object-literal property value `v` hold a method (or a getter or setter) that takes
/// the object as its `this` parameter?
pub(crate) fn prop_takes_this(v: &ast::Expr) -> bool {
    if let ast::ExprKind::Call { callee, args, .. } = &v.kind {
        if let (ast::ExprKind::Ident(f), [arrow]) = (&callee.kind, args.as_slice()) {
            if f.name == ast::OBJECT_GET_FN || f.name == ast::OBJECT_SET_FN {
                return takes_this(arrow);
            }
        }
    }
    takes_this(v)
}

/// Does generator function expression `d` (a generator method) take the object as its `this`
/// parameter?
pub(crate) fn fn_takes_this(d: &ast::FnDecl) -> bool {
    d.sig
        .params
        .first()
        .is_some_and(|p| ast::is_method_this(&p.name))
}

/// The first `this` in a method written at `root` outside the functions and methods nested in
/// it.
struct FindThis {
    root: Span,
    this: Vec<Span>,
    own: Vec<Span>,
}

impl<'a> velt_syntax::visit::Visit<'a> for FindThis {
    fn stmt(&mut self, s: &'a ast::Stmt) {
        if let ast::StmtKind::Item(_) = s.kind {
            self.own.push(s.span);
        }
    }
    fn expr(&mut self, e: &'a ast::Expr) {
        match &e.kind {
            ast::ExprKind::This => self.this.push(e.span),
            _ if e.span == self.root => {}
            ast::ExprKind::Function(_) => self.own.push(e.span),
            _ if takes_this(e) => self.own.push(e.span),
            _ => {}
        }
    }
}

impl FindThis {
    fn first(self) -> Option<Span> {
        let inside = |at: &Span| self.own.iter().any(|o| o.lo <= at.lo && at.hi <= o.hi);
        self.this.iter().copied().find(|at| !inside(at))
    }
}

/// Where method `e` (an arrow) uses `this` first.
pub(crate) fn first_this(e: &ast::Expr) -> Option<Span> {
    let mut f = FindThis {
        root: e.span,
        this: vec![],
        own: vec![],
    };
    velt_syntax::visit::walk_expr(e, &mut f);
    f.first()
}

/// Where generator method `d` (written at `span`) uses `this` first.
pub(crate) fn first_this_fn(d: &ast::FnDecl, span: Span) -> Option<Span> {
    let mut f = FindThis {
        root: span,
        this: vec![],
        own: vec![],
    };
    velt_syntax::visit::walk_fn(d, &mut f);
    f.first()
}

impl FnCx<'_, '_> {
    /// The type of the `this` parameter of the method written at `span`, whose body uses
    /// `this` first at `at`: the one its object literal gives it, else reported (`None`).
    pub(super) fn method_this_ty(&mut self, span: Span, at: Option<Span>) -> Option<TyId> {
        if let Some((_, t)) = self.method_this.filter(|(s, _)| *s == span) {
            return Some(t);
        }
        self.cx.error(
            Diagnostic::error(
                "`this` in an object-literal method is supported only where the literal implements an interface or has no expected type",
                at.unwrap_or(span),
            )
            .with_note("TypeScript allows this (`this` is the object the method is called on); here the method is stored as a plain function, which has no object to use as `this`")
            .with_note("declare the expected type as an interface (`interface T { m(): R }`), or use the object's variable instead of `this`"),
        );
        None
    }

    /// `fn_ty` (a function type) with a leading `this` parameter of type `this`.
    pub(super) fn with_this_param(&mut self, fn_ty: TyId, this: TyId) -> TyId {
        match self.cx.ty.kind(fn_ty).clone() {
            TyKind::FnPtr {
                mut params,
                ret,
                throws,
                rest,
            } => {
                params.insert(0, this);
                self.cx.ty.intern(TyKind::FnPtr {
                    params,
                    ret,
                    throws,
                    rest,
                })
            }
            _ => fn_ty,
        }
    }

    /// The value `e` written for slot `slot` of an interface literal, checked against function
    /// type `exp`, or against it after a `this` parameter of type `this` when `e` is a method
    /// using `this`. Whether it was the latter.
    pub(super) fn slot_fn(
        &mut self,
        e: &ast::Expr,
        exp: TyId,
        slot: (DefId, usize),
        this: TyId,
    ) -> (hir::Expr, bool) {
        if !takes_this(e) {
            return (self.slot_value(e, exp, slot), false);
        }
        // A method reading `this` implements the slot: reading the method as a value elsewhere
        // would detach it (`crate::iface_method_values`).
        self.cx
            .object_impls
            .method_values
            .this_slots
            .insert((slot.0, slot.1 as u32));
        let exp = self.with_this_param(exp, this);
        let saved = self.method_this.replace((e.span, this));
        let h = self.slot_value(e, exp, slot);
        self.method_this = saved;
        (h, true)
    }

    /// The value `v` of object-literal property `name` (`None`: the shorthand `{ name }`),
    /// checked to `h`. A function value not written in the literal (a method read off another
    /// object, a variable, a call's result) is recorded: Node calls it with the literal as
    /// `this` (`o.name()`), while a method read as a value keeps the object it was read from
    /// (#746). Once every body is checked, it is refused when a method of that name on any
    /// interface or class may read `this` (`crate::iface_method_values`). A function written
    /// there (an arrow, a function expression, a method) is the literal's own.
    pub(super) fn stored_value(&mut self, name: &ast::Ident, v: Option<&ast::Expr>, h: &hir::Expr) {
        // Std's literals are its own: a program's method names say nothing about them.
        let dynamic = self.may_be_any_fn(h.ty);
        // `null` / `undefined` for a function that may be absent is no function.
        let nothing = v.map(super::method_value::strip_parens).is_some_and(|e| {
            matches!(
                e.kind,
                ast::ExprKind::Lit(ast::Lit::Null | ast::Lit::Undefined)
            )
        });
        if nothing
            || v.is_some_and(fresh_fn)
            || !(dynamic || self.fn_valued(h.ty))
            || self.cx.scopes[self.module].is_std
        {
            return;
        }
        // A function declared by name (`{ go: run }`, `{ run }`) has no `this` of its own here.
        let ident = match v.map(super::method_value::strip_parens) {
            None => Some(name),
            Some(e) => match &e.kind {
                ast::ExprKind::Ident(id) => Some(id),
                _ => None,
            },
        };
        // A compiler's temporary holds a value checked where it was made (`discriminated_cast`).
        if ident.is_some_and(|id| {
            id.name.starts_with('#') || self.is_fn_item(id) || self.arrow_const(&id.name)
        }) {
            return;
        }
        let read = match v {
            Some(v) => super::method_value::path_text(v),
            None => Some(name.name.clone()),
        };
        let span = v.map_or(name.span, |v| v.span);
        // The methods read directly (`{ run: g.greet }`), checked by their own names too.
        let mut methods = vec![];
        if let Some(v) = v {
            read_methods(v, &mut methods);
        }
        // A function field read (`b.f`) is checked by name only: a method value that went
        // through a field isn't followed (#746's provenance follow-up), and its type alone
        // would refuse every function field of a `this`-reading method's type.
        // A direct read of a method (`g.greet`, also in `?:` / `||`) is checked by that method's
        // name, which says exactly what is read; the type is for a method renamed through a
        // variable or a parameter.
        let field = matches!(h.kind, hir::ExprKind::Field { .. });
        let direct = v.is_some_and(only_member_reads);
        let sig = if field || direct {
            None
        } else {
            self.fn_sig(h.ty)
        };
        let r = crate::iface_method_values::StoredRead {
            name: name.name.clone(),
            read,
            span,
            methods,
            sig,
            dynamic,
        };
        self.cx.object_impls.method_values.reads.push(r);
    }

    /// Does `name` name a `const` initialized with an arrow, a local or the module's? Its function
    /// has no `this` of its own.
    fn arrow_const(&self, name: &str) -> bool {
        let frames = std::iter::once(&self.f).chain(self.outer.iter().rev());
        for f in frames {
            if let Some(l) = crate::body::locals::frame_lookup(f, name) {
                return f.arrow_consts.contains(&l);
            }
        }
        let Some(&crate::ctx::Item::Def(d)) = self.cx.scopes[self.module].items.get(name) else {
            return false;
        };
        match self.cx.global(d) {
            Some(g) => !g.mutable && g.src.init.is_some_and(is_arrow),
            None => false,
        }
    }

    /// Is a value of type `t` an `any` or a type parameter's, which may hold a function?
    fn may_be_any_fn(&self, t: TyId) -> bool {
        let t = self.cx.ty.opt_payload(t).unwrap_or(t);
        self.cx.dynamic(t) == Some(crate::any::Dynamic::Any)
            || matches!(self.cx.ty.kind(t), TyKind::Param(_))
    }

    /// The parameter and result types of a function value of type `t` (or `t | undefined`).
    fn fn_sig(&self, t: TyId) -> Option<(Vec<TyId>, TyId)> {
        let t = self.cx.ty.opt_payload(t).unwrap_or(t);
        match self.cx.ty.kind(t) {
            TyKind::FnPtr { params, ret, .. } => Some((params.clone(), *ret)),
            &TyKind::Closure(d) => match &self.cx.info[d.0 as usize] {
                crate::defs::DefInfo::Fn(f) => {
                    Some((f.params.iter().map(|p| p.ty).collect(), f.ret))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Is a value of type `t` a function, or may it be one (`F | undefined`)?
    fn fn_valued(&self, t: TyId) -> bool {
        let t = self.cx.ty.opt_payload(t).unwrap_or(t);
        matches!(
            self.cx.ty.kind(t),
            TyKind::FnPtr { .. } | TyKind::Closure(_)
        )
    }
}

/// Why a function stored as an object-literal property is refused (`crate::iface_method_values`):
/// a method of that name may read `this` (the property's name, or the method the value reads),
/// or one of that function type does.
pub(crate) enum StoredWhy<'n> {
    Named(&'n str),
    Typed(&'n str),
}

/// The error for property `name` of an object literal, whose value (`read`, when that is a path)
/// is a function not written in the literal, where a method that may read `this` may be (`why`).
pub(crate) fn stored_method_error(
    name: &str,
    why: StoredWhy,
    read: Option<&str>,
    span: Span,
) -> Diagnostic {
    let fix = match read {
        Some(read) => format!("write the function in the literal: `{name}: (...args) => {read}(...args)`, or a method `{name}(...) {{ ... }}`"),
        None => format!("write the function in the literal: `{name}: (...args) => ...`, or a method `{name}(...) {{ ... }}`"),
    };
    let why = match why {
        StoredWhy::Named(m) => format!("a method named `{m}` may use `this`"),
        StoredWhy::Typed(m) => format!("it has the type of method `{m}`, which may use `this`"),
    };
    Diagnostic::error(
        format!("the value of property `{name}` is a function not written in the literal, and {why}, so it is not supported yet"),
        span,
    )
    .with_note(format!("Node calls it with the literal as `this` (`o.{name}()`), while Velt keeps the object a method read as a value was read from"))
    .with_note(fix)
}

/// The names of the methods `v` reads directly as its value (`g.greet`), also inside a wrapper
/// that keeps the value: parentheses, `as`, `satisfies`, `!`, either arm of `?:`, either
/// operand of `||` and `??`, and the right one of `&&`.
fn read_methods(v: &ast::Expr, out: &mut Vec<String>) {
    match &v.kind {
        ast::ExprKind::Paren(x) | ast::ExprKind::NonNull(x) => read_methods(x, out),
        ast::ExprKind::Cast { expr, .. } => read_methods(expr, out),
        ast::ExprKind::Cond { then, els, .. } => {
            read_methods(then, out);
            read_methods(els, out);
        }
        ast::ExprKind::Binary { op, lhs, rhs } => match op {
            ast::BinaryOp::Or | ast::BinaryOp::Nullish => {
                read_methods(lhs, out);
                read_methods(rhs, out);
            }
            ast::BinaryOp::And => read_methods(rhs, out),
            _ => {}
        },
        ast::ExprKind::Call { callee, args, .. }
            if args.len() == 1
                && matches!(&callee.kind, ast::ExprKind::Ident(f) if f.name == velt_common::std_hook!(satisfies)) =>
        {
            read_methods(&args[0], out)
        }
        ast::ExprKind::Member { prop, .. } => out.push(prop.name.clone()),
        _ => {}
    }
}

/// Is each value `v` may give a member read (`g.greet`), through the wrappers `read_methods`
/// looks through?
fn only_member_reads(v: &ast::Expr) -> bool {
    match &v.kind {
        ast::ExprKind::Paren(x) | ast::ExprKind::NonNull(x) => only_member_reads(x),
        ast::ExprKind::Cast { expr, .. } => only_member_reads(expr),
        ast::ExprKind::Cond { then, els, .. } => only_member_reads(then) && only_member_reads(els),
        ast::ExprKind::Binary { op, lhs, rhs } => match op {
            ast::BinaryOp::Or | ast::BinaryOp::Nullish => {
                only_member_reads(lhs) && only_member_reads(rhs)
            }
            ast::BinaryOp::And => only_member_reads(rhs),
            _ => false,
        },
        ast::ExprKind::Call { callee, args, .. }
            if args.len() == 1
                && matches!(&callee.kind, ast::ExprKind::Ident(f) if f.name == velt_common::std_hook!(satisfies)) =>
        {
            only_member_reads(&args[0])
        }
        ast::ExprKind::Member { .. } => true,
        _ => false,
    }
}

/// Is `v` a function written in place: an arrow or a function expression?
fn fresh_fn(v: &ast::Expr) -> bool {
    match &v.kind {
        ast::ExprKind::Paren(x) => fresh_fn(x),
        ast::ExprKind::Arrow { .. } | ast::ExprKind::Function(_) => true,
        _ => false,
    }
}

/// Is `e` an arrow (in parentheses or not)?
pub(crate) fn is_arrow(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Paren(x) => is_arrow(x),
        ast::ExprKind::Arrow { .. } => true,
        _ => false,
    }
}
