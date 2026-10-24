//! Object literals with no expected type whose methods use `this`, or that have getters or
//! setters (docs/reference/types.md "`this` in object-literal methods"). TypeScript types
//! `this` in such a literal as the literal's own type; Velt makes that type an interface no
//! source names, with the literal's members, which the literal then implements as one written
//! where an interface is expected does (`iface_object.rs`):
//!
//! - a value `k: v` (or `{ k }`, or a method that doesn't use `this`) is a field of `v`'s type;
//! - a method using `this` is a method, with the parameter types written on it;
//! - `get k()` is a getter method and `set k(v)` a setter method (`v` of the getter's type when
//!   not written).
//!
//! The result types of methods and getters come from their bodies: each is checked on trial
//! with `this` typed by the interface, in rounds, until those reading other members through
//! `this` know their types; a result that depends on itself must be written.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::iface_object::{written, Written};
use super::literal_this::takes_this;
use crate::body::{FnCx, Want};
use crate::defs::{DefInfo, FieldInfo, Generics, IfaceInfo, IfaceMethod, ParamSig};
use crate::hir::{self, DefId, PassMode, TyId, TyKind};

/// A member of the literal's interface, before the result types are known.
struct Member<'e> {
    name: String,
    span: Span,
    /// The method, getter or setter (`None`: a field).
    value: Option<&'e ast::Expr>,
    is_getter: bool,
    is_setter: bool,
}

impl FnCx<'_, '_> {
    /// Should an object literal with `props` and no expected type implement its own interface
    /// (module docs)?
    fn wants_own_iface(&self, props: &[ast::ObjectProp]) -> bool {
        props.iter().any(|p| match p {
            ast::ObjectProp::KeyValue(_, v) => {
                super::literal_this::prop_takes_this(v)
                    || !matches!(written(Some(v)), Written::Value(_))
            }
            _ => false,
        })
    }

    /// Is nothing known about the type expected where `exp` is (none, or one still being
    /// inferred)?
    fn no_expected_type(&self, exp: Option<TyId>) -> bool {
        match self.hint(exp) {
            None => true,
            Some(t) => t == self.cx.ty.error,
        }
    }

    /// A literal with `props` where `exp` is expected (no interface) that has getters, setters
    /// or methods using `this`: with no expected type, the value of its own interface; else
    /// accessors are reported (`this` is, where the methods are checked). `None` otherwise.
    pub(super) fn accessor_literal(
        &mut self,
        props: &[ast::ObjectProp],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        if self.no_expected_type(exp) && self.wants_own_iface(props) {
            return Some(self.own_iface_object(props, span));
        }
        let at = props.iter().find_map(super::iface_object::is_accessor)?;
        self.cx.error(
            Diagnostic::error(
                "getters and setters in object literals are not supported yet where no interface with methods is expected",
                at,
            )
            .with_note("an object literal of an object type is plain data: its fields hold values; store the value in a field, annotate the literal with an interface that declares the member, or declare a class with the accessor"),
        );
        Some(self.error_expr(span))
    }

    /// The literal with `props` (no expected type) as the value of its own interface.
    fn own_iface_object(&mut self, props: &[ast::ObjectProp], span: Span) -> hir::Expr {
        match self.own_iface(props, span) {
            Some(target) => self.iface_object(props, target, span),
            None => self.error_expr(span),
        }
    }

    /// The interface of a literal with `props` (module docs), as `iface_object`'s target.
    fn own_iface(
        &mut self,
        props: &[ast::ObjectProp],
        span: Span,
    ) -> Option<(DefId, Vec<TyId>, TyId)> {
        let mut members = vec![];
        let mut fields = vec![];
        for p in props {
            let (key, value) = match p {
                ast::ObjectProp::KeyValue(k, v) => (k.clone(), Some(v)),
                ast::ObjectProp::Shorthand(k) => (k.clone(), None),
                ast::ObjectProp::Spread(e) => {
                    self.cx.error(
                        Diagnostic::error(
                            "object spread (`...x`) in an object literal with getters, setters or methods using `this` is not supported yet",
                            e.span,
                        )
                        .with_note("write the members out, or annotate the literal with an interface"),
                    );
                    return None;
                }
                ast::ObjectProp::Method(f) => {
                    self.cx.err(
                        "this method cannot be part of an object literal with getters, setters or methods using `this`",
                        f.sig.name.span,
                    );
                    return None;
                }
            };
            let key = self.literal_key_name(key);
            let (value, is_getter, is_setter) = match written(value) {
                Written::Get(a) => (Some(a), true, false),
                Written::Set(a) => (Some(a), false, true),
                Written::Value(Some(v)) if takes_this(v) => (Some(v), false, false),
                Written::Value(v) => {
                    // A field: its value's type, checked on trial (the literal checks it again).
                    let h = match v {
                        Some(v) => self.trial_type(v),
                        None => {
                            let e =
                                super::setters::synth(ast::ExprKind::Ident(key.clone()), key.span);
                            self.trial_type(&e)
                        }
                    };
                    let Some(ty) = h else {
                        // Reported where the literal checks it.
                        self.prop_value(&key, v, None);
                        return None;
                    };
                    fields.push(field(&key, ty));
                    continue;
                }
            };
            members.push(Member {
                name: key.name.clone(),
                span: key.span,
                value,
                is_getter,
                is_setter,
            });
        }
        let n = self.bounds.len();
        let d = self.new_own_iface(fields, n, span);
        let args: Vec<TyId> = (0..n as u32).map(|i| self.cx.ty.param(i)).collect();
        let dyn_ty = self.cx.ty.intern(TyKind::Dyn(d, args.clone()));
        // Signatures with what is written, results unknown (`error`) until inferred.
        let error = self.cx.ty.error;
        let mut known = vec![];
        for m in &members {
            let e = m.value.expect("ICE: a method member has a value");
            let (params, ret) = self.written_sig(e, m)?;
            let name = match m.is_setter {
                true => format!("set {}", m.name),
                false => m.name.clone(),
            };
            known.push(ret.is_some() || m.is_setter);
            let unit = self.cx.ty.unit;
            iface_mut(self.cx, d).methods.push(IfaceMethod {
                name,
                span: m.span,
                params,
                ret: ret.unwrap_or(if m.is_setter { unit } else { error }),
                mut_this: false,
                default: None,
                is_getter: m.is_getter,
                generics: Generics::default(),
                throws: None,
            });
        }
        // Rounds of trials until no result type is learned.
        loop {
            let mut learned = false;
            for (k, m) in members.iter().enumerate() {
                if known[k] {
                    continue;
                }
                let e = m.value.expect("ICE: a method member has a value");
                if let Some(ret) = self.trial_result(e, d, k, dyn_ty) {
                    iface_mut(self.cx, d).methods[k].ret = ret;
                    known[k] = true;
                    learned = true;
                }
            }
            if !learned {
                break;
            }
        }
        if let Some(k) = known.iter().position(|k| !k) {
            // Checked for real, to report what keeps it from being known.
            let diags = self.cx.diags.len();
            let e = members[k].value.expect("ICE: a method member has a value");
            let exp = self.member_fn_type(d, k, dyn_ty, takes_this(e));
            let saved = self.method_this.replace((e.span, dyn_ty));
            self.expr(e, Some(exp), Want::Move);
            self.method_this = saved;
            if self.cx.diags.len() == diags {
                let what = if members[k].is_getter {
                    "getter"
                } else {
                    "method"
                };
                self.cx.error(
                    Diagnostic::error(
                        format!("the result type of {what} `{}` cannot be inferred: it depends on its own result through `this`", members[k].name),
                        members[k].span,
                    )
                    .with_note(format!("write it: `{}(): T`", members[k].name)),
                );
            }
            return None;
        }
        // A setter without a written type takes its getter's.
        for (k, m) in members.iter().enumerate() {
            if !m.is_setter || !self.cx.ty.has_error(iface_param(self.cx, d, k)) {
                continue;
            }
            let getter = members.iter().position(|g| g.is_getter && g.name == m.name);
            let Some(g) = getter else {
                self.cx.err(
                    format!(
                        "the parameter of setter `{}` needs a type: the literal has no getter `{}`",
                        m.name, m.name
                    ),
                    m.span,
                );
                return None;
            };
            let t = self.cx.iface(d).map_or(error, |i| i.methods[g].ret);
            let mode = self.param_mode(t);
            let p = &mut iface_mut(self.cx, d).methods[k].params[0];
            p.ty = t;
            p.mode = mode;
        }
        self.name_own_iface(d);
        Some((d, args, dyn_ty))
    }

    /// The written parameter types of method `e` (member `m`) and its written result, if any.
    #[allow(clippy::type_complexity)] // the parameters and the result
    fn written_sig(&mut self, e: &ast::Expr, m: &Member) -> Option<(Vec<ParamSig>, Option<TyId>)> {
        let mut params = vec![];
        let error = self.cx.ty.error;
        type Written<'p> = (
            &'p ast::Ident,
            Option<&'p ast::TypeExpr>,
            Option<(&'p ast::Expr, bool)>,
        );
        let (ps, ret, is_async): (Vec<Written>, _, _) = match &e.kind {
            ast::ExprKind::Arrow {
                params,
                ret,
                is_async,
                ..
            } => (
                params
                    .iter()
                    .map(|p| {
                        (
                            &p.name,
                            p.ty.as_ref(),
                            p.default.as_ref().map(|d| (d, p.optional)),
                        )
                    })
                    .collect(),
                ret.as_ref(),
                *is_async,
            ),
            ast::ExprKind::Function(f) => (
                f.sig
                    .params
                    .iter()
                    .map(|p| {
                        (
                            &p.name,
                            Some(&p.ty),
                            p.default.as_ref().map(|d| (d, p.optional)),
                        )
                    })
                    .collect(),
                f.sig.ret.as_ref(),
                false,
            ),
            _ => (vec![], None, false),
        };
        for (name, ty, default) in ps {
            if ast::is_method_this(name) {
                continue;
            }
            let t = match ty {
                Some(t) => self.resolve(t),
                // A setter's parameter may take its getter's type (after inference).
                None if m.is_setter => error,
                None => {
                    self.cx.error(
                        Diagnostic::error(
                            format!("parameter `{}` of method `{}` needs a type: the object literal has no expected type", name.name, m.name),
                            name.span,
                        )
                        .with_note("write its type, or annotate the literal with an interface that declares the method"),
                    );
                    return None;
                }
            };
            let mode = self.param_mode(t);
            // Calls through the interface pass every argument, so they take the defaults.
            let default = default.map(|d| {
                let fname = format!("{{}}.{}", m.name);
                crate::body::defaults::literal_method_default(self.cx, self.module, d, t, &fname)
            });
            params.push(ParamSig {
                name: name.name.clone(),
                span: name.span,
                ty: t,
                mode,
                default,
            });
        }
        let ret = ret.map(|t| {
            let r = self.resolve(t);
            match (is_async, self.cx.ty.promise_payload(r)) {
                (true, None) => self.closure_ret_annotation(t, true),
                _ => r,
            }
        });
        Some((params, ret))
    }

    /// How an interface method takes a parameter of type `t`.
    fn param_mode(&mut self, t: TyId) -> PassMode {
        match self.cx.is_copy(t) {
            true => PassMode::Copy,
            false => PassMode::Borrow,
        }
    }

    /// The function type member `k` of interface `d` is checked against, its result unknown
    /// unless inferred, after a `this` parameter (`dyn_ty`) when it uses `this`.
    fn member_fn_type(&mut self, d: DefId, k: usize, dyn_ty: TyId, this: bool) -> TyId {
        let m = self.cx.iface(d).map(|i| i.methods[k].clone());
        let Some(m) = m else {
            return self.cx.ty.error;
        };
        let params = this
            .then_some(dyn_ty)
            .into_iter()
            .chain(m.params.iter().map(|p| p.ty))
            .collect();
        let throws = self.cx.ty.error;
        self.cx.ty.intern(TyKind::FnPtr {
            params,
            ret: m.ret,
            throws,
            rest: false,
        })
    }

    /// The result type of member `k` (method `e`) of interface `d`, checked on trial; `None`
    /// while it is not known (it reads a member whose type is not known yet, or has errors).
    fn trial_result(&mut self, e: &ast::Expr, d: DefId, k: usize, dyn_ty: TyId) -> Option<TyId> {
        let exp = self.member_fn_type(d, k, dyn_ty, takes_this(e));
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let temps = self.call_temps.len();
        let saved = self.method_this.replace((e.span, dyn_ty));
        let h = self.expr(e, Some(exp), Want::Move);
        self.method_this = saved;
        let failed = self.cx.diags.len() > diags;
        mark.rollback(self.cx);
        self.cx.diags.truncate(diags);
        self.call_temps.truncate(temps);
        self.restore_trial_frames(frames);
        let ret = match self.cx.ty.kind(h.ty) {
            TyKind::FnPtr { ret, .. } => *ret,
            _ => return None,
        };
        (!failed && !self.cx.ty.has_error(ret)).then_some(ret)
    }

    /// A fresh interface with `fields` over the `n` generic params in scope.
    fn new_own_iface(&mut self, fields: Vec<FieldInfo>, n: usize, span: Span) -> DefId {
        let mut generics = Generics::default();
        for i in 0..n {
            generics.push(&format!("T{i}"));
        }
        let info = IfaceInfo {
            name: "{}".into(),
            qual_name: "{}".into(),
            module: self.module,
            span,
            generics,
            fields,
            methods: vec![],
            parents: vec![],
            decl: None,
        };
        let d = self.cx.alloc_def(span, DefInfo::Iface(Box::new(info)));
        self.cx.literal_ifaces.insert(d);
        d
    }

    /// Is `t` the interface of an object literal with no expected type?
    pub(super) fn own_literal_iface(&self, t: TyId) -> bool {
        matches!(self.cx.ty.kind(t), TyKind::Dyn(d, _) if self.cx.literal_ifaces.contains(d))
    }

    /// `o.m` read as a value, where `m` is a method of an object literal using `this`: reported.
    pub(super) fn detached_literal_method(&mut self, prop: &ast::Ident, span: Span) -> hir::Expr {
        self.cx.error(
            Diagnostic::error(
                format!("the method `{}` uses `this`, so it cannot be read as a value", prop.name),
                span,
            )
            .with_note("TypeScript allows this, but `this` is then the object the function is called on, if any (`undefined` for a plain call, which throws a TypeError in strict mode)")
            .with_note(format!("call the method on its object, or wrap the call in an arrow function: `(...) => o.{}(...)`", prop.name)),
        );
        self.error_expr(span)
    }

    /// Names interface `d` as TypeScript shows the literal's type: `{ x: number; m(): string }`.
    fn name_own_iface(&mut self, d: DefId) {
        let Some(i) = self.cx.iface(d) else {
            return;
        };
        let mut parts: Vec<(Span, String)> = vec![];
        for f in &i.fields {
            parts.push((f.span, format!("{}: {}", f.name, self.cx.display(f.ty))));
        }
        for m in &i.methods {
            let params: Vec<String> = m
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name, self.cx.display(p.ty)))
                .collect();
            let s = match (m.is_getter, m.name.strip_prefix("set ")) {
                (true, _) => format!("get {}(): {}", m.name, self.cx.display(m.ret)),
                (_, Some(n)) => format!("set {n}({})", params.join(", ")),
                _ => format!(
                    "{}({}): {}",
                    m.name,
                    params.join(", "),
                    self.cx.display(m.ret)
                ),
            };
            parts.push((m.span, s));
        }
        parts.sort_by_key(|(s, _)| s.lo);
        let parts: Vec<String> = parts.into_iter().map(|(_, s)| s).collect();
        let name = format!("{{ {} }}", parts.join("; "));
        let i = iface_mut(self.cx, d);
        i.name.clone_from(&name);
        i.qual_name = name;
    }
}

/// Field `key` of type `ty`, as a literal's value gives it.
fn field(key: &ast::Ident, ty: TyId) -> FieldInfo {
    FieldInfo {
        name: key.name.clone(),
        ty,
        declared: ty,
        span: key.span,
        readonly: false,
        optional: false,
        has_default: false,
        default: None,
        default_throws: vec![],
        private_to: None,
    }
}

/// The parameter type of setter `k` of interface `d`.
fn iface_param(cx: &crate::ctx::Ctx, d: DefId, k: usize) -> TyId {
    cx.iface(d)
        .and_then(|i| i.methods[k].params.first())
        .map_or(cx.ty.error, |p| p.ty)
}

fn iface_mut<'c, 'm>(cx: &'c mut crate::ctx::Ctx<'m>, d: DefId) -> &'c mut IfaceInfo<'m> {
    match &mut cx.info[d.0 as usize] {
        DefInfo::Iface(i) => i,
        _ => panic!("ICE: def {d:?} is not an interface"),
    }
}
