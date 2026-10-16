//! Object literals that implement an interface with methods (docs/reference/types.md "Object
//! literals implementing interfaces"). Where such an interface is expected, a literal gives
//! each of its members:
//!
//! - a method `m(...)` as a function value (`m(x) { … }`, `m: (x) => …`), whose parameter types
//!   come from the interface's method (contextual typing);
//! - a field, or a getter method of the interface, as a value or as a getter `get f() { … }`
//!   that runs on each read;
//! - a setter method `set f(v)` as a setter in the literal;
//! - a method with a default body or an optional field may be left out.
//!
//! The value is a struct no source names (`Ctx::hidden_struct_def`), one per literal, whose
//! fields hold the values and functions, and an implementation of the interface for it
//! (`Program::impls`): each method slot calls the function in its field (or the default), and
//! each field getter slot returns the stored value or calls the getter. The literal is then an
//! interface value (`ToDyn`), as a class instance converted to it is.
//!
//! ```text
//! const t: Topic = { get size() { return n; }, publish(x) { … } }
//!   →  <Topic literal> { size: () => n, publish: (x: string) => { … } } as Topic
//! ```

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::collect::forwarders::{synth_method, Host, Target};
use crate::defs::ParamSig;
use crate::hir::{self, DefId, ExprKind as H, ImplDef, PassMode, TyId, TyKind};

/// How a literal property is written.
enum Written<'e> {
    /// `k: v`, or `{ k }` (`None`).
    Value(Option<&'e ast::Expr>),
    /// `get k() { … }`: the getter as an arrow.
    Get(&'e ast::Expr),
    /// `set k(v) { … }`: the setter as an arrow.
    Set(&'e ast::Expr),
}

/// What fills an implementation slot.
enum Slot {
    /// The function in this field of the struct.
    Call(u32),
    /// The interface method's default body.
    Default,
    /// The value in this field of the struct (a field getter slot).
    Value(u32),
}

/// One interface method, with the interface's type arguments substituted.
struct MethodSig {
    name: String,
    params: Vec<ParamSig>,
    ret: TyId,
    throws: Option<TyId>,
    default: Option<DefId>,
    is_getter: bool,
    generic: bool,
}

/// The accessor a property is written as, if it is a getter or setter (`ast::OBJECT_GET_FN`).
fn written(value: Option<&ast::Expr>) -> Written<'_> {
    if let Some(ast::Expr {
        kind: ast::ExprKind::Call { callee, args, .. },
        ..
    }) = value
    {
        if let (ast::ExprKind::Ident(f), [arrow]) = (&callee.kind, args.as_slice()) {
            if f.name == ast::OBJECT_GET_FN {
                return Written::Get(arrow);
            }
            if f.name == ast::OBJECT_SET_FN {
                return Written::Set(arrow);
            }
        }
    }
    Written::Value(value)
}

/// Is property `p` a getter or setter?
pub(crate) fn is_accessor(p: &ast::ObjectProp) -> Option<Span> {
    match p {
        ast::ObjectProp::KeyValue(k, v) => match written(Some(v)) {
            Written::Value(_) => None,
            _ => Some(k.span),
        },
        _ => None,
    }
}

impl FnCx<'_, '_> {
    /// The interface with methods `exp` expects an object literal to implement, and its type
    /// arguments (`None` for any other expected type, `any` and `unknown` included).
    pub(super) fn literal_iface(&self, exp: Option<TyId>) -> Option<(DefId, Vec<TyId>, TyId)> {
        let t = self.hint(exp)?;
        let TyKind::Dyn(d, args) = self.cx.ty.kind(t) else {
            return None;
        };
        if self.cx.dynamic(t).is_some() {
            return None;
        }
        self.cx.iface(*d)?;
        Some((*d, args.clone(), t))
    }

    /// An object literal implementing interface `iface<iargs>` (type `dyn_ty`); see the module
    /// docs.
    pub(super) fn iface_object(
        &mut self,
        props: &[ast::ObjectProp],
        target: (DefId, Vec<TyId>, TyId),
        span: Span,
    ) -> hir::Expr {
        // What its methods throw counts for the interface's slots until its implementation
        // exists (`Ctx::provisional_slots`).
        let mark = self.cx.provisional_slots.len();
        let h = self.iface_object_members(props, target, span);
        self.cx.provisional_slots.truncate(mark);
        h
    }

    fn iface_object_members(
        &mut self,
        props: &[ast::ObjectProp],
        (iface, iargs, dyn_ty): (DefId, Vec<TyId>, TyId),
        span: Span,
    ) -> hir::Expr {
        let diags = self.cx.diags.len();
        let Some(info) = self.cx.iface(iface) else {
            return self.error_expr(span);
        };
        let iname = info.name.clone();
        let module = self.module;
        let fields: Vec<(String, TyId, bool)> = info
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty, f.optional))
            .collect();
        let raw: Vec<_> = info
            .methods
            .iter()
            .map(|m| {
                (
                    m.name.clone(),
                    m.params.clone(),
                    m.ret,
                    m.throws.and_then(|t| t.ty),
                    m.default,
                    m.is_getter,
                    !m.generics.names.is_empty(),
                )
            })
            .collect();
        let self_ty = self.cx.ty.error;
        let mut sargs = iargs.clone();
        sargs.push(self_ty);
        let mut methods = vec![];
        for (name, params, ret, throws, default, is_getter, generic) in raw {
            let params = params
                .into_iter()
                .map(|p| ParamSig {
                    ty: self.cx.subst(p.ty, &sargs),
                    ..p
                })
                .collect();
            methods.push(MethodSig {
                name,
                params,
                ret: self.cx.subst(ret, &sargs),
                throws: throws.map(|t| self.cx.subst(t, &sargs)),
                default,
                is_getter,
                generic,
            });
        }
        let fields: Vec<(String, TyId, bool)> = fields
            .into_iter()
            .map(|(n, t, o)| (n, self.cx.subst(t, &sargs), o))
            .collect();
        let n_methods = methods.len();
        let mut slots: Vec<Option<Slot>> = (0..n_methods + fields.len()).map(|_| None).collect();
        let mut values: Vec<(String, hir::Expr)> = vec![];
        let mut seen: Vec<String> = vec![];
        let mut ok = true;
        for p in props {
            let (key, value) = match p {
                ast::ObjectProp::KeyValue(k, v) => (k.clone(), Some(v)),
                ast::ObjectProp::Shorthand(k) => (k.clone(), None),
                ast::ObjectProp::Spread(e) => {
                    self.cx.err(
                        format!("object spread (`...x`) in an object literal implementing `{iname}` is not supported yet"),
                        e.span,
                    );
                    ok = false;
                    continue;
                }
                ast::ObjectProp::Method(f) => {
                    self.cx.err(
                        format!("this method cannot be part of an object literal implementing `{iname}`"),
                        f.sig.name.span,
                    );
                    ok = false;
                    continue;
                }
            };
            let key = self.literal_key_name(key);
            let w = written(value);
            let tag = match w {
                Written::Set(_) => format!("set {}", key.name),
                _ => key.name.clone(),
            };
            if seen.contains(&tag) {
                self.cx.err(
                    format!("duplicate member `{}` in object literal", key.name),
                    key.span,
                );
                ok = false;
                continue;
            }
            seen.push(tag.clone());
            let method = methods.iter().position(|m| m.name == tag);
            let field = fields.iter().position(|f| f.0 == key.name);
            let index = values.len() as u32;
            match (w, method, field) {
                (Written::Set(arrow), Some(slot), _) => {
                    let m = &methods[slot];
                    let exp = self.method_fn_type(m);
                    let h = self.slot_value(arrow, exp, (iface, slot));
                    let h = self.fit_fn(h, m);
                    values.push((format!("set {}", key.name), h));
                    slots[slot] = Some(Slot::Call(index));
                }
                (Written::Set(arrow), None, Some(fi)) => {
                    // A setter of a field: the interface value cannot be written through
                    // (`member.rs` `iface_field`), so it is checked and kept, never called.
                    let unit = self.cx.ty.unit;
                    let exp = self.fn_type(vec![fields[fi].1], unit, None);
                    let h = self.expr(arrow, Some(exp), Want::Move);
                    values.push((format!("set {}", key.name), h));
                }
                (Written::Get(arrow), Some(slot), _) if methods[slot].is_getter => {
                    let m = &methods[slot];
                    let exp = self.method_fn_type(m);
                    let h = self.slot_value(arrow, exp, (iface, slot));
                    let h = self.fit_fn(h, m);
                    values.push((key.name.clone(), h));
                    slots[slot] = Some(Slot::Call(index));
                }
                (Written::Get(arrow), None, Some(fi)) => {
                    let exp = self.fn_type(vec![], fields[fi].1, None);
                    let h = self.slot_value(arrow, exp, (iface, n_methods + fi));
                    let fty = self.fn_type(vec![], fields[fi].1, self.fn_throws(h.ty));
                    let h = self.coerce(h, fty);
                    values.push((key.name.clone(), h));
                    slots[n_methods + fi] = Some(Slot::Call(index));
                }
                (Written::Value(v), Some(slot), _) if !methods[slot].is_getter => {
                    let m = &methods[slot];
                    if m.generic {
                        self.cx.error(
                            Diagnostic::error(
                                format!("an object literal cannot implement the generic method `{}` of `{iname}`", m.name),
                                key.span,
                            )
                            .with_note("a function value is not generic; declare a class that implements the interface"),
                        );
                        ok = false;
                        continue;
                    }
                    let mut exp = self.method_fn_type(m);
                    if let Some(t) = v
                        .filter(|_| m.throws.is_none())
                        .and_then(|v| self.branches_throw(v, exp))
                    {
                        let params = m.params.iter().map(|p| p.ty).collect();
                        exp = self.fn_type(params, m.ret, Some(t));
                    }
                    let h = match v {
                        Some(v) => self.slot_value(v, exp, (iface, slot)),
                        None => self.prop_value(&key, v, Some(exp)),
                    };
                    let h = self.fit_fn(h, m);
                    values.push((key.name.clone(), h));
                    slots[slot] = Some(Slot::Call(index));
                }
                (Written::Value(v), Some(slot), _) => {
                    // A plain value for a getter (`size: 5` for `get size()`): the slot reads
                    // the stored value, as TypeScript allows.
                    let ret = methods[slot].ret;
                    let h = self.prop_value(&key, v, Some(ret));
                    let h = self.coerce(h, ret);
                    values.push((key.name.clone(), h));
                    slots[slot] = Some(Slot::Value(index));
                }
                (Written::Value(v), None, Some(fi)) => {
                    let fty = fields[fi].1;
                    let h = self.prop_value(&key, v, Some(fty));
                    let h = self.coerce(h, fty);
                    values.push((key.name.clone(), h));
                    slots[n_methods + fi] = Some(Slot::Value(index));
                }
                (w, _, _) => {
                    let what = match w {
                        Written::Get(_) => "a getter",
                        Written::Set(_) => "a setter",
                        Written::Value(_) => "a member",
                    };
                    self.cx.err(
                        format!("`{iname}` has no {} `{}`: an object literal implementing it may only give its members", what.trim_start_matches("a "), key.name),
                        key.span,
                    );
                    ok = false;
                }
            }
        }
        // Members left out: defaults and optional fields, else errors.
        for (slot, m) in methods.iter().enumerate() {
            if slots[slot].is_some() {
                continue;
            }
            if m.default.is_some() {
                slots[slot] = Some(Slot::Default);
            } else if ok {
                let (what, name) = match m.name.strip_prefix("set ") {
                    Some(name) => ("setter", name),
                    None if m.is_getter => ("getter", m.name.as_str()),
                    None => ("method", m.name.as_str()),
                };
                self.cx.err(
                    format!("missing {what} `{name}` of `{iname}` in object literal"),
                    span,
                );
                ok = false;
            }
        }
        for (fi, (name, ty, optional)) in fields.iter().enumerate() {
            if slots[n_methods + fi].is_some() {
                continue;
            }
            if *optional {
                let index = values.len() as u32;
                let null = self.mk(H::Lit(hir::Lit::Null), *ty, span);
                values.push((name.clone(), null));
                slots[n_methods + fi] = Some(Slot::Value(index));
            } else if ok {
                self.cx.err(
                    format!("missing field `{name}` of `{iname}` in object literal"),
                    span,
                );
                ok = false;
            }
        }
        if !ok {
            return self.error_expr(span);
        }
        if let Some((name, h)) = values.iter().find(|(_, h)| self.cx.ty.has_error(h.ty)) {
            if self.cx.diags.len() == diags {
                // A member whose type is not known, though nothing reported why.
                let at = h.span;
                self.cx.err(
                    format!("the type of member `{name}` of this object literal implementing `{iname}` is not known; write it as a function, or give it a type"),
                    at,
                );
            }
            return self.error_expr(span);
        }
        self.build_iface_object(
            iface, &iname, &iargs, dyn_ty, &methods, slots, values, module, span,
        )
    }

    /// The struct, its implementation of `iface` and the interface value (module docs).
    #[allow(clippy::too_many_arguments)] // the parts the literal was checked into
    fn build_iface_object(
        &mut self,
        iface: DefId,
        iname: &str,
        iargs: &[TyId],
        dyn_ty: TyId,
        methods: &[MethodSig],
        slots: Vec<Option<Slot>>,
        values: Vec<(String, hir::Expr)>,
        module: usize,
        span: Span,
    ) -> hir::Expr {
        // The struct is generic over the type params its fields and the interface's arguments
        // mention (a literal in a generic function).
        let mut ps: Vec<u32> = vec![];
        for (_, h) in &values {
            crate::types::collect_params(&self.cx.ty, h.ty, &mut ps);
        }
        for a in iargs {
            crate::types::collect_params(&self.cx.ty, *a, &mut ps);
        }
        let map: Vec<(u32, TyId)> = ps
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, self.cx.ty.param(i as u32)))
            .collect();
        let norm = |cx: &mut crate::ctx::Ctx, t: TyId| {
            cx.ty.map(t, &mut |k| match k {
                TyKind::Param(i) => map.iter().find(|(p, _)| p == i).map(|(_, t)| *t),
                _ => None,
            })
        };
        let field_tys: Vec<(String, TyId)> = values
            .iter()
            .map(|(n, h)| (n.clone(), norm(self.cx, h.ty)))
            .collect();
        let d =
            self.cx
                .hidden_struct_def(&format!("{iname} literal"), &field_tys, ps.len(), module);
        let own: Vec<TyId> = (0..ps.len() as u32).map(|i| self.cx.ty.param(i)).collect();
        let self_ty = self.cx.ty.intern(TyKind::Adt(d, own));
        let niargs: Vec<TyId> = iargs.iter().map(|a| norm(self.cx, *a)).collect();
        let mut sargs = niargs.clone();
        sargs.push(self_ty);
        let host = Host::adt(self.cx, d);
        let mut impl_methods = vec![];
        for (slot, s) in slots.into_iter().enumerate() {
            let s = s.expect("ICE: every slot of an interface literal is filled");
            let def = match (s, methods.get(slot)) {
                (Slot::Value(j), _) => {
                    let ty = field_tys[j as usize].1;
                    crate::collect::getters::field_getter(self.cx, d, self_ty, j as usize, ty)
                }
                (s, Some(m)) => {
                    let params: Vec<ParamSig> = m
                        .params
                        .iter()
                        .map(|p| ParamSig {
                            ty: norm(self.cx, p.ty),
                            mode: if self.cx.is_copy(p.ty) {
                                PassMode::Copy
                            } else {
                                PassMode::Borrow
                            },
                            ..p.clone()
                        })
                        .collect();
                    let ret = norm(self.cx, m.ret);
                    let target = match s {
                        Slot::Call(j) => Target::Field {
                            index: j,
                            fn_ty: field_tys[j as usize].1,
                        },
                        _ => Target::Default(
                            m.default.expect("ICE: a default slot has a default"),
                            sargs.clone(),
                        ),
                    };
                    let name = format!("{iname} literal.{}", m.name);
                    synth_method(self.cx, &host, self_ty, name, params, ret, false, target)
                }
                (Slot::Call(j), None) => {
                    // A field read through a getter.
                    let fty = field_tys[j as usize].1;
                    let ret = match self.cx.ty.kind(fty) {
                        TyKind::FnPtr { ret, .. } => *ret,
                        _ => fty,
                    };
                    let name = format!("{iname} literal.<get>");
                    let target = Target::Field {
                        index: j,
                        fn_ty: fty,
                    };
                    synth_method(self.cx, &host, self_ty, name, vec![], ret, false, target)
                }
                (Slot::Default, None) => unreachable!("ICE: a field slot has no default"),
            };
            impl_methods.push(def);
        }
        self.cx.impls.push(ImplDef {
            ty: self_ty,
            generics: ps.len() as u32,
            iface,
            iface_args: niargs,
            methods: impl_methods,
        });
        let impl_index = self.cx.impls.len() as u32 - 1;
        // The dispatch groups gain the new methods (`throws/groups.rs`): built again on next use.
        self.cx.groups = None;
        let type_args: Vec<TyId> = ps.iter().map(|p| self.cx.ty.param(*p)).collect();
        let ty = self.cx.ty.intern(TyKind::Adt(d, type_args.clone()));
        let fields = values.into_iter().map(|(_, h)| h).collect();
        let lit = self.mk(
            H::AdtLit {
                def: d,
                type_args,
                fields,
            },
            ty,
            span,
        );
        self.mk(
            H::ToDyn {
                expr: Box::new(lit),
                impl_index,
            },
            dyn_ty,
            span,
        )
    }

    /// What a conditional function value (`c ? f : g`) throws: the errors of either branch,
    /// each checked against `exp` in a trial. Its branches are then checked against the
    /// function type throwing them all, which each converts to where it is made. `None` for
    /// any other value, or a branch that is no function.
    fn branches_throw(&mut self, e: &ast::Expr, exp: TyId) -> Option<TyId> {
        let mut e = e;
        while let ast::ExprKind::Paren(inner) = &e.kind {
            e = inner;
        }
        let ast::ExprKind::Cond { then, els, .. } = &e.kind else {
            return None;
        };
        let never = self.cx.ty.never;
        let mut acc = None;
        for b in [then, els] {
            let t = match self.branches_throw(b, exp) {
                Some(t) => t,
                None => self.trial_throws(b, exp)?,
            };
            acc = self.cx.join_errors(acc, (t != never).then_some(t));
        }
        Some(acc.unwrap_or(never))
    }

    /// What function value `e` throws, checked against `exp` and rolled back.
    fn trial_throws(&mut self, e: &ast::Expr, exp: TyId) -> Option<TyId> {
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let h = self.expr(e, Some(exp), Want::Move);
        let ok = self.cx.diags.len() == diags;
        mark.rollback(self.cx);
        self.restore_trial_frames(frames);
        let t = self.fn_throws(h.ty).filter(|_| ok)?;
        (!self.cx.ty.has_error(t)).then_some(t)
    }

    /// The value `e` written for slot `slot` of `iface` (expected type `exp`).
    fn slot_value(&mut self, e: &ast::Expr, exp: TyId, (iface, slot): (DefId, usize)) -> hir::Expr {
        let slot = Some((iface, slot as u32, e.span));
        let saved = std::mem::replace(&mut self.literal_slot, slot);
        let h = self.expr(e, Some(exp), Want::Move);
        self.literal_slot = saved;
        h
    }

    /// A closure written at `span` throws what `srcs` throw: for the interface slot it fills, if
    /// it is a value of an object literal implementing an interface ([`Self::slot_value`]).
    pub(super) fn provisional_slot(&mut self, span: Span, srcs: &[crate::defs::ThrowSrc]) {
        if let Some((iface, slot, _)) = self.literal_slot.filter(|l| l.2 == span) {
            self.cx.provisional_slots.push((iface, slot, srcs.to_vec()));
        }
    }

    /// `(params) => ret throws err` (`err` unknown: `error`, so a closure's comes from its body).
    fn fn_type(&mut self, params: Vec<TyId>, ret: TyId, throws: Option<TyId>) -> TyId {
        let throws = throws.unwrap_or(self.cx.ty.error);
        self.cx.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
        })
    }

    /// The function type a literal's function value for method `m` is checked against.
    fn method_fn_type(&mut self, m: &MethodSig) -> TyId {
        let params = m.params.iter().map(|p| p.ty).collect();
        self.fn_type(params, m.ret, m.throws)
    }

    /// What a function value of type `t` throws (`None` if it is no function).
    fn fn_throws(&self, t: TyId) -> Option<TyId> {
        match self.cx.ty.kind(t) {
            TyKind::FnPtr { throws, .. } => Some(*throws),
            _ => None,
        }
    }

    /// `h` converted to method `m`'s function type, throwing what it throws (or what `m` allows).
    fn fit_fn(&mut self, h: hir::Expr, m: &MethodSig) -> hir::Expr {
        let throws = m
            .throws
            .or_else(|| self.fn_throws(h.ty))
            .unwrap_or(self.cx.ty.never);
        let params = m.params.iter().map(|p| p.ty).collect();
        let t = self.fn_type(params, m.ret, Some(throws));
        self.coerce(h, t)
    }
}
