//! Building method calls on a checked receiver: direct (`Callee::Def`), virtual, interface
//! (`Dyn` / `ParamMethod`) and builtin; the receiver is converted to the declaring type and
//! used per the method's `this` mode.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::iface_call::IfaceSlot;
use super::method::Resolved;
use super::setters::synth;
use crate::body::places::set_place_mode;
use crate::body::{FnCx, Want};
use crate::hir::{self, Callee, DefId, ExprKind as H, PassMode, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// Method call on a checked receiver.
    pub(crate) fn method_call_on(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let recv = match self.dynamic_method_call(recv, prop, args, span) {
            Ok(h) => return h,
            Err(recv) => recv,
        };
        let recv = match self.brand_tag_call(recv, prop, args, span) {
            Ok(h) => return h,
            Err(recv) => recv,
        };
        // A branded value has its primitive's (or its class's) methods.
        let recv = self.unbrand(recv);
        if prop.name == "revive"
            && matches!(self.cx.ty.kind(recv.ty), TyKind::Adt(d, _) if self.cx.is_json_value(*d))
            && args
                .first()
                .and_then(|a| self.json_callback_typed(a, "`JsonValue.revive`"))
                == Some(false)
        {
            return self.error_expr(span);
        }
        // A `shared<T>` has `T`'s methods (#36): they read the value inside, and one that
        // modifies its receiver is an error (`frozen_write`).
        let recv = match matches!(self.cx.ty.kind(recv.ty), TyKind::Shared(_))
            && !self.method_exists(recv.ty, &prop.name)
        {
            true => self.frozen_view(recv, Some(&prop.name)),
            false => recv,
        };
        if self.cx.frozen_reads && self.frozen_buffer_view(&recv, &prop.name, span) {
            for a in args {
                self.expr(a, None, Want::Borrow);
            }
            return self.error_expr(span);
        }
        // `x.toFixed(2)`: a number's method (`literal_locals`).
        self.literal_use_number(&recv);
        // `s.replace(/a/g, "b")`: std/regex's method for a regex argument (`regex_args`).
        if recv.ty == self.cx.ty.str_ {
            if let Some((hook, args)) = self.regex_string_call(prop, args) {
                return self.with_temp("<string>", recv, span, span, |lw, s| {
                    let call = ast::ExprKind::Call {
                        callee: Box::new(synth(
                            ast::ExprKind::Ident(ast::Ident {
                                name: hook.into(),
                                span: prop.span,
                            }),
                            prop.span,
                        )),
                        type_args: type_args.to_vec(),
                        args: std::iter::once(s.clone()).chain(args).collect(),
                        optional: false,
                    };
                    lw.expr(&synth(call, span), exp, Want::Move)
                });
            }
        }
        self.method_call_at(recv, prop, type_args, args, exp, span, false)
    }

    /// Method call on a checked receiver; `getter` for a property read of a `get` accessor.
    #[allow(clippy::too_many_arguments)] // `method_call_on` plus how the member is used
    pub(crate) fn method_call_at(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
        getter: bool,
    ) -> hir::Expr {
        if self.cx.ty.is_bottom(recv.ty) {
            self.check_args_loose(args);
            // A receiver narrowed to `never` panics before the call: the call is unreachable.
            let never = recv.ty == self.cx.ty.never;
            return if never { recv } else { self.error_expr(span) };
        }
        if self.shadowed_private_name(recv.ty, prop) {
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        // Methods of a literal type are its base type's (`kind.toUpperCase()`).
        let recv = self.widen_literal_receiver(recv, &prop.name);
        let recv = self.without_trap_vars(recv);
        // `xs.join()` / `xs.toString()` write each element as JS's `String(x)` (js_list.rs; a
        // list of `any`, `unknown` or `object` values: any_text.rs).
        let list_text = match prop.name.as_str() {
            "join" => args.len() <= 1,
            "toString" => args.is_empty(),
            _ => false,
        };
        if list_text && !getter && self.joins_dynamic(recv.ty) {
            return self.dynamic_list_join(recv, args, span);
        }
        // A list of literals with accessors: through `any`, each element `[object Object]`.
        if list_text
            && !getter
            && matches!(self.cx.ty.kind(recv.ty), TyKind::Array(_))
            && self.holds_literal_iface(recv.ty, 0)
        {
            let recv = self.share_if_place(recv);
            let recv = self.literal_iface_as_any(recv);
            if self.cx.dynamic(recv.ty).is_some() {
                if let Ok(h) = self.dynamic_method_call(recv, prop, args, span) {
                    return h;
                }
                return self.error_expr(span);
            }
            return self.method_call_at(recv, prop, type_args, args, exp, span, getter);
        }
        let writes_list = matches!(prop.name.as_str(), "join" | "toString")
            && matches!(self.cx.ty.kind(recv.ty), TyKind::Array(_));
        if writes_list && self.reject_js_list(recv.ty, &format!("`{}`", prop.name), prop.span) {
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        if prop.name == crate::collect::DISPOSE && !getter {
            return self.explicit_dispose(recv, args, span);
        }
        if !getter
            && matches!(self.cx.ty.kind(recv.ty), TyKind::Promise(..))
            && super::promise_then::is_promise_callback_method(&prop.name)
        {
            return self.promise_callback_call(recv, prop, args, span);
        }
        let recv = match self.cx.has_overloads && !getter {
            true => match self.overloaded_method_call(recv, prop, type_args, args, exp, span) {
                Ok(h) => return h,
                Err(recv) => recv,
            },
            false => recv,
        };
        self.readonly_method_call(&recv, &prop.name, args, prop.span);
        let Some(r) = self.resolve_method(recv.ty, &prop.name) else {
            // A method of an interface every member of a union extends.
            let recv = match self.cx.union_def(recv.ty).is_some() {
                true => match self.union_iface_receiver(recv, &prop.name) {
                    Ok(recv) => {
                        return self.method_call_at(recv, prop, type_args, args, exp, span, getter)
                    }
                    Err(recv) => match self
                        .union_member_call(recv, prop, type_args, args, exp, span, getter)
                    {
                        Ok(h) => return h,
                        Err(recv) => recv,
                    },
                },
                false => recv,
            };
            // `e.toString()` on an error union: `Error`'s method, dispatched to each class's.
            if self.is_error_union(recv.ty) {
                let recv = self.error_union_accessor(recv, &prop.name);
                if !self.is_error_union(recv.ty) {
                    return self.method_call_at(recv, prop, type_args, args, exp, span, getter);
                }
                return self.no_method(recv, prop, args, span);
            }
            if prop.name == "toString" && args.is_empty() && !getter {
                return match self.union_to_string(recv, span) {
                    Ok(h) => h,
                    Err(recv) => self.no_method(recv, prop, args, span),
                };
            }
            return self.no_method(recv, prop, args, span);
        };
        // `g.throw(e)` with `e: never` (from the `catch` of a `try` that cannot throw) cannot
        // run: the call is its argument, so it adds no `any` to what the caller throws.
        if prop.name == "throw" && args.len() == 1 && self.cx.generator_result(recv.ty).is_some() {
            let mark = crate::body::recheck::Mark::here(self.cx);
            let frames = self.trial_frames();
            let never = self.expr(&args[0], None, Want::Move).ty == self.cx.ty.never;
            mark.rollback(self.cx);
            self.restore_trial_frames(frames);
            if never {
                let arg = self.expr(&args[0], None, Want::Move);
                let block = hir::Block {
                    stmts: vec![hir::Stmt {
                        kind: hir::StmtKind::Expr(recv),
                        span,
                    }],
                    value: Some(Box::new(arg)),
                    span,
                };
                return self.mk(H::Block(block), self.cx.ty.never, span);
            }
        }
        if prop.name == "next" && !args.is_empty() && self.cx.generator_result(recv.ty).is_some() {
            // `next(value)` on a `Generator<T, E, N>`: the prelude's `nextWith(value)`.
            let n = self.cx.generator_next_ty(recv.ty);
            if args.len() == 1 && !self.cx.ty.is_bottom(n) {
                if let Some(Resolved::Def {
                    def,
                    slots,
                    recv_ty,
                    ..
                }) = self.resolve_method(recv.ty, "nextWith")
                {
                    return self
                        .def_method_call(recv, def, slots, recv_ty, type_args, args, exp, span);
                }
            }
            self.cx.error(
                Diagnostic::error("`next()` takes no argument", args[0].span).with_note(
                    "`next(value)` makes the paused `yield` evaluate to `value`, which needs the generator's type to say what it takes: declare it `Generator<T, void, N>` (`N`: the type of the values `next` passes), or write `next()`",
                ),
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        if let Resolved::Def { def, .. } = &r {
            self.no_generator_send(recv.ty, *def, span);
        }
        self.check_extension_ambiguity(&r, &prop.name, prop.span);
        self.check_private(self.method_private_to(&r), &prop.name, prop.span);
        self.rec_method(prop.span, &r);
        if self.is_getter(&r) && !getter {
            self.cx.error(
                Diagnostic::error(
                    format!("`{}` is a getter, not a method", prop.name),
                    prop.span,
                )
                .with_note(format!("read it as a property: `.{}`", prop.name)),
            );
        }
        match r {
            Resolved::Def {
                def,
                slots,
                recv_ty,
                is_static,
            } => {
                if is_static {
                    self.cx.err(
                        format!("`{}` is a static method; call it on the type", prop.name),
                        prop.span,
                    );
                }
                self.check_generic_override(recv.ty, prop);
                self.def_method_call(recv, def, slots, recv_ty, type_args, args, exp, span)
            }
            Resolved::Virtual { def, slot, slots } => {
                let c = self.fn_callable(def, format!("method `{}`", prop.name), span);
                // Left-out arguments take the default of the override that runs.
                let Ok((fill, args)) = self.virtual_fill(&c, recv.ty, (def, slot), args, span)
                else {
                    return self.error_expr(span);
                };
                let args = &args[..];
                let this_recv = self.this_recv(&recv);
                let (c, slot, throws_def) = match &fill {
                    Some(t) => (self.fill_callable(&c, t, span), t.slot, t.entry),
                    None => (c, slot, def),
                };
                let ck = self.check_call(&c, slots, args, exp, span);
                let recv = self.receiver(recv, None, self.this_mode(def), self.is_async_fn(def));
                let mut all = vec![recv];
                all.extend(ck.args);
                self.call_throws(throws_def, &ck.type_args, ck.ret, span);
                let kind = H::Call {
                    callee: Callee::Virtual { slot },
                    args: all,
                };
                let call = self.mk(kind, ck.ret, span);
                self.this_result(call, def, this_recv)
            }
            Resolved::Iface {
                on_param,
                iface,
                iface_args,
                slot,
                method,
            } => {
                let s = IfaceSlot {
                    on_param,
                    iface,
                    iface_args,
                    slot,
                    method,
                    rest: false,
                };
                self.iface_call(recv, s, type_args, args, exp, span)
            }
            Resolved::Builtin(b) => self.builtin_method_call(recv, b, prop, args, exp, span),
        }
    }

    #[allow(clippy::too_many_arguments)] // receiver + resolved target + call-site parts
    pub(super) fn def_method_call(
        &mut self,
        recv: hir::Expr,
        def: DefId,
        slots: Vec<Option<TyId>>,
        recv_ty: TyId,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let name = self
            .cx
            .fn_info(def)
            .name
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_string();
        let mut c = self.fn_callable(def, format!("method `{name}`"), span);
        // A parameter that is `void` through the receiver's type arguments may be left out, as
        // in TS (`new Box<void>().put()` with `put(x: T)`); the call's own type arguments don't
        // count for that (`h<void>()` is TS2554, as for a function).
        let unit = self.cx.ty.unit;
        for p in &mut c.params {
            if self.cx.subst_known(p.ty, &slots) == unit {
                p.ty = unit;
            }
        }
        let mut slots = slots;
        let own = slots.iter().filter(|s| s.is_none()).count();
        self.explicit_type_args(&mut slots, own, type_args, &c.defaults, span);
        self.weak_key_args = self.weak_method(def);
        // A promise set into a `Map` (or pushed) whose type is a site (`throws::promise_sites`).
        let stored = self.stored_arg_site(&recv, &name, args);
        let outer = std::mem::replace(&mut self.cx.fn_sites.push, stored);
        let ck = self.check_call(&c, slots, args, exp, span);
        self.cx.fn_sites.push = outer;
        self.note_async_args(def, &ck.args);
        let this_recv = self.this_recv(&recv);
        let recv = self.receiver(
            recv,
            Some(recv_ty),
            self.this_mode(def),
            self.is_async_fn(def),
        );
        let mut all = vec![recv];
        all.extend(ck.args);
        if let Some(h) = self.sync_call(def, &mut all, ck.ret, span) {
            return h;
        }
        self.call_throws(def, &ck.type_args, ck.ret, span);
        let kind = H::Call {
            callee: Callee::Def(def, ck.type_args),
            args: all,
        };
        let call = self.mk(kind, ck.ret, span);
        self.this_result(call, def, this_recv)
    }

    /// The receiver argument: upcast to the declaring type, then used per the `this` mode
    /// (`is_async`: of an async method).
    pub(super) fn receiver(
        &mut self,
        recv: hir::Expr,
        to: Option<TyId>,
        mode: PassMode,
        is_async: bool,
    ) -> hir::Expr {
        let mut recv = match to {
            Some(t) if t != recv.ty => self.coerce(recv, t),
            _ => recv,
        };
        let target = match &mut recv.kind {
            H::Upcast(inner) | H::Downcast(inner) => &mut **inner,
            _ => &mut recv,
        };
        match mode {
            PassMode::BorrowMut => self.use_mutably(target, "call a mutating method on"),
            PassMode::Owned => {
                self.force_move(target);
                // An async method's receiver (an object) is shared with the call when the place
                // is used again or cannot be moved from (a `using` variable), like an argument.
                let ty = target.ty;
                if crate::body::places::is_place(target) && self.cx.is_shared_value(ty) {
                    self.soft_move(target, is_async);
                }
            }
            PassMode::Copy | PassMode::Borrow => set_place_mode(target, UseMode::Borrow),
        }
        recv
    }

    fn no_method(
        &mut self,
        recv: hir::Expr,
        prop: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        if let Some((_, fty)) = self.field_of(recv.ty, &prop.name) {
            if fty == self.cx.ty.error {
                // The field's own error was reported where it was declared.
                for a in args {
                    self.expr(a, None, Want::Move);
                }
                return self.error_expr(span);
            }
            // A field holding a function value, or a `Function` / `any` (issue #926): a dynamic
            // call of the value it holds.
            if matches!(self.cx.ty.kind(fty), TyKind::FnPtr { .. })
                || self.cx.generic_sig(fty).is_some()
                || self.cx.is_callable_object(fty)
                || matches!(
                    self.cx.dynamic(fty),
                    Some(crate::any::Dynamic::Function | crate::any::Dynamic::Any)
                )
            {
                return match self.field_access(recv, prop, Want::Borrow, span) {
                    Some(f) => self.call_value(f, args, span),
                    None => self.error_expr(span),
                };
            }
        }
        // `r.f()` on a `Record<string, any>` (or of functions): a call of the entry's value,
        // as `r.f` reads it (`slots.header()` through `[key: string]: any`).
        if let Some((_, v)) = self.record_args(recv.ty) {
            let callable = matches!(self.cx.ty.kind(v), TyKind::FnPtr { .. })
                || matches!(
                    self.cx.dynamic(v),
                    Some(crate::any::Dynamic::Function | crate::any::Dynamic::Any)
                );
            if callable {
                let f = self.record_read(recv, super::record::RecordKey::Name(prop), span);
                // A missing entry of a `Record<string, any>` reads as an `any` holding
                // `undefined`. Calling it panics with a `TypeError` for now, where Node throws
                // one that `catch` can handle (a call of any `any` that holds no function).
                let f = match self.cx.dynamic(v) {
                    Some(_) if f.ty != v => self.coerce(f, v),
                    _ => f,
                };
                return self.call_value(f, args, span);
            }
        }
        // A function in an object part of `T & { … }` (`s.$set(v)`), read through its slot;
        // also a generic function value in an interface field.
        if let Some(f) = self.iface_field_of(recv.ty, &prop.name) {
            if matches!(self.cx.ty.kind(f.ty), TyKind::FnPtr { .. })
                || self.cx.generic_sig(f.ty).is_some()
            {
                return match self.field_access(recv, prop, Want::Borrow, span) {
                    Some(f) => self.call_value(f, args, span),
                    None => self.error_expr(span),
                };
            }
        }
        // `t.join(sep)` / `t.toString()` on a tuple, as on an array.
        let recv = match self.tuple_text_call(recv, prop, args, span) {
            Ok(text) => return text,
            Err(recv) => recv,
        };
        if crate::collect::exports::member_maybe_dropped(self.cx, recv.ty, &prop.name) {
            // The parser may have dropped its declaration (`Ctx::recovered`).
            for a in args {
                self.expr(a, None, Want::Move);
            }
            return self.error_expr(span);
        }
        let tn = self.cx.display(recv.ty);
        let mut d = Diagnostic::error(
            format!("no method named `{}` found for type `{tn}`", prop.name),
            prop.span,
        );
        if self.cx.ty.bigint_named && tn.contains("never") {
            d = d.with_note(crate::types::BIGINT_NEVER_NOTE);
        }
        if recv.ty == self.cx.ty.str_ && prop.name == "length" {
            d = Diagnostic::error(
                "`length` is a property, not a method; write `s.length`",
                prop.span,
            );
        } else if self.unreadable_shared(recv.ty) {
            d = d.with_note(self.unreadable_shared_note(recv.ty));
        } else if let Some(note) = self.narrowing_note(recv.ty) {
            d = d.with_note(note);
        } else if let Some(note) = self.unnarrowed_note(recv.span) {
            d = d.with_note(note);
        } else if self.is_record_read(&recv) {
            d = d.with_note(format!(
                "a `Record` read is `V | undefined`, since the key may be missing (TypeScript types it `V`): use `?.{0}(...)`, or keep the value you store: `(r[k] ??= []).{0}(...)`",
                prop.name
            ));
        } else if let Some(n) = self.cx.ty.nothing_of(recv.ty) {
            d = d.with_note(format!(
                "the value may be {}: use `?.{}(...)` or check `!= null` first",
                n.word(),
                prop.name
            ));
        } else if prop.name == "writeString" && self.is_std_stdout(recv.ty) {
            d = d.with_note(
                "`writeString` was removed: write a string with `process.stdout.write(s)` (no \
                 import); `stdout.write` of `velt/process` takes bytes",
            );
        }
        self.cx.error(d);
        self.check_args_loose(args);
        self.error_expr(span)
    }

    /// Is `h` a read of a `Record` key (`r[k]`, `r.name`: `Record.__get`)?
    fn is_record_read(&self, h: &hir::Expr) -> bool {
        matches!(&h.kind, H::Call { callee: hir::Callee::Def(d, _), .. }
            if self.cx.try_fn(*d).is_some_and(|f| f.name.ends_with("Record.__get")))
    }

    /// Is `t` the `Stdout` of `velt/process` (the byte sink `stdout`)?
    fn is_std_stdout(&self, t: TyId) -> bool {
        match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => self
                .cx
                .adt(*d)
                .is_some_and(|a| a.qual_name == velt_common::std_item!(process_stdout, "Stdout")),
            _ => false,
        }
    }

    /// Call a zero-argument method on a checked receiver (desugarings such as `entries()`).
    pub(crate) fn method_call_hir(
        &mut self,
        recv: hir::Expr,
        name: &str,
        span: Span,
    ) -> Option<hir::Expr> {
        let prop = ast::Ident {
            name: name.to_string(),
            span,
        };
        let h = self.method_call_on(recv, &prop, &[], &[], None, span);
        (!self.cx.ty.is_bottom(h.ty)).then_some(h)
    }
}

impl FnCx<'_, '_> {
    /// A call of `method` (`ch.send(v)` / `ch.trySend(v)` on std's `Channel<T>`) that copies
    /// `v` to the receiving task like a `spawn` argument: `T` cannot hold a generator.
    fn no_generator_send(&mut self, recv: TyId, method: DefId, span: Span) {
        let in_std = self
            .cx
            .try_fn(method)
            .is_some_and(|f| self.cx.scopes[f.module].is_std);
        if !in_std || !self.cx.channel_sends().contains(&method) {
            return;
        }
        if let TyKind::Adt(_, args) = self.cx.ty.kind(recv).clone() {
            if let [t] = args.as_slice() {
                self.no_generator_copy(*t, crate::body::GenCopy::Task, span);
            }
        }
    }
}

impl FnCx<'_, '_> {
    /// Is `def` a method of the prelude's `WeakMap` or `WeakSet` taking a key first (`set`,
    /// `get`, `has`, `delete`, `add`)?
    fn weak_method(&self, def: DefId) -> bool {
        let Some(owner) = self.cx.fn_info(def).owner else {
            return false;
        };
        let weak = ["WeakMap", "WeakSet"]
            .iter()
            .any(|n| self.cx.prelude_adt(n) == Some(owner));
        let name = self.cx.fn_info(def).name.rsplit('.').next().unwrap_or("");
        weak && matches!(name, "set" | "get" | "has" | "delete" | "add")
    }
}

impl FnCx<'_, '_> {
    /// `x.toString()` on a union (#1275): every member has it, and on each of them it gives
    /// what `String(x)` does (a class's own `toString`, a primitive's text), so it is written
    /// as `String(x)`. `Err` gives `recv` back for a type that is not such a union (an array
    /// member is written as `String(x)` writes an array only outside a union).
    fn union_to_string(&mut self, recv: hir::Expr, span: Span) -> Result<hir::Expr, hir::Expr> {
        let Some(members) = self.cx.union_members(recv.ty) else {
            return Err(recv);
        };
        let plain = members.iter().all(|&m| {
            !matches!(self.cx.ty.kind(m), TyKind::Array(_))
                && self.cx.ty.nothing_of(m).is_none()
                && self.cx.dynamic(m).is_none()
        });
        if !plain {
            return Err(recv);
        }
        let part = self.js_text(recv, "`x.toString()`");
        Ok(self.concat_parts(vec![part], span))
    }
}
