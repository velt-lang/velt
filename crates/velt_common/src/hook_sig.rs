//! The signatures of the compiler's hooks (`std_hooks.rs`), as data: a small type language
//! written inside the `hooks!` table, like .NET's `metasig.h`, which the table's macros turn into
//! [`Sig`] constants. `velt_sema`'s test of the table walks each hook's HIR signature against its
//! [`Sig`], and `velt_vir` generates one typed call builder per lowered hook from the same tokens
//! (`velt_vir` `lower/hook_calls.rs`), so a hook's signature is written once.
//!
//! The language (every type is one token tree, or a name followed by one parenthesized group):
//!
//! | written | Velt type |
//! |---|---|
//! | `i8` … `u64`, `isize`, `usize`, `f32`, `f64`, `bool`, `string`, `void`, `never`, `symbol` | the same |
//! | `A`, `B`, `C`, `D` | the hook's type parameters, in order |
//! | `[T]` | `T[]` |
//! | `Promise(T, E)` | `Promise<T, E>` |
//! | `tuple(T, U)` | `[T, U]` |
//! | `or_null(T)`, `or_undefined(T)` | `T \| null`, `T \| undefined` |
//! | `union(T, U)` | the union `T \| U` (its members in the order sema lists them) |
//! | `Fn((T, U) -> R)`, `Fn((T) -> R throws E)` | `(T, U) => R`, `(T) => R throws E` |
//! | `FnRest((T, [U]) -> R)` | `(t: T, ...us: U[]) => R` |
//! | `Name`, `Name(T, U)` | the std class, interface or enum `Name<T, U>` (`Map`, `any` and `unknown` too) |
//! | `object("{ a?: i64 }")` | an object-literal type, by the name sema gives it (its text) |
//!
//! A signature is `sig(<A, B>(P, …) -> R throws E)`: the type parameters and `throws` are
//! optional. A parameter is written with its passing mode as sema infers it: `T` for a `Copy`
//! value, `&T` borrowed, `&mut T` borrowed mutably, `move T` owned (the callee drops it). Sema
//! infers modes and `throws` from a hook's body, so `velt_sema`'s test compares them on a program
//! checked with every std body (`velt check --std`): lazy std leaves the body of a hook no code
//! reaches unchecked. A class is `class`, and a compile-time decorator the compiler acts on
//! (`declare decorator function`, #2203) is `decorator`.
//!
//! ```
//! use velt_common::hook_sig::{Mode, Sig, T};
//! use velt_common::std_hooks::HOOKS;
//! let int_push = HOOKS.iter().find(|h| h.name == "int_push").unwrap();
//! let Sig::Fn(f) = int_push.sig else { panic!() };
//! assert_eq!(f.params[0].mode, Mode::BorrowMut);
//! assert_eq!(f.params[1].ty, T::I64);
//! assert_eq!(int_push.sig.to_string(), "(&mut string, i64) -> void");
//! ```
//!
//! The object-literal escape hatch: sema names an anonymous object type by its text (a
//! `Def::Adt` named `{ unknownKeys?: … }`), and its fields' literal-union types have no other
//! spelling here, so `object("…")` compares that name. One hook uses it
//! (`json_parse_from_context`).

use std::fmt;

/// A hook's signature: a function's, a class, or a compile-time decorator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sig {
    Fn(FnSig),
    Class,
    /// A `declare decorator function` the compiler acts on where it is used (`@inline`): its
    /// declaration says what it takes and where it applies, so the table gives no signature.
    Decorator,
    /// A process static (#2779): a module constant `new Atomic<u64>(0)` whose word the runtime
    /// reads and writes, given its address at start (`velt_rt_statics`).
    ProcessStatic,
}

/// A function hook's signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FnSig {
    /// Its number of type parameters (`A`, `B`, … are `T::Param(0)`, `T::Param(1)`, …).
    pub generics: u32,
    pub params: &'static [Param],
    pub ret: T,
    /// What it throws (`None`: nothing).
    pub throws: Option<T>,
}

/// One parameter: its passing mode and its type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Param {
    pub mode: Mode,
    pub ty: T,
}

/// A parameter's passing mode (`velt_sema` `hir::PassMode`), as the table writes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Written plainly: a `Copy` value.
    Value,
    /// `&T`.
    Borrow,
    /// `&mut T`.
    BorrowMut,
    /// `move T`.
    Owned,
}

/// A type of the hooks' type language (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum T {
    I8,
    I16,
    I32,
    I64,
    ISize,
    U8,
    U16,
    U32,
    U64,
    USize,
    F32,
    F64,
    Bool,
    Str,
    Void,
    Never,
    Symbol,
    /// Type parameter `n` (`A` is 0).
    Param(u32),
    Array(&'static T),
    Tuple(&'static [T]),
    OrNull(&'static T),
    OrUndefined(&'static T),
    Union(&'static [T]),
    Promise(&'static T, &'static T),
    Fn {
        params: &'static [T],
        ret: &'static T,
        throws: Option<&'static T>,
        /// The last parameter is a rest parameter.
        rest: bool,
    },
    /// A std class, interface or enum by its last name segment, with its type arguments; also an
    /// object-literal type by its text (`object("…")`).
    Named(&'static str, &'static [T]),
}

impl fmt::Display for T {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prim = match self {
            T::I8 => "i8",
            T::I16 => "i16",
            T::I32 => "i32",
            T::I64 => "i64",
            T::ISize => "isize",
            T::U8 => "u8",
            T::U16 => "u16",
            T::U32 => "u32",
            T::U64 => "u64",
            T::USize => "usize",
            T::F32 => "f32",
            T::F64 => "f64",
            T::Bool => "bool",
            T::Str => "string",
            T::Void => "void",
            T::Never => "never",
            T::Symbol => "symbol",
            _ => "",
        };
        if !prim.is_empty() {
            return f.write_str(prim);
        }
        match self {
            T::Param(i) => write!(f, "{}", char::from(b'A' + *i as u8)),
            T::Array(e) => write!(f, "[{e}]"),
            T::Tuple(xs) => write!(f, "tuple({})", List(xs)),
            T::OrNull(x) => write!(f, "or_null({x})"),
            T::OrUndefined(x) => write!(f, "or_undefined({x})"),
            T::Union(xs) => write!(f, "union({})", List(xs)),
            T::Promise(x, e) => write!(f, "Promise({x}, {e})"),
            T::Fn {
                params,
                ret,
                throws,
                rest,
            } => {
                let kind = if *rest { "FnRest" } else { "Fn" };
                write!(f, "{kind}(({}) -> {ret}", List(params))?;
                if let Some(e) = throws {
                    write!(f, " throws {e}")?;
                }
                f.write_str(")")
            }
            T::Named(n, args) if n.starts_with('{') => write!(f, "object({n:?})"),
            T::Named(n, []) => f.write_str(n),
            T::Named(n, args) => write!(f, "{n}({})", List(args)),
            _ => unreachable!("ICE: a primitive is printed above"),
        }
    }
}

/// Types separated by `, `.
struct List<'a>(&'a [T]);

impl fmt::Display for List<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, t) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{t}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Param {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mode = match self.mode {
            Mode::Value => "",
            Mode::Borrow => "&",
            Mode::BorrowMut => "&mut ",
            Mode::Owned => "move ",
        };
        write!(f, "{mode}{}", self.ty)
    }
}

impl fmt::Display for Sig {
    /// As the table writes it, without `sig(…)`: `<A>(&mut ArrayView(A), usize) -> void`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Sig::Fn(s) => s,
            Sig::Class => return f.write_str("class"),
            Sig::Decorator => return f.write_str("decorator"),
            Sig::ProcessStatic => return f.write_str("process_static"),
        };
        if s.generics > 0 {
            let names: Vec<String> = (0..s.generics).map(|i| T::Param(i).to_string()).collect();
            write!(f, "<{}>", names.join(", "))?;
        }
        f.write_str("(")?;
        for (i, p) in s.params.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{p}")?;
        }
        write!(f, ") -> {}", s.ret)?;
        if let Some(e) = s.throws {
            write!(f, " throws {e}")?;
        }
        Ok(())
    }
}

/// A [`Sig`] from the table's tokens: `class`, or `sig(<A>(&T, …) -> R throws E)`.
#[doc(hidden)]
#[macro_export]
macro_rules! hook_sig {
    (class) => {
        $crate::hook_sig::Sig::Class
    };
    (decorator) => {
        $crate::hook_sig::Sig::Decorator
    };
    (process_static) => {
        $crate::hook_sig::Sig::ProcessStatic
    };
    (sig(< $($g:ident),* > ( $($p:tt)* ) -> $r:tt $(($($ra:tt)*))? $(throws $e:tt $(($($ea:tt)*))?)?)) => {
        $crate::hook_sig::Sig::Fn($crate::hook_sig::FnSig {
            generics: <[&str]>::len(&[$(stringify!($g)),*]) as u32,
            params: $crate::hook_params!([] $($p)*),
            ret: $crate::hook_ty!($r $(($($ra)*))?),
            throws: $crate::hook_throws!($($e $(($($ea)*))?)?),
        })
    };
    (sig(( $($p:tt)* ) -> $r:tt $(($($ra:tt)*))? $(throws $e:tt $(($($ea:tt)*))?)?)) => {
        $crate::hook_sig!(sig(<>($($p)*) -> $r $(($($ra)*))? $(throws $e $(($($ea)*))?)?))
    };
}

/// `Some(type)` from the tokens after `throws`, `None` without them.
#[doc(hidden)]
#[macro_export]
macro_rules! hook_throws {
    () => {
        None
    };
    (@ref) => {
        None
    };
    (@ref $($t:tt)+) => {
        Some(&$crate::hook_ty!($($t)+))
    };
    ($($t:tt)+) => {
        Some($crate::hook_ty!($($t)+))
    };
}

/// The parameters of a signature, each with its mode (module docs).
#[doc(hidden)]
#[macro_export]
macro_rules! hook_params {
    ([$($acc:expr,)*]) => { &[$($acc),*] };
    ([$($acc:expr,)*] & mut $t:tt $(($($a:tt)*))? $(, $($rest:tt)*)?) => {
        $crate::hook_params!([$($acc,)* $crate::hook_param!(BorrowMut $t $(($($a)*))?),] $($($rest)*)?)
    };
    ([$($acc:expr,)*] & $t:tt $(($($a:tt)*))? $(, $($rest:tt)*)?) => {
        $crate::hook_params!([$($acc,)* $crate::hook_param!(Borrow $t $(($($a)*))?),] $($($rest)*)?)
    };
    ([$($acc:expr,)*] move $t:tt $(($($a:tt)*))? $(, $($rest:tt)*)?) => {
        $crate::hook_params!([$($acc,)* $crate::hook_param!(Owned $t $(($($a)*))?),] $($($rest)*)?)
    };
    ([$($acc:expr,)*] $t:tt $(($($a:tt)*))? $(, $($rest:tt)*)?) => {
        $crate::hook_params!([$($acc,)* $crate::hook_param!(Value $t $(($($a)*))?),] $($($rest)*)?)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! hook_param {
    ($mode:ident $($t:tt)+) => {
        $crate::hook_sig::Param {
            mode: $crate::hook_sig::Mode::$mode,
            ty: $crate::hook_ty!($($t)+),
        }
    };
}

/// A list of types: `&[T, …]`.
#[doc(hidden)]
#[macro_export]
macro_rules! hook_tys {
    ($($t:tt $(($($a:tt)*))?),* $(,)?) => { &[$($crate::hook_ty!($t $(($($a)*))?)),*] };
}

/// One type of the language (module docs).
#[doc(hidden)]
#[macro_export]
macro_rules! hook_ty {
    (i8) => { $crate::hook_sig::T::I8 };
    (i16) => { $crate::hook_sig::T::I16 };
    (i32) => { $crate::hook_sig::T::I32 };
    (i64) => { $crate::hook_sig::T::I64 };
    (isize) => { $crate::hook_sig::T::ISize };
    (u8) => { $crate::hook_sig::T::U8 };
    (u16) => { $crate::hook_sig::T::U16 };
    (u32) => { $crate::hook_sig::T::U32 };
    (u64) => { $crate::hook_sig::T::U64 };
    (usize) => { $crate::hook_sig::T::USize };
    (f32) => { $crate::hook_sig::T::F32 };
    (f64) => { $crate::hook_sig::T::F64 };
    (bool) => { $crate::hook_sig::T::Bool };
    (string) => { $crate::hook_sig::T::Str };
    (void) => { $crate::hook_sig::T::Void };
    (never) => { $crate::hook_sig::T::Never };
    (symbol) => { $crate::hook_sig::T::Symbol };
    (A) => { $crate::hook_sig::T::Param(0) };
    (B) => { $crate::hook_sig::T::Param(1) };
    (C) => { $crate::hook_sig::T::Param(2) };
    (D) => { $crate::hook_sig::T::Param(3) };
    ([ $($e:tt)+ ]) => { $crate::hook_sig::T::Array(&$crate::hook_ty!($($e)+)) };
    (Promise ( $($a:tt)* )) => { $crate::hook_ty!(@pair Promise $($a)*) };
    (@pair $k:ident $a:tt $(($($aa:tt)*))?, $b:tt $(($($ba:tt)*))?) => {
        $crate::hook_sig::T::$k(&$crate::hook_ty!($a $(($($aa)*))?), &$crate::hook_ty!($b $(($($ba)*))?))
    };
    (tuple ( $($x:tt)* )) => { $crate::hook_sig::T::Tuple($crate::hook_tys!($($x)*)) };
    (union ( $($x:tt)* )) => { $crate::hook_sig::T::Union($crate::hook_tys!($($x)*)) };
    (or_null ( $($x:tt)+ )) => { $crate::hook_sig::T::OrNull(&$crate::hook_ty!($($x)+)) };
    (or_undefined ( $($x:tt)+ )) => { $crate::hook_sig::T::OrUndefined(&$crate::hook_ty!($($x)+)) };
    (object ( $text:literal )) => { $crate::hook_sig::T::Named($text, &[]) };
    (Fn $f:tt) => { $crate::hook_ty!(@fn false $f) };
    (FnRest $f:tt) => { $crate::hook_ty!(@fn true $f) };
    (@fn $rest:literal (( $($p:tt)* ) -> $r:tt $(($($ra:tt)*))? $(throws $e:tt $(($($ea:tt)*))?)?)) => {
        $crate::hook_sig::T::Fn {
            params: $crate::hook_tys!($($p)*),
            ret: &$crate::hook_ty!($r $(($($ra)*))?),
            throws: $crate::hook_throws!(@ref $($e $(($($ea)*))?)?),
            rest: $rest,
        }
    };
    ($n:ident) => { $crate::hook_sig::T::Named(stringify!($n), &[]) };
    ($n:ident ( $($a:tt)* )) => { $crate::hook_sig::T::Named(stringify!($n), $crate::hook_tys!($($a)*)) };
}
