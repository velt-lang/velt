//! Prebuilt hook modules (#1728, #2196): an on-demand hook module whose `prebuilt` entries take
//! and return only numbers and strings is compiled once, with everything it reaches, into one
//! object of the build cache (`veltc` `driver/prebuilt`). Where a build finds that object, sema
//! checks a `hookCall` of such an entry as a call of the declared function [`symbol`]
//! (`velt_sema` `lazy_std/hook_stubs.rs`), as it checked the `velt_rt_*` extern the hook
//! replaced: the module is not loaded, checked, lowered or optimized, and the link adds the
//! object. Where it doesn't, the build loads the module as for any other hook.
//!
//! Only numbers and strings cross, so nothing a program decides (which types are counted, how a
//! `u8[]` is laid out, which parameters are `noalias`) reaches the object's code: its key names
//! the std sources it was compiled from, the compiler and the target, never a program.
//!
//! How each value crosses, the convention of the runtime's string externs (rt_abi.md "Strings"):
//! - a number type (`i8` … `u64`, `isize`, `usize`, `f32`, `f64`): by value, as that integer or
//!   float type;
//! - a `&string` parameter: the address of the caller's string value (24 bytes), borrowed for
//!   the call: the callee neither keeps nor releases it;
//! - a `string` result: through a trailing out-pointer to 24 bytes the caller provides, which
//!   the callee fills with a string the caller then owns (and releases);
//! - a `void` result: none.
//!
//! Anything else (`bool`, a string by value or moved, arrays, classes, type parameters, `async`,
//! `throws`) is refused when the table is compiled: [`scalar`].

use super::{Hook, HOOKS};
use crate::hook_sig::{Mode, Sig, T};

/// What every prebuilt hook's symbol starts with.
pub const PREFIX: &str = "velt_hook_";

/// The symbol of hook `h`'s entry point in its module's object: `velt_hook_<entry name>`.
pub fn symbol(h: &Hook) -> String {
    format!("{PREFIX}{}", h.name)
}

/// The index in [`HOOKS`] of the prebuilt hook whose entry point is `symbol`.
pub fn of_symbol(symbol: &str) -> Option<usize> {
    let name = symbol.strip_prefix(PREFIX)?;
    HOOKS.iter().position(|h| h.prebuilt && h.name == name)
}

/// The prebuilt hooks of the module of table entry `module_entry`, with their indices.
pub fn of_module(module_entry: &str) -> impl Iterator<Item = (usize, &'static Hook)> + '_ {
    (HOOKS.iter().enumerate()).filter(move |(_, h)| h.prebuilt && h.module_entry == module_entry)
}

/// Can a hook of signature `sig` be prebuilt: a function, neither generic, `async` nor
/// throwing, whose parameters are numbers (by value) and `&string`s, and whose result is a
/// number, a `string` or `void` (module docs)?
pub const fn scalar(sig: &Sig) -> bool {
    let Sig::Fn(f) = sig else {
        return false;
    };
    if f.generics != 0 || f.is_async || f.throws.is_some() {
        return false;
    }
    let mut i = 0;
    while i < f.params.len() {
        let p = &f.params[i];
        let ok = match p.mode {
            Mode::Value => number(&p.ty),
            Mode::Borrow => matches!(p.ty, T::Str),
            Mode::BorrowMut | Mode::Owned => false,
        };
        if !ok {
            return false;
        }
        i += 1;
    }
    number(&f.ret) || matches!(f.ret, T::Str | T::Void)
}

/// Is `t` a number type?
const fn number(t: &T) -> bool {
    matches!(
        t,
        T::I8
            | T::I16
            | T::I32
            | T::I64
            | T::ISize
            | T::U8
            | T::U16
            | T::U32
            | T::U64
            | T::USize
            | T::F32
            | T::F64
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook_sig::{FnSig, Param};

    fn sig(params: &[Param], ret: T) -> Sig {
        let params: &'static [Param] = Vec::leak(params.to_vec());
        Sig::Fn(FnSig {
            generics: 0,
            params,
            ret,
            throws: None,
            is_async: false,
        })
    }

    const fn value(ty: T) -> Param {
        Param {
            mode: Mode::Value,
            ty,
        }
    }

    const fn borrow(ty: T) -> Param {
        Param {
            mode: Mode::Borrow,
            ty,
        }
    }

    #[test]
    fn numbers_and_borrowed_strings_cross() {
        assert!(scalar(&sig(&[value(T::F64), value(T::I64)], T::Str)));
        assert!(scalar(&sig(
            &[borrow(T::Str), value(T::USize), value(T::USize)],
            T::F64
        )));
        assert!(scalar(&sig(&[], T::Void)));
        assert!(scalar(&sig(&[value(T::U8)], T::I32)));
    }

    /// Anything a program's facts could change the layout or ownership of is refused: the
    /// object would otherwise depend on the program that called it.
    #[test]
    fn anything_else_is_refused() {
        const BYTES: T = T::Array(&T::U8);
        let refused = [
            sig(&[value(T::Bool)], T::Str),
            sig(&[value(T::F64)], T::Bool),
            sig(&[value(T::Str)], T::Str),
            sig(
                &[Param {
                    mode: Mode::Owned,
                    ty: T::Str,
                }],
                T::Str,
            ),
            sig(
                &[Param {
                    mode: Mode::BorrowMut,
                    ty: T::Str,
                }],
                T::Void,
            ),
            sig(&[borrow(BYTES)], T::Str),
            sig(&[value(T::F64)], BYTES),
            sig(&[value(T::Param(0))], T::Str),
            sig(&[value(T::F64)], T::Never),
            sig(&[value(T::F64)], T::OrNull(&T::Str)),
            Sig::Class,
            Sig::Decorator,
            Sig::ProcessStatic,
        ];
        for s in refused {
            assert!(!scalar(&s), "{s}");
        }
        let generic = Sig::Fn(FnSig {
            generics: 1,
            params: &[],
            ret: T::Str,
            throws: None,
            is_async: false,
        });
        let throwing = Sig::Fn(FnSig {
            generics: 0,
            params: &[],
            ret: T::Str,
            throws: Some(T::Str),
            is_async: false,
        });
        let is_async = Sig::Fn(FnSig {
            generics: 0,
            params: &[],
            ret: T::Str,
            throws: None,
            is_async: true,
        });
        for s in [generic, throwing, is_async] {
            assert!(!scalar(&s), "{s}");
        }
    }

    /// Every prebuilt entry passes, its symbol finds it again, and its module is one that
    /// programs load only on demand (a module the prelude loads has nothing to save).
    #[test]
    fn the_table_s_prebuilt_entries() {
        for (i, h) in HOOKS.iter().enumerate().filter(|(_, h)| h.prebuilt) {
            assert!(scalar(&h.sig), "{}", h.name);
            assert_eq!(of_symbol(&symbol(h)), Some(i));
            assert!(
                super::super::ON_DEMAND_MODULES.contains(&h.module().as_str()),
                "{}: {} is not loaded on demand",
                h.name,
                h.module()
            );
        }
        assert_eq!(of_symbol("velt_hook_f64_to_string"), None);
    }
}
