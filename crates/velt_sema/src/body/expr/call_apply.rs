//! `f.call(thisArg, a, b)` and `f.apply(thisArg, args)` on function values (#1110): the call
//! `f(a, b)` (`f(...args)` for `apply`, whose array or tuple argument spreads as in
//! `spread_args.rs`, so a `number[]` fills only a rest parameter, as TypeScript's
//! `strictBindCallApply` requires). `thisArg` is evaluated, after `f` and before the arguments,
//! as JavaScript does (a class or namespace name has no effect to evaluate).
//!
//! A function declared with a `this` parameter that its body uses runs on `thisArg`
//! (`this_param.rs`). Otherwise the `this` a call passes matters only to a function that reads
//! it. Velt's functions,
//! arrow functions and static functions of the built-in namespaces other than `Promise` read
//! none, so any `thisArg` works for them; `Promise`'s statics take `Promise`. A method read as a
//! value (`o.m`, `C.m`) keeps the object or class it was read from, so its `thisArg` must be
//! that object (`o.m.call(o, x)`), or `null` / `undefined` when the method doesn't mention
//! `this`. A variable holding a method read as a value (`const f = o.m`) takes no other `this`;
//! another function value (a parameter, an element) is called with `null` or `undefined` only,
//! as `f(x)` calls it. Anything else would call the function with another `this` than Node
//! does, which is an error saying it is not supported yet.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::method::Resolved;
use super::setters::synth;
use crate::body::{FnCx, LocalKind, Want};
use crate::collect::static_this::uses_this;
use crate::ctx::Item;
use crate::defs::DefInfo;
use crate::hir::{self, DefId, TyId, TyKind};
use crate::hook_calls::{self as hooks, ty, HookTy};

impl FnCx<'_, '_> {
    /// `f.call(...)` / `f.apply(...)` where `f` is a function (`None`: another call).
    pub(crate) fn call_apply(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Member {
            object: f,
            prop,
            optional: false,
        } = &callee.kind
        else {
            return None;
        };
        let (apply, bind) = match prop.name.as_str() {
            "call" => (false, false),
            "apply" => (true, false),
            "bind" => (false, true),
            _ => return None,
        };
        if !type_args.is_empty() || (bind && self.bind_of_method(f)) || !self.is_function_target(f)
        {
            return None;
        }
        let what = format!("`{}`", prop.name);
        let (this_arg, rest) = match args.split_first() {
            Some((t, rest)) => (Some(t), rest),
            None => (None, args),
        };
        if bind && (!rest.is_empty() || self.takes_this_fn(f)) {
            self.cx.error(
                Diagnostic::error("`bind` with arguments, or of a function with a `this` parameter, is not supported yet", span)
                    .with_note("use an arrow function: `(...xs) => f(a, ...xs)`"),
            );
            self.check_args_loose(&typed_args(args));
            return Some(self.error_expr(span));
        }
        let call_args: Vec<ast::Expr> = match (apply, rest) {
            (false, _) => rest.to_vec(),
            (true, []) => vec![],
            (true, [list]) => vec![synth(
                ast::ExprKind::Spread(Box::new(list.clone())),
                list.span,
            )],
            (true, _) => {
                self.cx.err(
                    format!(
                        "`apply` expects 1 or 2 argument(s), but got {} (TypeScript's TS2554)",
                        args.len()
                    ),
                    span,
                );
                self.check_args_loose(&typed_args(args));
                return Some(self.error_expr(span));
            }
        };
        if let ast::ExprKind::Ident(id) = &strip(f).kind {
            if self.is_fn_item(id) {
                if let Some(Item::Def(d)) = self.lookup_item(&id.name, id.span) {
                    if self.takes_this(d) {
                        let h =
                            self.this_fn_call(d, &id.name, this_arg, &[], &call_args, exp, span);
                        return Some(h);
                    }
                }
            }
        }
        if !bind {
            if let Some(h) = this_arg.and_then(|t| self.read_method_call(f, t, &call_args, exp)) {
                return Some(h);
            }
        }
        if let Some(t) = this_arg {
            if let Some(why) = self.this_not_ignored(f, t) {
                self.cx.error(
                    Diagnostic::error(
                        format!("{what} with this `this` is not supported yet: {why}"),
                        t.span,
                    )
                    .with_note("call the method on the object or class it should run on (`o.m(x)`); a function that doesn't use `this` takes `null`"),
                );
                self.check_args_loose(&typed_args(args));
                return Some(self.error_expr(span));
            }
        }
        let mut lets = vec![];
        let evaluated = this_arg.filter(|t| !is_nothing(t) && !self.static_namespace(t));
        if let Some(t) = evaluated {
            if !is_pure_path(f) {
                self.cx.err(
                    format!("{what} of a function that is computed, with a `this` that is computed too, is not supported yet: store the function in a variable first"),
                    t.span,
                );
                self.check_args_loose(args);
                return Some(self.error_expr(span));
            }
            // Evaluated for its effects, as JavaScript evaluates it; the function ignores it.
            let h = self.expr(t, None, Want::Borrow);
            lets.push(hir::Stmt {
                span: t.span,
                kind: hir::StmtKind::Expr(h),
            });
        }
        if bind {
            // A new function forwarding to `f`, which ignores `this` or keeps the object it was
            // read from, which is `thisArg` (`fn_bind.rs`).
            return Some(self.bound_fn(f, lets, exp, span));
        }
        let call = synth(
            ast::ExprKind::Call {
                callee: f.clone(),
                type_args: vec![],
                args: call_args,
                optional: false,
            },
            span,
        );
        let h = self.expr(&call, exp, Want::Move);
        Some(self.with_lets(lets, h))
    }

    /// Is `f` a function, so that `f.call` / `f.apply` are `Function.prototype`'s: a function
    /// or arrow literal, a function's name, a static function of a class or namespace, or a
    /// value of a function type?
    fn is_function_target(&mut self, f: &ast::Expr) -> bool {
        let written = f;
        let f = strip(f);
        match &f.kind {
            ast::ExprKind::Arrow { .. } | ast::ExprKind::Function(_) => true,
            ast::ExprKind::Ident(id) if self.is_fn_item(id) => true,
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } if self.static_namespace(object) => match self.class_of_name(object) {
                // A static method; a static field goes by its type.
                Some(d) => self.find_static(d, &prop.name).is_ok() || self.is_fn_value(f),
                None => true,
            },
            // `(f as () => T).call(o)`: the cast gives the function type.
            _ => self.is_fn_value(f) || (!std::ptr::eq(written, f) && self.is_fn_value(written)),
        }
    }

    /// Is `f` a method read as a value (`o.m`), whose `bind` `method_value.rs` checks?
    fn bind_of_method(&mut self, f: &ast::Expr) -> bool {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &strip(f).kind
        else {
            return false;
        };
        let Some(t) = self.peek_ty(object) else {
            return false;
        };
        self.is_method_value(t, &prop.name)
    }

    /// Does `f` name a function declared with a `this` parameter its body uses?
    fn takes_this_fn(&mut self, f: &ast::Expr) -> bool {
        let ast::ExprKind::Ident(id) = &strip(f).kind else {
            return false;
        };
        if !self.is_fn_item(id) {
            return false;
        }
        match self.lookup_item(&id.name, id.span) {
            Some(Item::Def(d)) => self.takes_this(d),
            _ => false,
        }
    }

    /// Is `f`'s type a function type?
    fn is_fn_value(&mut self, f: &ast::Expr) -> bool {
        let t = self.trial_expr(f).ty;
        matches!(
            self.cx.ty.kind(t),
            TyKind::Closure(_) | TyKind::FnPtr { .. }
        )
    }

    /// Does `id` name a function declaration (not a variable holding a function)?
    pub(super) fn is_fn_item(&mut self, id: &ast::Ident) -> bool {
        !self.is_local_name(&id.name)
            && matches!(self.lookup_item(&id.name, id.span), Some(Item::Def(d)) if matches!(self.cx.info[d.0 as usize], DefInfo::Fn(_)))
    }

    /// Is `e` the name of a class or of a built-in namespace (`String`, `Math`), whose members
    /// are static?
    pub(super) fn static_namespace(&mut self, e: &ast::Expr) -> bool {
        let ast::ExprKind::Ident(id) = &e.kind else {
            return false;
        };
        if self.is_local_name(&id.name) {
            return false;
        }
        match self.lookup_item(&id.name, id.span) {
            Some(Item::Def(d)) => self.cx.adt(d).is_some(),
            Some(_) => false,
            None => matches!(
                id.name.as_str(),
                "String" | "Number" | "Math" | "JSON" | "Array" | "Promise" | "Date" | "console"
            ),
        }
    }

    /// Why calling `f` with `this_arg` as `this` would not be the same as calling it without
    /// (`None`: it is).
    fn this_not_ignored(&mut self, f: &ast::Expr, this_arg: &ast::Expr) -> Option<&'static str> {
        let nothing = is_nothing(this_arg);
        let f = strip(f);
        match &f.kind {
            ast::ExprKind::Arrow { .. } | ast::ExprKind::Function(_) => None,
            ast::ExprKind::Ident(id) if self.is_fn_item(id) => None,
            ast::ExprKind::Member { object, prop, .. } if self.static_namespace(object) => {
                if same_path(object, this_arg) {
                    return None;
                }
                let Some(d) = self.class_of_name(object) else {
                    // `Promise.resolve` makes its value with its `this`, a constructor.
                    let promise = matches!(&object.kind, ast::ExprKind::Ident(n) if n.name == "Promise");
                    return promise.then_some("`Promise`'s static methods make their value with `this`, which must be `Promise`");
                };
                match self.find_static(d, &prop.name) {
                    // A static method's `this` is its class.
                    Ok((m, ..)) if self.cx.static_this.contains_key(&m.def) => {
                        Some("a static method that uses `this` runs on its class")
                    }
                    Ok(_) => None,
                    // A static field holding a function.
                    Err(_) if nothing => None,
                    Err(_) => Some("a function value may be a method that keeps the object it was read from"),
                }
            }
            ast::ExprKind::Member { object, prop, .. } => {
                match same_path(object, this_arg) || nothing && !self.method_reads_this(object, prop) {
                    true => None,
                    false => Some("a method read as a value keeps the object it was read from"),
                }
            }
            // `const f = (o as T).m; f.call(o)`: `f` keeps the object it was read from.
            ast::ExprKind::Ident(id) if self.read_from(&id.name, this_arg) => None,
            ast::ExprKind::Ident(id) if self.holds_method_value(&id.name) => {
                Some("this variable holds a method read as a value, which keeps the object it was read from")
            }
            _ if nothing => None,
            _ => Some("a function value may be a method that keeps the object it was read from"),
        }
    }

    /// The class `e` names, if it names one of the program (not a built-in namespace).
    pub(super) fn class_of_name(&mut self, e: &ast::Expr) -> Option<DefId> {
        let ast::ExprKind::Ident(id) = &e.kind else {
            return None;
        };
        match self.lookup_item(&id.name, id.span) {
            Some(Item::Def(d)) if self.cx.adt(d).is_some() => Some(d),
            _ => None,
        }
    }

    /// May the method `prop` of `object` read `this` (a field holding a function: no)?
    fn method_reads_this(&mut self, object: &ast::Expr, prop: &ast::Ident) -> bool {
        let t = self.trial_expr(object).ty;
        if self.cx.ty.is_bottom(t) {
            return true;
        }
        self.method_of_reads_this(t, prop)
    }

    /// May the method `prop` of a value of type `t` read `this` (a field holding a function:
    /// no)?
    pub(super) fn method_of_reads_this(&mut self, t: TyId, prop: &ast::Ident) -> bool {
        if !self.is_method_value(t, &prop.name) {
            return false;
        }
        match self.resolve_method(t, &prop.name) {
            Some(Resolved::Def { def, .. }) => uses_this(self.cx, def),
            // An override may read it.
            _ => true,
        }
    }

    /// Is the visible local `name` a `const` read from the object `this_arg` names (`const f =
    /// (o as T).m` and `this_arg` `o`)?
    fn read_from(&mut self, name: &str, this_arg: &ast::Expr) -> bool {
        if !self.is_local_name(name) {
            return false;
        }
        if self.read_from_outer(name, this_arg) {
            return true;
        }
        let Some(l) = self.lookup_local(name, Span::default()) else {
            return false;
        };
        let Some(&root) = self.f.member_reads.get(&l) else {
            return false;
        };
        // `this_arg` names that variable, and nothing assigns it: it still holds the object.
        let ast::ExprKind::Ident(t) = &strip(this_arg).kind else {
            return false;
        };
        if !self.is_local_name(&t.name) || self.lookup_local(&t.name, t.span) != Some(root) {
            return false;
        }
        let name = self.f.locals[root.0 as usize].name.clone();
        match self.local_kind(root) {
            LocalKind::Const | LocalKind::Using => true,
            // A variable of this function's own (a capture's assignments are its declarer's).
            LocalKind::Let | LocalKind::Param => {
                !self.f.captures.iter().any(|c| c.inner == root)
                    && !self.f.assign_ends.contains_key(&name)
                    && !self.f.closure_assigned.contains_key(&name)
            }
            _ => false,
        }
    }

    /// `f.call(o, …)` where `f`, a dynamic value, was read as `o.m` (`const d = (o as T)?.m`). A
    /// function value is called as it was read, as in JavaScript, whatever `o.m` holds by now. A
    /// class's method read through `any` is no function value yet (#823): that one, which can't
    /// be replaced through `any`, is the call `(o as any).m(…)`, which reads the member again, and
    /// a proxy `o` stops instead (`proxyCallCheck`).
    fn read_method_call(
        &mut self,
        f: &ast::Expr,
        this_arg: &ast::Expr,
        args: &[ast::Expr],
        exp: Option<hir::TyId>,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Ident(id) = &strip(f).kind else {
            return None;
        };
        if !self.read_from(&id.name, this_arg) || !self.uses_proxy() {
            return None;
        }
        let (ty, prop) = self.member_read_of(&id.name)?;
        self.cx.dynamic(ty)?;
        let span = f.span;
        let held = self.expr(strip(f), None, Want::Borrow);
        self.cx.dynamic(held.ty)?;
        let diags = self.cx.diags.len();
        let call = synth(
            ast::ExprKind::Call {
                callee: Box::new(f.clone()),
                type_args: vec![],
                args: args.to_vec(),
                optional: false,
            },
            span,
        );
        let value_call = self.expr(&call, exp, Want::Move);
        let result = value_call.ty;
        if self.cx.diags.len() > diags || self.cx.ty.is_bottom(result) {
            return Some(value_call);
        }
        let any = ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: vec![ast::Ident {
                    name: "any".into(),
                    span,
                }],
                args: vec![],
            },
            span,
        };
        let object = synth(
            ast::ExprKind::Cast {
                expr: Box::new(this_arg.clone()),
                ty: any,
            },
            span,
        );
        let check = hooks::proxy_call_check(ty::Any::of(object.clone())).check(self, span);
        let member = synth(
            ast::ExprKind::Member {
                object: Box::new(object),
                prop: ast::Ident { name: prop, span },
                optional: false,
            },
            span,
        );
        let call = synth(
            ast::ExprKind::Call {
                callee: Box::new(member),
                type_args: vec![],
                args: args.to_vec(),
                optional: false,
            },
            span,
        );
        let method_call = self.expr(&call, None, Want::Move);
        let unit = self.cx.ty.unit;
        let mut stmts = vec![hir::Stmt {
            kind: hir::StmtKind::Expr(check),
            span,
        }];
        let value = if result == unit {
            stmts.push(hir::Stmt {
                kind: hir::StmtKind::Expr(method_call),
                span,
            });
            self.unit_expr(span)
        } else {
            match self.assert_value(method_call, result, span) {
                Ok(h) => h,
                Err(h) => self.cast_error(h, result, span),
            }
        };
        let method_call = self.with_lets(stmts, value);
        let cond = self.intrinsic(
            hir::Intrinsic::AnyUnboundMethod,
            vec![held],
            self.cx.ty.bool_,
            span,
        );
        let kind = hir::ExprKind::If {
            cond: Box::new(cond),
            then: Box::new(method_call),
            els: Box::new(value_call),
        };
        Some(self.mk(kind, result, span))
    }

    /// The declared type and the member read of local `name`, here or in a function around this
    /// closure (`FnFrame::member_props`).
    fn member_read_of(&self, name: &str) -> Option<(hir::TyId, String)> {
        let frames = std::iter::once(&self.f).chain(self.outer.iter().rev());
        frames.into_iter().find_map(|o| {
            let l = crate::body::locals::frame_lookup(o, name)?;
            let prop = o.member_props.get(&l)?.clone();
            Some((o.locals[l.0 as usize].ty, prop))
        })
    }

    /// [`Self::read_from`] in a closure, of variables of a function around it (`const f = o.m;
    /// onDone(() => f.call(o))`) that nothing assigns.
    fn read_from_outer(&self, name: &str, this_arg: &ast::Expr) -> bool {
        let ast::ExprKind::Ident(t) = &strip(this_arg).kind else {
            return false;
        };
        if crate::body::locals::frame_lookup(&self.f, name).is_some() {
            return false;
        }
        (self.outer.iter().rev())
            .find_map(|o| crate::body::locals::frame_lookup(o, name).map(|l| (o, l)))
            .is_some_and(|(o, l)| {
                let root = o.member_reads.get(&l).copied();
                root.is_some()
                    && root == crate::body::locals::frame_lookup(o, &t.name)
                    && !o.assign_ends.contains_key(&t.name)
                    && !o.closure_assigned.contains_key(&t.name)
            })
    }

    /// `const f = o.m`: `f` was read from the variable `o`.
    pub(in crate::body) fn note_member_read(
        &mut self,
        local: hir::LocalId,
        root: hir::LocalId,
        init: Option<&ast::Expr>,
    ) {
        self.f.member_reads.insert(local, root);
        if let Some(ast::ExprKind::Member { prop, .. }) = init.map(|e| &strip(e).kind) {
            self.f.member_props.insert(local, prop.name.clone());
        }
    }

    fn holds_method_value(&mut self, name: &str) -> bool {
        self.is_local_name(name)
            && self
                .lookup_local(name, Span::default())
                .is_some_and(|l| self.f.method_values.contains(&l))
    }
}

/// Is `e` a property read (`o.m`, `C.m`), which may be a method read as a value?
pub(crate) fn is_member_read(e: &ast::Expr) -> bool {
    matches!(
        strip(e).kind,
        ast::ExprKind::Member {
            optional: false,
            ..
        }
    )
}

/// The variable the property read `e` reads from (`o` of `o.m`, `(o as T).m`), if `e` is one.
pub(crate) fn read_root(e: &ast::Expr) -> Option<&ast::Ident> {
    match &strip(e).kind {
        ast::ExprKind::Member { object, .. } => match &strip(object).kind {
            ast::ExprKind::Ident(id) => Some(id),
            _ => None,
        },
        _ => None,
    }
}

/// `e` without parentheses, `as` casts and `!`.
fn strip(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(inner)
        | ast::ExprKind::Cast { expr: inner, .. }
        | ast::ExprKind::NonNull(inner) => strip(inner),
        _ => e,
    }
}

/// `args` without `null` and `undefined`, which have no type of their own to check them against.
fn typed_args(args: &[ast::Expr]) -> Vec<ast::Expr> {
    args.iter().filter(|a| !is_nothing(a)).cloned().collect()
}

/// `null` or `undefined`.
fn is_nothing(e: &ast::Expr) -> bool {
    matches!(
        &strip(e).kind,
        ast::ExprKind::Lit(ast::Lit::Null | ast::Lit::Undefined)
    )
}

/// Is `e` a name, `this`, a function literal or a property of a name (no effects to order)?
fn is_pure_path(e: &ast::Expr) -> bool {
    match &strip(e).kind {
        ast::ExprKind::Ident(_)
        | ast::ExprKind::This
        | ast::ExprKind::Arrow { .. }
        | ast::ExprKind::Function(_) => true,
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_pure_path(object),
        _ => false,
    }
}

/// Do `a` and `b` name the same object (`o`, `this`, `o.p.q`)?
fn same_path(a: &ast::Expr, b: &ast::Expr) -> bool {
    match (&strip(a).kind, &strip(b).kind) {
        (ast::ExprKind::Ident(x), ast::ExprKind::Ident(y)) => x.name == y.name,
        (ast::ExprKind::This, ast::ExprKind::This) => true,
        (
            ast::ExprKind::Member {
                object: oa,
                prop: pa,
                optional: false,
            },
            ast::ExprKind::Member {
                object: ob,
                prop: pb,
                optional: false,
            },
        ) => pa.name == pb.name && same_path(oa, ob),
        _ => false,
    }
}
