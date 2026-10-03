//! Builtin `Iterable<T, E>` and `AsyncIterable<T, E>` (std/prelude/iter.vlt). `extend` blocks
//! can't list `implements`, so an `extend X { [Symbol.iterator](): Iterator<T, E> { … } }`
//! block makes `X` implement the prelude's `Iterable<T, E>` (an entry in `Program::impls`,
//! generic when the block is), like `compareTo` makes it `Comparable` (`comparable.rs`), and
//! `[Symbol.asyncIterator](): AsyncIterator<T, E>` makes it an `AsyncIterable<T, E>`; so does a
//! generator method (`*[Symbol.iterator](): Generator<T, E>` gets an adapter returning the
//! generator as an `Iterator<T, E>`, `forwarders.rs` `Target::Generator`). The
//! prelude does this for arrays and `string`, so `[1, 2, 3]` converts to an `Iterable<i64>`
//! value and satisfies an `Iterable<T>` bound (`for...of` over them keeps its own loops,
//! `body/for_iter.rs`), and for the `IterableIterator<T, E>` interface values, which do not
//! convert to the interfaces they extend otherwise.

use velt_syntax::ast::{SYMBOL_ASYNC_ITERATOR, SYMBOL_ITERATOR};

use super::forwarders::{synth_method, Host, Target};
use crate::ctx::Ctx;
use crate::hir::{DefId, ImplDef, PassMode, TyId, TyKind};

/// (method, interface it implements, interface its result must be).
const PROTOCOLS: [(&str, &str, &str); 2] = [
    (SYMBOL_ITERATOR, "Iterable", "Iterator"),
    (SYMBOL_ASYNC_ITERATOR, "AsyncIterable", "AsyncIterator"),
];

pub(super) fn extension_impls(cx: &mut Ctx) {
    for (method, able, tor) in PROTOCOLS {
        let (Some(iterable), Some(iterator)) = (cx.prelude_iface(able), cx.prelude_iface(tor))
        else {
            continue;
        };
        for i in 0..cx.extensions.len() {
            if let Some(imp) = extension_impl(cx, i, (method, able), iterable, iterator) {
                cx.impls.push(imp);
            }
        }
    }
}

/// The impl of `iterable` that extension `i` provides with its `method`, if it does.
fn extension_impl(
    cx: &mut Ctx,
    i: usize,
    (method, able): (&str, &str),
    iterable: DefId,
    iterator: DefId,
) -> Option<ImplDef> {
    let ext = &cx.extensions[i];
    let m = ext.methods.get(method).copied()?;
    let (target, n) = (ext.target, ext.generics.len());
    let f = cx.fn_info(m.def);
    let (is_generator, declared) = (f.is_generator, f.declared_throws);
    let fits = !m.is_static
        && f.generics.len() == n
        && f.params.is_empty()
        && (declared.is_none() || is_generator);
    // A generator's signature keeps `E = never` in its result and a written `E` as its `throws`
    // (generator_sig.rs): the result as written.
    let ret = match is_generator {
        true => {
            let e = declared.and_then(|t| t.ty).unwrap_or(cx.ty.never);
            cx.with_generator_error(f.ret, e)
        }
        false => f.ret,
    };
    let (iface_args, gen) = match cx.ty.kind(ret) {
        TyKind::Dyn(d, args) if *d == iterator => (args.clone(), None),
        TyKind::Adt(_, args) if is_generator && generator_of(cx, ret, iterator) => {
            (args.clone(), Some(ret))
        }
        _ => return None,
    };
    let taken = cx
        .impls
        .iter()
        .any(|x| x.iface == iterable && x.ty == target);
    if !fits || taken {
        return None;
    }
    if is_generator && declared.is_none() {
        // Its error type is inferred later and must stay `never` (throws/checks.rs).
        cx.iface_generators
            .push((m.def, format!("{able}.{method}")));
    }
    let entry = match gen {
        // `Generator<T, E>` is an `Iterator<T, E>`: an adapter returns it as one.
        Some(gen) => {
            let ret = cx.ty.intern(TyKind::Dyn(iterator, iface_args.clone()));
            adapter(cx, i, m.def, (gen, ret), iterator)?
        }
        None => m.def,
    };
    // Called through `Callee::Dyn` / `Callee::ParamMethod`: the interface's borrow ABI.
    cx.fn_info_mut(entry).fixed_modes = true;
    Some(ImplDef {
        ty: target,
        generics: n as u32,
        iface: iterable,
        iface_args,
        methods: vec![entry],
    })
}

/// Is `t` the generator class of the protocol whose iterator is `iterator` (`Generator` for
/// `Iterator`, `AsyncGenerator` for `AsyncIterator`)?
fn generator_of(cx: &Ctx, t: TyId, iterator: DefId) -> bool {
    let want_async = Some(iterator) == cx.prelude_iface("AsyncIterator");
    matches!(cx.generator_result_kind(t), Some((_, _, a)) if a == want_async)
}

/// `X.<dyn [Symbol.iterator]>()` returning generator method `g`'s generator (type `gen`) as
/// the iterator interface value `ret` (forwarders.rs).
fn adapter(
    cx: &mut Ctx,
    i: usize,
    g: DefId,
    (gen, ret): (TyId, TyId),
    iterator: DefId,
) -> Option<DefId> {
    let (impl_index, _, _) = cx.find_impl(gen, iterator)?;
    let ext = &cx.extensions[i];
    let (target, generics) = (ext.target, ext.generics.clone());
    let f = cx.fn_info(g);
    let (span, module, name) = (f.name_span, f.module, f.name.clone());
    let mut_this = f
        .this
        .as_ref()
        .is_some_and(|t| t.mode == PassMode::BorrowMut);
    let targs = (0..generics.len() as u32).map(|k| cx.ty.param(k)).collect();
    let host = Host {
        d: None,
        generics,
        span,
        module,
    };
    let (prefix, key) = name.rsplit_once('.').unwrap_or(("", &name));
    let target_fn = Target::Generator {
        def: g,
        targs,
        gen,
        impl_index,
    };
    let name = format!("{prefix}.<dyn {key}>");
    Some(synth_method(
        cx,
        &host,
        target,
        name,
        vec![],
        ret,
        mut_this,
        target_fn,
    ))
}
