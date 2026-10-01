//! Builtin `Comparable<T>` (std/prelude/compare.vlt). `extend` blocks can't list
//! `implements`, so an `extend X { compareTo(other: X): i64 }` block makes `X` implement the
//! prelude's `Comparable<X>` (an entry in `Program::impls`, generic when the block is). That
//! gives numbers, `string` and `bool` their ordering bound; classes and structs can also say
//! `implements Comparable<X>` as usual.

use crate::ctx::Ctx;
use crate::hir::ImplDef;
use crate::known::COMPARE_TO;

pub(super) fn extension_impls(cx: &mut Ctx) {
    let Some(iface) = cx.comparable_iface() else {
        return;
    };
    for i in 0..cx.extensions.len() {
        let ext = &cx.extensions[i];
        let Some(m) = ext.methods.get(COMPARE_TO).copied() else {
            continue;
        };
        let (target, n) = (ext.target, ext.generics.len());
        let f = cx.fn_info(m.def);
        let fits = !m.is_static
            && f.generics.len() == n
            && f.params.len() == 1
            && f.params[0].ty == target
            && f.ret == cx.ty.i64;
        let taken = cx.impls.iter().any(|x| x.iface == iface && x.ty == target);
        if !fits || taken {
            continue;
        }
        // Called through `Callee::ParamMethod`: the interface's borrow ABI.
        cx.fn_info_mut(m.def).fixed_modes = true;
        cx.impls.push(ImplDef {
            ty: target,
            generics: n as u32,
            iface,
            iface_args: vec![target],
            methods: vec![m.def],
        });
    }
}
