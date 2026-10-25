//! `this` in object-literal methods, getters and setters (docs/reference/types.md "`this` in
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
            return (self.stored_value(e, |s| s.slot_value(e, exp, slot)), false);
        }
        let exp = self.with_this_param(exp, this);
        let saved = self.method_this.replace((e.span, this));
        let h = self.slot_value(e, exp, slot);
        self.method_this = saved;
        (h, true)
    }

    /// Object-literal property value `v`, checked by `check`. A method read off another object
    /// whose body may use `this` (`{ m: c.m }`, `{ m: mk().m }`) is reported: Node calls it
    /// with the literal as `this` (`o.m()`), while the value read keeps the object it was read
    /// from. A static method is found by its class's name; another receiver's type is the one
    /// it is checked with ([`Self::stored_method_read`], from `member`), so it is checked once.
    pub(super) fn stored_value(
        &mut self,
        v: &ast::Expr,
        check: impl FnOnce(&mut Self) -> hir::Expr,
    ) -> hir::Expr {
        let read = super::method_value::strip_parens(v);
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &read.kind
        else {
            return check(self);
        };
        if self.static_namespace(object) {
            // A static method that uses `this` runs on its class.
            let uses = match self.class_of_name(object) {
                Some(d) => match self.find_static(d, &prop.name) {
                    Ok((m, ..)) => self.cx.static_this.contains_key(&m.def),
                    Err(_) => false,
                },
                None => false,
            };
            if uses {
                let text = super::method_value::path_text(read);
                let what = format!("`{}`", text.clone().unwrap_or_else(|| prop.name.clone()));
                self.report_stored_method(what, text, v.span);
                return self.error_expr(v.span);
            }
            return check(self);
        }
        let saved = self.stored_prop.replace(read.span);
        let h = check(self);
        self.stored_prop = saved;
        h
    }

    /// For `member`: is `object.prop` (the property read at `span`, `object` checked to type
    /// `t`) the value of an object-literal property (`stored_value`), and a method whose body
    /// may use `this`? Reported.
    pub(super) fn stored_method_read(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        t: TyId,
        span: Span,
    ) -> bool {
        // A literal's own method using `this` is reported where it is read
        // (`detached_literal_method`).
        if self.stored_prop != Some(span)
            || self.cx.ty.has_error(t)
            || self.own_literal_iface(t)
            || !self.method_of_reads_this(t, prop)
        {
            return false;
        }
        self.stored_prop = None;
        let text = super::method_value::path_text(object).map(|o| format!("{o}.{}", prop.name));
        let what = match &text {
            Some(text) => format!("`{text}`"),
            None => format!("`{}` of `{}`", prop.name, self.cx.display(t)),
        };
        self.report_stored_method(what, text, span);
        true
    }

    /// The method `what` (as written in the message), read as `read` when that is a path.
    fn report_stored_method(&mut self, what: String, read: Option<String>, span: Span) {
        let fix = match read {
            Some(read) => format!("call it from a method or an arrow function: `(...) => {read}(...)`, or write the method in the literal"),
            None => "call it on its object from a method or an arrow function, or write the method in the literal".into(),
        };
        self.cx.error(
            Diagnostic::error(
                format!("the method {what} may use `this`, so it cannot be the value of an object-literal property"),
                span,
            )
            .with_note("TypeScript allows this, but `this` is then the object the property is called on (`o.m()` calls it with `o`), while Velt keeps the object the method was read from")
            .with_note(fix),
        );
    }
}
