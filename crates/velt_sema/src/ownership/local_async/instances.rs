//! What a type that mentions type parameters is in the instantiations of the function that makes
//! it (`instances`): through the direct calls with type arguments (`Graph::insts`) and, for a
//! generic closure or an implementation's generic method, the calls through its interface
//! (`super::generic_closures`).

use crate::ctx::Ctx;
use crate::defs::FnKind;
use crate::hir::{Def, DefId, TyId, TyKind};

use super::graph::Graph;
use super::types;

/// The function whose instantiations resolve the type parameters a type in function `c` mentions:
/// the outermost function `c` is in, or the first generic closure on the way whose calls give its
/// instantiations (`generic_closures`): its own type parameters are what those calls give.
pub(super) fn instance_owner(cx: &Ctx, g: &Graph, c: DefId) -> DefId {
    let mut owner = c;
    let mut hops = 0;
    while let (Some(&p), true) = (g.parent.get(&owner), hops < 64) {
        if g.dyn_calls.sites(cx, g, owner).is_some() {
            break;
        }
        owner = p;
        hops += 1;
    }
    owner
}

/// The types closure literal `c` (of type `ty`) has in the instantiations of the generic
/// function that creates it: `(value: T) => void` in `withResolvers<T>` is `(value: Job) =>
/// void` where the program calls `withResolvers<Job>()`. Generic parameters of callers that
/// are generic themselves are followed to their own callers; where that gives no answer (or too
/// many), the generic type itself, whose parameters match anything; a generic library function
/// the program never calls gives none.
pub(super) fn instances(cx: &mut Ctx, g: &Graph, c: DefId, ty: TyId) -> Vec<TyId> {
    let owner = instance_owner(cx, g, c);
    let mut out = vec![];
    let mut work = vec![(owner, ty, 0)];
    while let Some((f, t, depth)) = work.pop() {
        let mut params = vec![];
        crate::types::collect_params(&cx.ty, t, &mut params);
        if params.is_empty() {
            out.push(t);
            continue;
        }
        // A method of an object type that goes into `any` or is converted: a dynamic call may
        // reach it with any type arguments, whatever its calls by name give (#2860).
        let escaped_self = matches!(&cx.defs[f.0 as usize], Some(Def::Fn(fd))
            if fd.self_ty.is_some_and(|s| matches!(cx.ty.kind(s), TyKind::Adt(d, _) if g.dyn_calls.type_escaped(*d))));
        if escaped_self {
            out.push(t);
            continue;
        }
        let mut sites = g.insts.get(&f).cloned().unwrap_or_default();
        // A generic closure's, or an implementation's generic method's, calls through its
        // interface (`generic_closures`), next to any direct ones.
        if let Some(s) = g.dyn_calls.sites(cx, g, f) {
            // Called only through its interface, and never: no instantiation.
            if s.is_empty() && sites.is_empty() {
                continue;
            }
            sites.extend(s);
        }
        // A generic library function that the program never calls directly
        // makes no such closure (its methods are called with their type arguments too). Nor
        // does a generic function declared in the program that nothing calls directly or holds
        // as a value (`computed`'s implementation where only its overloads are called, and they
        // are not). Never a method or a closure: a method may be called through dispatch, and a
        // generic closure through its generic function type (the wrapper behind `relay#value`
        // where `const mk = relay; mk<Job>(…)`), which name no type arguments here.
        let library = cx.try_fn(f).is_some_and(|i| cx.scopes[i.module].is_library);
        // Only where `f` has type parameters of its own: a closure literal placed at several
        // sites (a struct field's default) may have its type from a generic one and `f` from
        // another, which names no type parameters.
        let free = matches!(&cx.defs[f.0 as usize], Some(Def::Fn(fd)) if fd.self_ty.is_none() && fd.generics > 0)
            && cx.try_fn(f).is_some_and(|i| i.kind == FnKind::Free);
        // A method no dispatched call can reach (an object type's, not a class's or an
        // implementation's) is called by name only, with its type arguments (#2860: the adapter
        // method behind a generic method of an object type, `AsyncEngine.read<T>`).
        // Nor one whose object type goes into `any` or is converted, where a dynamic call may
        // reach it with any type arguments.
        let by_name = matches!(&cx.defs[f.0 as usize], Some(Def::Fn(fd)) if fd.generics > 0
                && fd.self_ty.is_some_and(|t| !matches!(cx.ty.kind(t), TyKind::Adt(d, _) if g.dyn_calls.type_escaped(*d))))
            && cx.try_fn(f).is_some_and(|i| i.kind == FnKind::Method)
            && g.dispatched.binary_search_by_key(&f.0, |d| d.0).is_err();
        let free = free || by_name;
        if sites.is_empty() && (library || (free && !g.held.referenced.contains(&f))) {
            continue;
        }
        if sites.is_empty() || depth > 4 || out.len() + work.len() > 64 {
            out.push(t);
            continue;
        }
        for (caller, targs) in sites {
            let caller_owner = instance_owner(cx, g, caller);
            let s = cx.subst(t, &targs);
            work.push((caller_owner, s, depth + 1));
        }
    }
    out.sort_by_key(|t| t.0);
    out.dedup();
    out
}

/// The class of method `m`, with the number of its type parameters (the method's own come after).
pub(super) fn method_class(cx: &Ctx, m: DefId) -> Option<(DefId, u32)> {
    let Some(Def::Fn(f)) = &cx.defs[m.0 as usize] else {
        return None;
    };
    match cx.ty.kind(f.self_ty?) {
        TyKind::Adt(class, args) => Some((*class, args.len() as u32)),
        _ => None,
    }
}

/// Every instantiation of `class`, with the function it is made in: the type arguments of each
/// `new` (or literal) of it, and of each subclass's `extends` (`class NumQueue extends
/// AsyncQueue<number>`), followed through the subclass's own instantiations where that names the
/// subclass's type parameters. `None` where none is seen (its objects come from somewhere the graph
/// doesn't show) or the chain is too deep.
pub(super) fn class_insts(
    cx: &mut Ctx,
    g: &Graph,
    class: DefId,
    depth: u32,
) -> Option<Vec<(DefId, Vec<TyId>)>> {
    if depth > 8 {
        return None;
    }
    let mut out = g.news.get(&class).cloned().unwrap_or_default();
    for (sub, base) in types::direct_subclasses(cx, class) {
        let TyKind::Adt(_, bargs) = cx.ty.kind(base).clone() else {
            continue;
        };
        if bargs.iter().any(|t| types::mentions_param(cx, *t)) {
            for (caller, sargs) in class_insts(cx, g, sub, depth + 1)? {
                let args = bargs.iter().map(|t| cx.subst(*t, &sargs)).collect();
                out.push((caller, args));
            }
        } else {
            out.push((sub, bargs));
        }
    }
    (!out.is_empty()).then_some(out)
}
