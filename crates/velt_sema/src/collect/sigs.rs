//! Phase 3: signatures of functions, methods, constructors, interface methods (+ default
//! bodies) and `extend` blocks.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::declare::{fn_placeholder, generic_names, ASYNC_DISPOSE, DISPOSE};
use super::shapes::{resolve_bounds, self_type};
use super::ItemDefs;
use crate::ctx::Ctx;
use crate::defs::{
    is_setter_key, static_key, Bound, DeclaredThrows, DefInfo, Extension, FnKind, FnSource,
    Generics, IfaceMethod, MethodRef, ParamSig, RetSource, ThisSig,
};
use crate::hir::{DefId, PassMode, TyId, TyKind};
use crate::resolve::TyEnv;

pub(super) fn resolve_sigs(cx: &mut Ctx, items: &ItemDefs) {
    resolve_def_sigs(cx, items);
    let mut used = FxHashSet::default();
    for m in 0..cx.modules.len() {
        if cx.is_generated_module(m) {
            continue;
        }
        let module = &cx.modules[m];
        for item in &module.ast.items {
            if let ast::ItemKind::Extend(e) = &item.kind {
                extension(cx, m, e, &mut used);
            }
        }
    }
}

/// The signatures of the definitions `items`.
pub(super) fn resolve_def_sigs(cx: &mut Ctx, items: &ItemDefs) {
    let mut defs: Vec<DefId> = items.clone();
    defs.sort();
    for d in defs {
        match &cx.info[d.0 as usize] {
            DefInfo::Fn(f) => {
                let (module, source) = (f.module, f.source);
                let sig = match source {
                    Some(FnSource::Decl(decl)) => &decl.sig,
                    _ => extern_sig(cx, d, module),
                };
                // A type query in the signature that needs a body (`ReturnType<typeof f>` of a
                // function with an inferred result) resolves it again when collection is done.
                let reported = cx.diags.len();
                cx.sigq.sig_fill = true;
                cx.sigq.sig_deferred = false;
                fill_sig(cx, d, sig, &Generics::default(), module);
                cx.sigq.sig_fill = false;
                if cx.sigq.sig_deferred {
                    cx.diags.truncate(reported);
                    cx.sigq.deferred_sigs.insert(d, (sig, module));
                }
            }
            DefInfo::Adt(_) => adt_methods(cx, d),
            DefInfo::Iface(_) => iface_methods(cx, d),
            _ => {}
        }
    }
}

/// The signatures `resolve_sigs` deferred, now that bodies may be checked (`Ctx::bodies_ok`).
/// A type query may reach one still pending (`fill_pending`), so they are filled in dependency
/// order, whatever the declaration order.
pub(super) fn fill_deferred(cx: &mut Ctx) {
    while let Some((&d, _)) = cx.sigq.deferred_sigs.first_key_value() {
        fill_pending(cx, d);
    }
}

/// Fill `d`'s deferred signature now if it is still pending (a type query needs it).
pub(crate) fn fill_pending(cx: &mut Ctx, d: DefId) {
    let Some((sig, module)) = cx.sigq.deferred_sigs.remove(&d) else {
        return;
    };
    cx.sigq.filling_sigs.push((d, sig.name.name.clone()));
    fill_sig(cx, d, sig, &Generics::default(), module);
    cx.sigq.filling_sigs.pop();
}

/// A parameter type written `readonly T[]` (parsed as `ReadonlyArray<T>`, which is `T[]` from
/// here on): a `declare system function` buffer the C function only reads.
fn written_readonly(t: &ast::TypeExpr) -> bool {
    matches!(&t.kind, ast::TypeExprKind::Named { path, .. }
        if path.len() == 1 && path[0].name == "ReadonlyArray")
}

pub(crate) fn extern_sig<'m>(cx: &Ctx<'m>, d: DefId, module: usize) -> &'m ast::FnSig {
    let span = cx.def_spans[d.0 as usize];
    cx.modules[module]
        .ast
        .items
        .iter()
        .find_map(|i| match &i.kind {
            ast::ItemKind::ExternFn(s) if s.name.span == span => Some(s),
            _ => None,
        })
        .expect("ICE: extern fn declaration")
}

/// Mode a parameter starts with (before ownership / mutation inference): Copy types by value,
/// everything else borrowed.
pub(crate) fn declared_mode(cx: &mut Ctx, ty: TyId) -> PassMode {
    if cx.is_copy(ty) {
        PassMode::Copy
    } else {
        PassMode::Borrow
    }
}

fn params(cx: &mut Ctx, ps: &[ast::Param], env: &TyEnv) -> Vec<ParamSig> {
    let mut out: Vec<ParamSig> = vec![];
    for p in ps {
        if let Some(f) = cx.sigq.filling_sigs.last_mut() {
            f.1.clone_from(&p.name.name);
        }
        let mut ty = cx.resolve_type(&p.ty, env);
        if p.rest && !rest_param_type(cx, p.ty.span, ty) {
            ty = cx.ty.error;
        }
        if out.iter().any(|q| q.name == p.name.name) {
            cx.err(
                format!(
                    "identifier `{}` is bound more than once in this parameter list",
                    p.name.name
                ),
                p.name.span,
            );
        }
        if ty == cx.ty.unit {
            cx.err(
                format!("parameter `{}` cannot have type `void`", p.name.name),
                p.ty.span,
            );
        }
        let mode = declared_mode(cx, ty);
        if crate::body::is_readonly_array(&p.ty) {
            cx.sigq.readonly_params.insert(p.name.span);
        }
        cx.note_written_type(p.name.span, p.ty.span);
        out.push(ParamSig {
            name: p.name.name.clone(),
            span: p.name.span,
            ty,
            mode,
            default: None,
        });
    }
    out
}

/// A rest parameter's type must be an array type: `T[]`, `readonly T[]` or an alias of one
/// (TypeScript's TS2370 otherwise). One typed by a type parameter (`...args: A` with
/// `A extends unknown[]`) is not supported yet. False after reporting an error at `at` (the
/// written type). Closures' rest parameters are checked the same way.
pub(crate) fn rest_param_type(cx: &mut Ctx, at: Span, ty: TyId) -> bool {
    match cx.ty.kind(ty) {
        TyKind::Array(_) | TyKind::Error => return true,
        TyKind::Param(_) => cx.error(
            Diagnostic::error(
                "a rest parameter typed by a type parameter (`...args: A`) is not supported yet (#943)",
                at,
            )
            .with_note("Velt passes the rest arguments as an array: type the parameter as one (`...args: unknown[]`)"),
        ),
        _ => cx.err(
            "a rest parameter must have an array type (`...xs: T[]`)",
            at,
        ),
    }
    false
}

/// Resolve `sig` into the placeholder `d`; generics = `owner` generics followed by the fn's own.
fn fill_sig(cx: &mut Ctx, d: DefId, sig: &ast::FnSig, owner: &Generics, module: usize) {
    let mut generics = owner.clone();
    for g in &sig.generics {
        generics.push(&g.name.name);
    }
    let mut env = TyEnv::new(module, &generics.names);
    env.this_ty = cx.fn_info(d).this_type();
    let own = resolve_bounds(cx, &sig.generics, &env);
    generics.bounds.truncate(owner.len());
    generics.bounds.extend(own.bounds);
    if !own.constraints.is_empty() || !generics.constraints.is_empty() {
        generics.constraints.resize(owner.len(), None);
        generics.constraints.extend(own.constraints);
        generics.constraints.resize(generics.len(), None);
    }
    if sig.generics.iter().any(|g| g.default.is_some()) {
        let mut defaults = generics.defaults.clone();
        defaults.resize(owner.len(), None);
        defaults.extend(cx.fn_param_defaults(&sig.generics, &env));
        generics.defaults = defaults;
    }
    let kind = cx.fn_info(d).kind;
    // `function f(this: T, ...)`: `this` is a leading parameter (`body::expr::this_param`).
    if let (Some(t), FnKind::Free | FnKind::Closure) = (&sig.this_param, kind) {
        let ty = cx.resolve_type(t, &env);
        let resumable = sig.is_async || sig.is_generator;
        cx.fn_info_mut(d).this = Some(method_this(ty, false, resumable));
    }
    let mut ps = params(cx, &sig.params, &env);
    if sig.is_intrinsic {
        crate::intrinsic_decls::param_modes(cx, module, sig, &mut ps);
    }
    if let Some(f) = cx.sigq.filling_sigs.last_mut().filter(|f| f.0 == d) {
        f.1.clone_from(&sig.name.name);
    }
    // A generator declares its result (`generator_sig`), so it is never inferred.
    let inferred = sig.ret.is_none() && !sig.is_generator && infers_ret(cx, d);
    let (mut ret, ret_span) = match &sig.ret {
        Some(t) => (cx.resolve_async_aware(t, &env, sig.is_async), Some(t.span)),
        None if inferred => (cx.ty.error, None),
        None => (cx.ty.unit, None),
    };
    let mut throws = throws_clause(cx, sig, &env);
    if sig.is_async && !sig.is_generator {
        ret = async_ret(cx, ret, sig);
        match kind {
            FnKind::Ctor => cx.err("constructors cannot be `async`", sig.name.span),
            FnKind::Extern => {}
            _ => owned_async_params(cx, &mut ps),
        }
        (ret, throws) = promise_throws(cx, ret, throws, sig);
    }
    if sig.is_generator {
        (ret, throws) = super::generator_sig::generator_sig(cx, kind, ret, throws, sig, &mut ps);
    }
    if kind == FnKind::Extern && throws.is_some() {
        cx.err(
            "`declare function` cannot have a `throws` clause",
            sig.throws.as_ref().map_or(sig.span, |t| t.span),
        );
        throws = None;
    }
    if kind == FnKind::Extern && sig.is_system && !sig.generics.is_empty() {
        cx.err(
            "a system function has no type parameters: its C signature fixes every type",
            sig.generics[0].name.span,
        );
    }
    // A runtime function std declares writes the `string` it takes as `out` or `buf`: a string
    // builder it appends to, or the slot it puts its result in (docs/reference/modules.md
    // "Runtime declarations"). Its argument is a mutable borrow, so the owner of the place is
    // modified too: a `StringBuilder` param of a function that appends to it is no read-only
    // (`frozen`) one whose fields the optimizer may read once.
    if kind == FnKind::Extern && !sig.is_system && !sig.is_intrinsic && cx.scopes[module].is_std {
        for p in &mut ps {
            let written = p.name == "out" || p.name == "buf";
            if written && p.mode == PassMode::Borrow && matches!(cx.ty.kind(p.ty), TyKind::Str) {
                p.mode = PassMode::BorrowMut;
            }
        }
    }
    let f = cx.fn_info_mut(d);
    if kind == FnKind::Extern && sig.is_system {
        f.system = Some(sig.params.iter().map(|p| written_readonly(&p.ty)).collect());
    }
    f.declared_throws = throws;
    f.is_async = sig.is_async && !sig.is_generator;
    f.is_generator = sig.is_generator;
    f.is_async_gen = sig.is_async && sig.is_generator;
    f.generics = generics;
    f.params = ps;
    // A system function writes every buffer and struct not written `readonly`
    // (docs/reference/system-functions.md): its argument is a mutable borrow from the start, so
    // the call is checked as one (a read-only array is rejected, a module constant is not
    // copied, another argument may not reach it) before ownership inference runs.
    if let Some(readonly) = f.system.clone() {
        for (p, ro) in f.params.iter_mut().zip(readonly) {
            if p.mode == PassMode::Borrow && !ro {
                p.mode = PassMode::BorrowMut;
            }
        }
    }
    f.ret = ret;
    f.ret_span = ret_span;
    if inferred {
        f.ret_source = RetSource::Body;
    }
    // Interface getters are reported with the interface's methods.
    if f.is_getter && sig.ret.is_none() && !inferred && kind != FnKind::IfaceDefault {
        cx.error(
            Diagnostic::error("a getter must return a value", sig.name.span)
                .with_note("return the property's value, or write its type: `get name(): T`"),
        );
    }
}

/// Does `d`, written without a return type, take it from its body? Functions and methods
/// (not setters) whose body has a `return` with a value; the others return `void`.
fn infers_ret(cx: &Ctx, d: DefId) -> bool {
    let f = cx.fn_info(d);
    let key = f.name.rsplit('.').next().unwrap_or_default();
    matches!(f.kind, FnKind::Free | FnKind::Method | FnKind::Static)
        && !is_setter_key(key)
        && super::ret_infer::returns_value(f.source)
}

/// The result and error type of a function whose promise carries its errors (an async function,
/// an interface method returning a promise): `Promise<T, E>` written as the result is the same
/// as `throws E`; the result becomes `Promise<T>` and the clause what the promise rejects with.
fn promise_throws(
    cx: &mut Ctx,
    ret: TyId,
    throws: Option<DeclaredThrows>,
    sig: &ast::FnSig,
) -> (TyId, Option<DeclaredThrows>) {
    let Some(v) = cx.ty.promise_payload(ret) else {
        return (ret, throws);
    };
    let written = cx.ty.promise_error(ret).filter(|e| *e != cx.ty.never);
    let throws = match throws {
        Some(DeclaredThrows { ty, span, .. }) => Some(DeclaredThrows {
            ty: cx.join_errors(ty, written),
            span,
            from_body: false,
        }),
        None => written.map(|e| DeclaredThrows {
            ty: Some(e),
            span: sig.ret.as_ref().map_or(sig.name.span, |t| t.span),
            from_body: false,
        }),
    };
    (cx.ty.promise(v), throws)
}

/// A written `throws E` clause.
pub(super) fn throws_clause(cx: &mut Ctx, sig: &ast::FnSig, env: &TyEnv) -> Option<DeclaredThrows> {
    let t = sig.throws.as_ref()?;
    let ty = cx.resolve_type(t, env);
    cx.no_void_error(ty, t.span);
    Some(DeclaredThrows {
        ty: cx.canon_error(Some(ty)),
        span: t.span,
        from_body: false,
    })
}

/// An async function's declared return type must be `Promise<T>` (omitted: `Promise<void>`).
fn async_ret(cx: &mut Ctx, ret: TyId, sig: &ast::FnSig) -> TyId {
    if matches!(cx.ty.kind(ret), TyKind::Promise(..)) || cx.ty.is_bottom(ret) {
        return ret;
    }
    if let Some(t) = &sig.ret {
        let found = cx.display(ret);
        cx.error(
            Diagnostic::error(
                "the return type of an async function must be `Promise<T>`",
                t.span,
            )
            .with_note(format!("write `Promise<{found}>`")),
        );
    }
    cx.ty.promise(ret)
}

/// Params of async functions are moved into the future (docs/reference/async.md):
/// always owned (or copied), never borrowed.
pub(super) fn owned_async_params(cx: &mut Ctx, ps: &mut [ParamSig]) {
    for p in ps.iter_mut() {
        p.mode = if cx.is_copy(p.ty) {
            PassMode::Copy
        } else {
            PassMode::Owned
        };
    }
}

/// `this` of a method: async or generator (`resumable`) → owned (kept by the future or the
/// generator); `mutates` (setters, `[Symbol.dispose]`)
/// → mutable borrow; otherwise borrowed until mutation inference (`crate::ownership`) decides.
fn method_this(ty: TyId, mutates: bool, resumable: bool) -> ThisSig {
    if resumable {
        return ThisSig {
            ty,
            mode: PassMode::Owned,
        };
    }
    this_sig(ty, mutates)
}

fn this_sig(ty: TyId, mutates: bool) -> ThisSig {
    ThisSig {
        ty,
        mode: if mutates {
            PassMode::BorrowMut
        } else {
            PassMode::Borrow
        },
    }
}

/// The declaring type of methods being collected.
struct Owner<'a> {
    d: DefId,
    module: usize,
    qual: &'a str,
    generics: &'a Generics,
    self_ty: TyId,
}

fn adt_methods(cx: &mut Ctx, d: DefId) {
    let a = cx.adt(d).expect("ICE: adt");
    let (decl, module, qual, generics) = (
        a.decl.expect("ICE: adt decl"),
        a.module,
        a.qual_name.clone(),
        a.generics.clone(),
    );
    let self_ty = self_type(cx, d, generics.len());
    let owner = Owner {
        d,
        module,
        qual: &qual,
        generics: &generics,
        self_ty,
    };
    let mut methods: FxHashMap<String, MethodRef> = FxHashMap::default();
    let instance: Vec<&str> = decl
        .methods
        .iter()
        .filter(|m| !m.is_static && !m.is_setter)
        .map(|m| m.decl.sig.name.name.as_str())
        .collect();
    for m in &decl.methods {
        let name = &m.decl.sig.name;
        let key = if m.is_static && instance.contains(&name.name.as_str()) {
            static_key(&name.name)
        } else {
            cx.check_method_key(module, name);
            cx.method_key(module, name, m.is_setter)
        };
        let def = method_def(cx, &owner, m, &key);
        if methods.contains_key(&key) {
            cx.err(
                format!("duplicate {} `{}`", what(m.is_setter), name.name),
                name.span,
            );
            continue;
        }
        if cx.adt(d).is_some_and(|a| {
            a.fields.iter().any(|f| f.name == name.name) || a.statics.contains_key(&name.name)
        }) {
            cx.err(
                format!("`{}` is both a field and a method", name.name),
                name.span,
            );
        }
        let r = MethodRef {
            def,
            is_static: m.is_static,
        };
        methods.insert(key, r);
    }
    let ctor = decl.constructor.as_ref().map(|c| ctor_def(cx, &owner, c));
    let a = cx.adt_mut(d);
    a.methods = methods;
    a.own_ctor = ctor;
}

/// The def of method `m`, whose method-table key is `key` (also the last part of its qualified
/// name, which names its symbol).
fn method_def<'m>(cx: &mut Ctx<'m>, o: &Owner, m: &'m ast::Method, key: &str) -> DefId {
    let name = &m.decl.sig.name;
    let kind = if m.is_static {
        FnKind::Static
    } else {
        FnKind::Method
    };
    let src = Some(FnSource::Decl(&m.decl));
    let full = format!("{}.{key}", o.qual);
    let mut info = fn_placeholder(full, name.span, m.decl.sig.span, o.module, kind, src);
    info.owner = Some(o.d);
    info.is_private = m.is_private;
    info.is_getter = m.is_getter;
    let resumable = m.decl.sig.is_async || m.decl.sig.is_generator;
    info.this = (!m.is_static).then(|| method_this(o.self_ty, always_mutates(m), resumable));
    let def = cx.alloc_def(name.span, DefInfo::Fn(Box::new(info)));
    fill_sig(cx, def, &m.decl.sig, o.generics, o.module);
    if is_dispose(m) {
        let f = cx.fn_info(def);
        let async_ = m.decl.sig.is_async;
        if async_ || !f.params.is_empty() || f.ret != cx.ty.unit || !m.decl.sig.generics.is_empty()
        {
            cx.err(
                "`[Symbol.dispose]` must not be `async`, take no parameters and return `void`",
                name.span,
            );
        }
    }
    if !m.is_static && name.name == ASYNC_DISPOSE {
        let f = cx.fn_info(def);
        let ret_ok = matches!(cx.ty.kind(f.ret), TyKind::Promise(t, _) if *t == cx.ty.unit);
        if !m.decl.sig.is_async || !f.params.is_empty() || !ret_ok {
            cx.err(
                "`[Symbol.asyncDispose]` must be `async`, take no parameters and return `Promise<void>`",
                name.span,
            );
        }
    }
    def
}

/// "method" / "setter", for messages about duplicates.
fn what(is_setter: bool) -> &'static str {
    if is_setter {
        "setter"
    } else {
        "method"
    }
}

fn is_dispose(m: &ast::Method) -> bool {
    !m.is_static && m.decl.sig.name.name == DISPOSE
}

/// Setters and the `[Symbol.dispose]()` drop hook always mutate `this` (other methods are
/// inferred).
fn always_mutates(m: &ast::Method) -> bool {
    m.is_setter || is_dispose(m)
}

fn ctor_def<'m>(cx: &mut Ctx<'m>, o: &Owner, c: &'m ast::FnDecl) -> DefId {
    let full = format!("{}.constructor", o.qual);
    let src = Some(FnSource::Decl(c));
    let mut info = fn_placeholder(
        full,
        c.sig.name.span,
        c.sig.span,
        o.module,
        FnKind::Ctor,
        src,
    );
    info.owner = Some(o.d);
    info.this = Some(this_sig(o.self_ty, true));
    let def = cx.alloc_def(c.sig.name.span, DefInfo::Fn(Box::new(info)));
    fill_sig(cx, def, &c.sig, o.generics, o.module);
    if !c.sig.generics.is_empty() {
        cx.err("constructors cannot have type parameters", c.sig.name.span);
    }
    def
}

fn iface_methods(cx: &mut Ctx, d: DefId) {
    let i = cx.iface(d).expect("ICE: iface");
    let (decl, module, qual, generics) = (
        i.decl.expect("ICE: iface decl"),
        i.module,
        i.qual_name.clone(),
        i.generics.clone(),
    );
    let mut methods: Vec<IfaceMethod> = vec![];
    for m in &decl.methods {
        let name = &m.sig.name;
        cx.check_method_key(module, name);
        let key = cx.method_key(module, name, m.is_setter);
        if m.sig.is_async && m.body.is_none() {
            cx.error(
                Diagnostic::error(
                    "only an interface method with a body can be `async`",
                    name.span,
                )
                .with_note(
                    "declare the method as returning a `Promise`; implementations may be `async`",
                ),
            );
        }
        if m.is_getter && m.sig.ret.is_none() {
            cx.error(
                Diagnostic::error("an interface getter must declare its type", name.span)
                    .with_note(format!("write `get {}(): T`", name.name)),
            );
        }
        let (own, env) = method_generics(cx, &generics, &m.sig.generics, module);
        if let (Some(gp), Some(_)) = (m.sig.generics.first(), &m.body) {
            cx.err(
                "generic interface methods cannot have a default body yet",
                gp.name.span,
            );
        }
        // No promise sites in a generic interface method's parameters: what a call stores there
        // would not reach the implementations' own parameter types, which a generic method of
        // an object literal does not compare (`throws::promise_sites`).
        let off = !m.sig.generics.is_empty() || cx.fn_sites.async_ret;
        let outer = std::mem::replace(&mut cx.fn_sites.async_ret, off);
        let ps = params(cx, &m.sig.params, &env);
        cx.fn_sites.async_ret = outer;
        // Where its result is written, for the site there (`throws::promise_sites`).
        if let Some(t) = &m.sig.ret {
            cx.note_written_type(name.span, t.span);
        }
        let mut ret = match &m.sig.ret {
            Some(t) => cx.resolve_type(t, &env),
            None => cx.ty.unit,
        };
        // An async default: its def (`fill_sig`) reports a result that is not a promise.
        let not_promise = !matches!(cx.ty.kind(ret), TyKind::Promise(..)) && !cx.ty.is_bottom(ret);
        if m.sig.is_async && m.body.is_some() && not_promise {
            ret = cx.ty.promise(ret);
        }
        let throws = throws_clause(cx, &m.sig, &env);
        // Like an async function (and a function type): the method's promise carries its errors.
        let (ret, throws) = promise_throws(cx, ret, throws, &m.sig);
        let default = m
            .body
            .as_ref()
            .map(|body| default_method(cx, d, &qual, &generics, m, body, module));
        if methods.iter().any(|x| x.name == key) {
            let msg = match m.is_setter || m.body.is_some() {
                true => format!("duplicate {} `{}`", what(m.is_setter), name.name),
                // TypeScript's overloads in an interface.
                false => format!("overloaded interface methods are not supported yet: declare `{}` once, with parameters that take every form (`a: string | number`)", name.name),
            };
            cx.err(msg, name.span);
            continue;
        }
        methods.push(IfaceMethod {
            name: key,
            span: name.span,
            params: ps,
            ret,
            mut_this: m.is_setter,
            default,
            is_getter: m.is_getter,
            generics: own,
            throws,
        });
    }
    let DefInfo::Iface(i) = &mut cx.info[d.0 as usize] else {
        unreachable!("ICE: iface")
    };
    i.methods = methods;
}

/// The own type params of an interface method (numbered after the interface's params and the
/// implementor) and the environment its signature resolves in.
fn method_generics(
    cx: &mut Ctx,
    iface: &Generics,
    gs: &[ast::GenericParam],
    module: usize,
) -> (Generics, TyEnv) {
    let mut names = iface.names.clone();
    // Not a writable name: the implementor is only reachable as `this`.
    names.push("<Self>".into());
    let mut own = Generics::default();
    for g in gs {
        names.push(g.name.name.clone());
        own.push(&g.name.name);
    }
    let env = TyEnv::new(module, &names);
    let b = super::shapes::resolve_bounds(cx, gs, &env);
    own.bounds = b.bounds;
    own.constraints = b.constraints;
    (own, env)
}

/// A default method body: generic over the interface's params plus `Self` (the implementor),
/// which is bounded by the interface itself.
fn default_method<'m>(
    cx: &mut Ctx<'m>,
    iface: DefId,
    qual: &str,
    generics: &Generics,
    m: &'m ast::InterfaceMethod,
    body: &'m ast::Block,
    module: usize,
) -> DefId {
    let name = &m.sig.name;
    let mut info = fn_placeholder(
        format!("{qual}.{}", cx.method_key(module, name, m.is_setter)),
        name.span,
        m.sig.span,
        module,
        FnKind::IfaceDefault,
        Some(FnSource::Default(&m.sig, body)),
    );
    let mut g = generics.clone();
    let n = g.len() as u32;
    g.push("Self");
    let args = (0..n).map(|i| cx.ty.param(i)).collect();
    g.bounds[n as usize].push(Bound { iface, args });
    let self_ty = cx.ty.param(n);
    info.owner = Some(iface);
    info.is_getter = m.is_getter;
    info.this = Some(method_this(self_ty, m.is_setter, m.sig.is_async));
    let def = cx.alloc_def(name.span, DefInfo::Fn(Box::new(info)));
    fill_sig(cx, def, &m.sig, &g, module);
    def
}

fn extension<'m>(cx: &mut Ctx<'m>, m: usize, e: &'m ast::ExtendDecl, used: &mut FxHashSet<String>) {
    let mut generics = generic_names(&e.generics);
    let env = TyEnv::new(m, &generics.names);
    let b = resolve_bounds(cx, &e.generics, &env);
    if b.constraints.iter().any(Option::is_some) {
        let at = e
            .generics
            .iter()
            .zip(&b.constraints)
            .find(|(_, c)| c.is_some());
        if let Some((g, Some(c))) = at {
            let tn = cx.display(*c);
            let span = g.bounds.last().map_or(g.name.span, |t| t.span);
            cx.err(format!("`{tn}` is not an interface"), span);
        }
    }
    generics.bounds = b.bounds;
    let target =
        super::ext_fn_errors::resolve_target(cx, &e.target, &env, &mut generics, &e.methods);
    let saved = std::mem::replace(&mut cx.display_params, generics.names.clone());
    let prefix = format!("{}{}", module_prefix(cx, m), cx.display(target));
    cx.display_params = saved;
    let mut methods = FxHashMap::default();
    for meth in &e.methods {
        let name = &meth.decl.sig.name;
        let key = cx.method_key(m, name, meth.is_setter);
        let mut full = format!("{prefix}.{key}");
        let mut n = 2;
        while !used.insert(full.clone()) {
            full = format!("{prefix}.{key}#{n}");
            n += 1;
        }
        let kind = if meth.is_static {
            FnKind::Static
        } else {
            FnKind::Method
        };
        let src = Some(FnSource::Decl(&meth.decl));
        let mut info = fn_placeholder(full, name.span, meth.decl.sig.span, m, kind, src);
        info.is_getter = meth.is_getter;
        let resumable = meth.decl.sig.is_async || meth.decl.sig.is_generator;
        info.this = (!meth.is_static).then(|| method_this(target, always_mutates(meth), resumable));
        // std's extension of the prelude `Record` (std/lang/record/symbols.vlt) holds
        // methods of its own, which read its fields.
        if let TyKind::Adt(d, _) = cx.ty.kind(target) {
            if cx.scopes[m].is_std && Some(*d) == cx.prelude_adt("Record") {
                info.owner = Some(*d);
            }
        }
        let def = cx.alloc_def(name.span, DefInfo::Fn(Box::new(info)));
        fill_sig(cx, def, &meth.decl.sig, &generics, m);
        if methods.contains_key(&key) {
            cx.err(
                format!("duplicate {} `{}`", what(meth.is_setter), name.name),
                name.span,
            );
            continue;
        }
        if meth.is_private {
            cx.err("`private` is not allowed in `extend` blocks", name.span);
        }
        let r = MethodRef {
            def,
            is_static: meth.is_static,
        };
        methods.insert(key, r);
    }
    let implements = e
        .implements
        .iter()
        .filter_map(|t| super::shapes::iface_bound(cx, t, &env).map(|b| (b, t.span)))
        .collect();
    cx.extensions.push(Extension {
        generics,
        target,
        methods,
        implements,
    });
}

fn module_prefix(cx: &Ctx, m: usize) -> String {
    if m == cx.root {
        String::new()
    } else {
        format!("{}::", cx.modules[m].path)
    }
}
