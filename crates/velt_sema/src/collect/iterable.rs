//! Builtin `Iterable<T, E>` and `AsyncIterable<T, E>` (std/prelude/iter.vlt). `extend` blocks
//! can't list `implements`, so an `extend X { [Symbol.iterator](): Iterator<T, E> { … } }`
//! block makes `X` implement the prelude's `Iterable<T, E>` (an entry in `Program::impls`,
//! generic when the block is), like `compareTo` makes it `Comparable` (`comparable.rs`), and
//! `[Symbol.asyncIterator](): AsyncIterator<T, E>` makes it an `AsyncIterable<T, E>`. The
//! prelude does this for arrays and `string`, so `[1, 2, 3]` converts to an `Iterable<i64>`
//! value and satisfies an `Iterable<T>` bound (`for...of` over them keeps its own loops,
//! `body/for_iter.rs`), and for the `IterableIterator<T, E>` interface values, which do not
//! convert to the interfaces they extend otherwise.

use velt_syntax::ast::{SYMBOL_ASYNC_ITERATOR, SYMBOL_ITERATOR};

use crate::ctx::Ctx;
use crate::hir::{ImplDef, TyKind};

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
            if let Some(imp) = extension_impl(cx, i, method, iterable, iterator) {
                cx.impls.push(imp);
            }
        }
    }
}

/// The impl of `iterable` that extension `i` provides with its `method`, if it does.
fn extension_impl(
    cx: &mut Ctx,
    i: usize,
    method: &str,
    iterable: crate::hir::DefId,
    iterator: crate::hir::DefId,
) -> Option<ImplDef> {
    let ext = &cx.extensions[i];
    let m = ext.methods.get(method).copied()?;
    let (target, n) = (ext.target, ext.generics.len());
    let f = cx.fn_info(m.def);
    let iface_args = match cx.ty.kind(f.ret) {
        TyKind::Dyn(d, args) if *d == iterator => args.clone(),
        _ => return None,
    };
    let fits = !m.is_static
        && f.generics.len() == n
        && f.params.is_empty()
        && f.declared_throws.is_none()
        && !f.is_generator;
    let taken = cx
        .impls
        .iter()
        .any(|x| x.iface == iterable && x.ty == target);
    if !fits || taken {
        return None;
    }
    // Called through `Callee::Dyn` / `Callee::ParamMethod`: the interface's borrow ABI.
    cx.fn_info_mut(m.def).fixed_modes = true;
    Some(ImplDef {
        ty: target,
        generics: n as u32,
        iface: iterable,
        iface_args,
        methods: vec![m.def],
    })
}
