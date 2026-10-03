//! Default parameter values (`f(a: i64, b: i64 = 2)`): checked once per signature, in the
//! declaring module, and inserted at call sites that omit the argument (`args::check_call`).
//! Interface methods have their own defaults (used for calls through interface values and
//! bounded generics); an inherited interface method takes the defaults of the ancestor that
//! declares it, substituted into the inheriting interface.

use velt_syntax::ast;

use super::driver::detached;
use super::Want;
use crate::ctx::Ctx;
use crate::defs::{member_key, FnSource};
use crate::hir::{DefId, TyId};

pub(super) fn fn_sig_ast<'m>(src: FnSource<'m>) -> &'m ast::FnSig {
    match src {
        FnSource::Decl(f) => &f.sig,
        FnSource::Default(s, _) => s,
    }
}

/// Check the parameter defaults of function `d` (once; later calls return at once).
pub(crate) fn param_defaults(cx: &mut Ctx, d: DefId) {
    if !cx.defaults_checked.insert(d) {
        return;
    }
    let f = cx.fn_info(d);
    let Some(src) = f.source else { return };
    let (module, names, name, owner) =
        (f.module, f.generics.names.clone(), f.name.clone(), f.owner);
    let sig = fn_sig_ast(src);
    for (i, p) in sig.params.iter().enumerate() {
        let Some(e) = &p.default else { continue };
        let ty = cx.fn_info(d).params[i].ty;
        let h = check_default(cx, module, &names, &name, owner, e, ty);
        cx.fn_info_mut(d).params[i].default = Some(h);
    }
}

fn check_default(
    cx: &mut Ctx,
    module: usize,
    names: &[String],
    fn_name: &str,
    owner: Option<DefId>,
    e: &ast::Expr,
    ty: TyId,
) -> crate::hir::Expr {
    let saved = std::mem::replace(&mut cx.display_params, names.to_vec());
    let mut fcx = detached(cx, module, names);
    fcx.fn_name = fn_name.to_string();
    fcx.owner = owner;
    let h = fcx.expr_coerce(e, ty, Want::Move);
    cx.display_params = saved;
    h
}

/// Defaults of an arrow's parameters (`(x: T = e) => …`), checked like a function's: in the
/// module's scope, and evaluated at the call site.
pub(crate) fn arrow_defaults(
    cx: &mut Ctx,
    module: usize,
    closure: DefId,
    params: &[ast::ArrowParam],
) {
    let name = cx.fn_info(closure).name.clone();
    for (i, p) in params.iter().enumerate() {
        let Some(e) = &p.default else { continue };
        let Some(ty) = cx.fn_info(closure).params.get(i).map(|p| p.ty) else {
            continue;
        };
        let h = check_default(cx, module, &[], &name, None, e, ty);
        cx.fn_info_mut(closure).params[i].default = Some(h);
    }
}

/// The type of an arrow parameter written with only a default (`(digits = 2) => …`), as TS
/// infers it.
pub(crate) fn default_type(cx: &mut Ctx, module: usize, e: &ast::Expr) -> TyId {
    let mut fcx = detached(cx, module, &[]);
    let h = fcx.expr(e, None, Want::Move);
    fcx.widen_value(h).ty
}

pub(super) fn iface_defaults(cx: &mut Ctx) {
    let ifaces: Vec<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .filter(|d| cx.iface(*d).is_some_and(|i| i.decl.is_some()))
        .collect();
    for &d in &ifaces {
        own_iface_defaults(cx, d);
    }
    for &d in &ifaces {
        inherited_iface_defaults(cx, d);
    }
}

fn own_iface_defaults(cx: &mut Ctx, d: DefId) {
    let i = cx.iface(d).expect("ICE: iface");
    let (decl, module, qual) = (i.decl.expect("ICE: decl"), i.module, i.qual_name.clone());
    let mut names = i.generics.names.clone();
    names.push("Self".into());
    for m in &decl.methods {
        let key = member_key(&m.sig.name.name, m.is_setter);
        let Some(slot) = cx
            .iface(d)
            .and_then(|i| i.methods.iter().position(|x| x.name == key))
        else {
            continue;
        };
        for (k, p) in m.sig.params.iter().enumerate() {
            let Some(e) = &p.default else { continue };
            let ty = cx.iface(d).expect("ICE: iface").methods[slot].params[k].ty;
            let fname = format!("{qual}.{key}");
            let h = check_default(cx, module, &names, &fname, Some(d), e, ty);
            iface_method_mut(cx, d, slot).params[k].default = Some(h);
        }
    }
}

fn iface_method_mut<'a>(
    cx: &'a mut Ctx,
    d: DefId,
    slot: usize,
) -> &'a mut crate::defs::IfaceMethod {
    match &mut cx.info[d.0 as usize] {
        crate::defs::DefInfo::Iface(i) => &mut i.methods[slot],
        _ => panic!("ICE: def {d:?} is not an interface"),
    }
}

/// Copy the declaring ancestor's defaults into the inherited slots of `d`.
fn inherited_iface_defaults(cx: &mut Ctx, d: DefId) {
    let i = cx.iface(d).expect("ICE: iface");
    let own: Vec<String> = i
        .decl
        .map(|decl| {
            decl.methods
                .iter()
                .map(|m| member_key(&m.sig.name.name, m.is_setter))
                .collect()
        })
        .unwrap_or_default();
    let (n, parents, methods) = (
        i.generics.len() as u32,
        i.parents.clone(),
        i.methods.clone(),
    );
    for (slot, m) in methods.iter().enumerate() {
        if own.contains(&m.name) {
            continue;
        }
        let declaring = parents.iter().find_map(|a| {
            let ai = cx.iface(a.iface)?;
            let declares = ai
                .decl?
                .methods
                .iter()
                .any(|x| member_key(&x.sig.name.name, x.is_setter) == m.name);
            let am = ai.methods.iter().find(|x| x.name == m.name)?;
            declares.then(|| (a.args.clone(), am.params.clone()))
        });
        let Some((mut args, params)) = declaring else {
            continue;
        };
        args.push(cx.ty.param(n));
        for (k, p) in params.into_iter().enumerate() {
            let Some(mut h) = p.default else { continue };
            crate::visit::map_expr_types(&mut h, &mut |t| cx.ty.subst(t, &args));
            iface_method_mut(cx, d, slot).params[k].default = Some(h);
        }
    }
}
