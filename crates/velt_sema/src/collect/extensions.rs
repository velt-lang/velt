//! Phase 3 for `extend` blocks: each one's target, its methods' signatures and what it implements.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_syntax::ast;

use super::declare::{fn_placeholder, generic_names};
use super::shapes::resolve_bounds;
use super::sigs::{always_mutates, fill_sig, method_this, what};
use crate::ctx::Ctx;
use crate::defs::{static_key, DefInfo, Extension, FnKind, FnSource, MethodRef};
use crate::hir::TyKind;
use crate::resolve::TyEnv;

pub(super) fn extension<'m>(
    cx: &mut Ctx<'m>,
    m: usize,
    e: &'m ast::ExtendDecl,
    used: &mut FxHashSet<String>,
) {
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
    // A static named as an instance method (the two forms of an operator method,
    // `[Symbol.mul](k)` and `static [Symbol.mul](k, v)`) has a key of its own, as in a class.
    let instance: rustc_hash::FxHashSet<&str> = (e.methods.iter())
        .filter(|m| !m.is_static && !m.is_setter)
        .map(|m| m.decl.sig.name.name.as_str())
        .collect();
    for meth in &e.methods {
        let name = &meth.decl.sig.name;
        let key = match meth.is_static && instance.contains(&name.name.as_str()) {
            true => static_key(&name.name),
            false => cx.method_key(m, name, meth.is_setter),
        };
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
        // std's extension of the prelude `Record` (std/lang/record/symbols.velt) holds
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
