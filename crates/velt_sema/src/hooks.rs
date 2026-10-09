//! The methods a class can give the formatters and `JSON.stringify` (`hir::AdtDef::to_string`,
//! `to_json` and `inspect`): `toString()` for `String(x)` / `${x}`, `toJSON(key)` for
//! `JSON.stringify`, as in JS, and `__inspect()` for `console.log` (std's types print their
//! state that way, as Node's custom inspect does, never their runtime handles).
//!
//! Lowering calls a hook from glue, so a hook is a plain method of the class or a base class:
//! `(this)`, or `(this, key: string)` for `toJSON`, neither async nor a generator, with no type
//! parameters of its own, that cannot throw (`toString` returns a `string`). A user method of
//! another supported shape gets an **adapter** (`C.<toString>`, `C.<toJSON>`, synthesized after
//! the bodies are checked) of that plain shape, which calls it as JS does: through the vtable
//! when a subclass overrides it, with the defaults of the parameters JS leaves `undefined`, and
//! with `String(result)` for a `toString()` that returns something else. Any other shape is a
//! compile error ([`check_shapes`], [`check_throws`]), never silently left out.

use velt_common::Diagnostic;

use crate::collect::{lookup_method, Found};
use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, FnKind, ParamSig, ThisSig, ThrowSrc};
use crate::hir::{
    self, AdtKind, Callee, Def, DefId, ExprKind as H, Intrinsic, LocalDef, LocalId, PassMode, TyId,
    UseMode,
};

/// The `toString()` hook's name.
pub(crate) const TO_STRING: &str = "toString";
/// The `toJSON()` hook's name.
pub(crate) const TO_JSON: &str = "toJSON";
/// The `__inspect()` hook's name.
pub(crate) const INSPECT: &str = "__inspect";

/// Class `d<args>`'s hook method `name` (its adapter, if it has one) and its result type
/// (substituted with `args`), if it has one.
pub(crate) fn hook(cx: &mut Ctx, d: DefId, args: &[TyId], name: &str) -> Option<(DefId, TyId)> {
    if cx.adt(d)?.kind != AdtKind::Class {
        return None;
    }
    let found = lookup_method(cx, d, args, name)?;
    let Found::Class { owner, .. } = &found else {
        return None;
    };
    if found.is_static() {
        return None;
    }
    let owner_generics = cx.adt(*owner)?.generics.len();
    let m = found.def();
    let m = cx.hook_adapters.get(&m).copied().unwrap_or(m);
    let f = cx.fn_info(m);
    let key = name == TO_JSON && f.params.len() == 1 && f.params[0].ty == cx.ty.str_;
    if (!f.params.is_empty() && !key)
        || f.is_async
        || f.is_generator
        || f.is_async_gen
        || f.generics.len() != owner_generics
    {
        return None;
    }
    let ret = f.ret;
    let ret = cx.subst(ret, &found.owner_args());
    Some((m, ret))
}

/// [`hook`], for lowering (after all bodies): only a method that cannot throw, since glue has
/// nowhere to send an error (`toString` must also return a `string`). [`check_throws`] reports
/// the others.
pub(crate) fn final_hook(
    cx: &mut Ctx,
    d: DefId,
    args: &[TyId],
    name: &str,
) -> Option<(DefId, TyId)> {
    let (m, ret) = hook(cx, d, args, name)?;
    let throws = cx.fn_info(m).throws;
    if throws.is_some_and(|t| t != cx.ty.never) {
        return None;
    }
    if name == TO_STRING && ret != cx.ty.str_ {
        return None;
    }
    Some((m, ret))
}

/// Every class's own `toString` and `toJSON` instance methods, with their names.
fn hook_methods(cx: &Ctx) -> Vec<(DefId, DefId, &'static str)> {
    let mut out = vec![];
    for (i, info) in cx.info.iter().enumerate() {
        let DefInfo::Adt(a) = info else { continue };
        if a.kind != AdtKind::Class {
            continue;
        }
        for name in [TO_STRING, TO_JSON] {
            if let Some(m) = a.methods.get(name).filter(|m| !m.is_static) {
                if cx.fn_info(m.def).owner == Some(DefId(i as u32)) {
                    out.push((DefId(i as u32), m.def, name));
                }
            }
        }
    }
    out
}

/// After the bodies are checked: report a `toString` / `toJSON` method of a shape glue cannot
/// call, and give each other one that is not already a plain hook its adapter.
pub(crate) fn check_shapes(cx: &mut Ctx) {
    for (owner, m, name) in hook_methods(cx) {
        crate::body::param_defaults(cx, m);
        match shape(cx, owner, m, name) {
            Err((msg, fix)) => {
                let span = cx.fn_info(m).name_span;
                cx.error(Diagnostic::error(msg, span).with_note(fix));
            }
            Ok(Shape::Plain) => {}
            Ok(Shape::Adapter { key, slot, wrap }) => {
                let a = adapter(cx, owner, m, name, key, slot, wrap);
                cx.hook_adapters.insert(m, a);
            }
        }
    }
}

/// After throws inference: a `toString` / `toJSON` that can throw is an error (glue has
/// nowhere to send it).
pub(crate) fn check_throws(cx: &mut Ctx) {
    for (_, m, name) in hook_methods(cx) {
        let f = cx.fn_info(m);
        let Some(t) = f.throws.filter(|&t| t != cx.ty.never) else {
            continue;
        };
        let (span, tn) = (f.name_span, cx.display(t));
        let what = match name {
            TO_STRING => "`String(x)` and template literals call it",
            _ => "`JSON.stringify` calls it",
        };
        cx.error(
            Diagnostic::error(format!("`{name}` cannot throw (it throws `{tn}`)"), span)
                .with_note(format!("{what}, and the error would have nowhere to go"))
                .with_note(format!(
                    "catch the error inside `{name}` and return a value instead"
                )),
        );
    }
}

/// How glue calls a hook method.
enum Shape {
    /// As it is: `(this)`, not overridden (`toString` returns a `string`).
    Plain,
    /// Through an adapter: passing the key (`toJSON(key)`), through vtable slot `slot`,
    /// writing the result with `String(result)` (`wrap`).
    Adapter {
        key: bool,
        slot: Option<u32>,
        wrap: bool,
    },
}

/// The shape of class `owner`'s method `m` named `name`, or the error and its fix-it.
fn shape(cx: &mut Ctx, owner: DefId, m: DefId, name: &str) -> Result<Shape, (String, String)> {
    let a = cx.adt(owner).expect("ICE: hook owner");
    let owner_generics = a.generics.len();
    let slot = (name == TO_STRING)
        .then(|| a.vslots.get(name).copied())
        .flatten();
    let f = cx.fn_info(m);
    if f.is_async || f.is_generator || f.is_async_gen {
        return Err((
            format!("`{name}` cannot be async or a generator"),
            format!("make `{name}` a plain method that returns its result"),
        ));
    }
    if f.generics.len() != owner_generics {
        return Err((
            format!("`{name}` cannot have type parameters of its own"),
            format!("remove the type parameters of `{name}`"),
        ));
    }
    let key = name == TO_JSON && !f.params.is_empty();
    if key && f.params[0].ty != cx.ty.str_ {
        let p = &f.params[0].name;
        return Err((
            "the parameter of `toJSON` is the property key, a `string`".to_string(),
            format!("declare it `{p}: string` (`JSON.stringify` passes `\"\"` for the top value)"),
        ));
    }
    if let Some(p) = f
        .params
        .iter()
        .skip(usize::from(key))
        .find(|p| p.default.is_none())
    {
        let caller = match name {
            TO_STRING => "`String(x)` and template literals call `toString()` without arguments",
            _ => "`JSON.stringify` passes `toJSON` only the key",
        };
        return Err((
            format!("parameter `{}` of `{name}` needs a default value", p.name),
            format!("{caller}; give `{}` a default value", p.name),
        ));
    }
    let ret = f.ret;
    let wrap = name == TO_STRING && ret != cx.ty.str_;
    if wrap && !crate::body::printable(cx, ret) {
        let tn = cx.display(ret);
        return Err((
            format!("`toString` returns `{tn}`, which has no string form"),
            "return a `string`".to_string(),
        ));
    }
    let f = cx.fn_info(m);
    if f.params.is_empty() && slot.is_none() && !wrap {
        return Ok(Shape::Plain);
    }
    Ok(Shape::Adapter { key, slot, wrap })
}

/// The adapter `C.<name>` of class `owner`'s method `m`: `(this[, key: string])` calling
/// `this.m([key, ]defaults…)` (through vtable `slot`), its result written with `String(…)`
/// when `wrap`.
fn adapter(
    cx: &mut Ctx,
    owner: DefId,
    m: DefId,
    name: &str,
    key: bool,
    slot: Option<u32>,
    wrap: bool,
) -> DefId {
    let a = cx.adt(owner).expect("ICE: hook owner");
    let (generics, module, qual) = (a.generics.clone(), a.module, a.qual_name.clone());
    let span = cx.fn_info(m).name_span;
    let n = generics.len();
    let self_ty = crate::collect::self_type(cx, owner, n);
    let targs: Vec<TyId> = (0..n as u32).map(|i| cx.ty.param(i)).collect();
    let str_ = cx.ty.str_;
    let params: Vec<ParamSig> = match key {
        true => vec![ParamSig {
            name: "key".into(),
            span,
            ty: str_,
            mode: PassMode::Borrow,
            default: None,
        }],
        false => vec![],
    };
    let ret = if wrap { str_ } else { cx.fn_info(m).ret };
    let adapter_name = format!("{qual}.<{name}>");
    let mut info = crate::collect::fn_placeholder(
        adapter_name.clone(),
        span,
        span,
        module,
        FnKind::Method,
        None,
    );
    info.generics = generics;
    info.this = Some(ThisSig {
        ty: self_ty,
        mode: PassMode::Borrow,
    });
    info.params = params.clone();
    info.ret = ret;
    info.state = BodyState::Done;
    info.owner = Some(owner);
    info.throw_srcs = vec![ThrowSrc::Call(m, targs.clone(), span)];
    info.local_kinds = std::iter::once(crate::body::LocalKind::This)
        .chain(params.iter().map(|_| crate::body::LocalKind::Param))
        .collect();
    let def = cx.alloc_def(span, DefInfo::Fn(Box::new(info)));
    let local = |name: &str, ty| LocalDef {
        name: name.to_string(),
        ty,
        mutable: false,
        boxed: false,
        span,
    };
    let mut locals = vec![local("this", self_ty)];
    let ex = |kind, ty| hir::Expr { kind, ty, span };
    let mut args = vec![ex(H::Local(LocalId(0), UseMode::Borrow), self_ty)];
    if key {
        locals.push(local("key", str_));
        args.push(ex(H::Local(LocalId(1), UseMode::Borrow), str_));
    }
    let defaults: Vec<hir::Expr> = cx.fn_info(m).params[usize::from(key)..]
        .iter()
        .map(|p| {
            let mut d = p.default.clone().expect("ICE: hook parameter default");
            d.span = span;
            d
        })
        .collect();
    args.extend(defaults);
    let callee = match slot {
        Some(slot) => Callee::Virtual { slot },
        None => Callee::Def(m, targs),
    };
    let mret = cx.fn_info(m).ret;
    let mut call = ex(H::Call { callee, args }, mret);
    if wrap {
        call = ex(
            H::Call {
                callee: Callee::Intrinsic(Intrinsic::ToString),
                args: vec![call],
            },
            str_,
        );
    }
    let fndef = hir::FnDef {
        name: adapter_name,
        generics: n as u32,
        params: std::iter::once(hir::Param {
            local: LocalId(0),
            ty: self_ty,
            mode: PassMode::Borrow,
        })
        .chain(key.then_some(hir::Param {
            local: LocalId(1),
            ty: str_,
            mode: PassMode::Borrow,
        }))
        .collect(),
        ret,
        is_async: false,
        is_generator: false,
        self_ty: Some(self_ty),
        captures: vec![],
        shares_captures: false,
        body: hir::Body {
            locals,
            block: hir::Block {
                stmts: vec![hir::Stmt {
                    kind: hir::StmtKind::Return(Some(call)),
                    span,
                }],
                value: None,
                span,
            },
        },
        throws: None,
        span,
    };
    cx.defs[def.0 as usize] = Some(Def::Fn(fndef));
    def
}
