//! Typed calls of std's lowered hooks (velt_common `std_hooks.rs`), one builder per lowered
//! function hook, generated from the table's signatures ([`velt_common::std_hook_table!`]): the
//! signature is written once, there, and `velt_sema`'s test checks it against std.
//!
//! A builder takes each argument wrapped in a marker of the hook's parameter type ([`ty`]'s
//! markers, which the call site names: `ty::Str::arg(buf)`). What the Rust compiler checks, so
//! that a call site that gets one of them wrong does not build:
//! - the arity;
//! - the order and kind of the markers (`ty::I64` where the table has `i64`, `ty::Array<ty::Gen>`
//!   where it has `[A]`);
//! - [`Arg`] for a `Copy` or borrowed parameter and [`Own`] for an owned one;
//! - the number of type arguments (`[TyId; N]`);
//! - the result's marker (the `R` of [`Call<R>`] and [`Val<R>`]).
//!
//! What it does not check: that the operand inside a marker has that type. `ty::Str::arg(v)` is
//! the call site's assertion that `v` is a string's address, so `ty::I64::arg(s),
//! ty::Str::arg(n)` with the operands swapped compiles. A debug build of the compiler (the one
//! the golden tests run) checks it instead: [`Call::with`] asserts that each operand's VIR type
//! is the parameter's (`Ptr` for an aggregate, which is passed by its address).
//!
//! A builder finds the definition, applies the passing modes (an owned parameter is given a
//! moved value or a share of a borrowed one: [`Own`]) and returns a [`Call`], which emits the call
//! the way the site needs it ([`Call::emit`], [`Call::emit_to`], [`Call::emit_std`],
//! [`Call::emit_owned`]). A hook lowering cannot do without is an ICE when std lacks it; a
//! `fallback` hook's builder returns `None` when it is missing or unchecked, and the caller calls
//! the runtime instead. The builder of a hook whose entry lists `constructs(…)` takes the
//! [`Construct`] the call site lowers, and asserts (in release compilers too: one bit test) that
//! sema demanded the hook (`hir::Program::hooks_demanded`, velt_common `hook_demand.rs`): a
//! construct missing from the table is an ICE naming it. Builders are `#[inline]` and allocate
//! only the call's argument list.
//!
//! `velt_vir::hook_args`'s doctest shows that a call with the wrong markers does not compile.

use std::marker::PhantomData;

pub(super) use velt_common::std_hooks::Construct;
use velt_common::std_hooks::Mode;
use velt_sema::hir::{DefId, PassMode, TyId};

use super::{ice, unit, FnLower};
use crate::vir::{Callee, Operand, Place, Proj, Ty};

/// The marker types of the hooks' type language (velt_common `hook_sig.rs`): what a builder's
/// argument or result is. A std class is the marker of its name; type arguments are given to the
/// builder as `targs`, so `ArrayView<A>` is [`ty::ArrayView`] and `A` is [`ty::Gen`].
#[allow(dead_code)] // Type-level only: a marker is never a value.
pub mod ty {
    use std::marker::PhantomData;

    macro_rules! markers {
        ($($m:ident),* $(,)?) => { $(pub struct $m;)* };
    }
    markers!(I8, I16, I32, I64, ISize, U8, U16, U32, U64, USize, F32, F64, Bool, Str, Void, Never);
    markers!(Symbol, Any, Unknown, Gen, Tuple, Union, Object, Func);
    // The std classes in lowered hooks' signatures.
    markers!(
        ArrayView,
        BufferView,
        CString,
        ClassMarker,
        Error,
        HostRef,
        HostSettled,
        Ptr,
        Record
    );
    pub struct Array<E>(PhantomData<E>);
    pub struct OrNull<E>(PhantomData<E>);
    pub struct OrUndefined<E>(PhantomData<E>);
    pub struct Promise<V>(PhantomData<V>);
}

/// The marker type of a type of the language.
macro_rules! rty {
    (i8) => { ty::I8 };
    (i16) => { ty::I16 };
    (i32) => { ty::I32 };
    (i64) => { ty::I64 };
    (isize) => { ty::ISize };
    (u8) => { ty::U8 };
    (u16) => { ty::U16 };
    (u32) => { ty::U32 };
    (u64) => { ty::U64 };
    (usize) => { ty::USize };
    (f32) => { ty::F32 };
    (f64) => { ty::F64 };
    (bool) => { ty::Bool };
    (string) => { ty::Str };
    (void) => { ty::Void };
    (never) => { ty::Never };
    (symbol) => { ty::Symbol };
    (any) => { ty::Any };
    (unknown) => { ty::Unknown };
    (A) => { ty::Gen };
    (B) => { ty::Gen };
    (C) => { ty::Gen };
    (D) => { ty::Gen };
    ([ $($e:tt)+ ]) => { ty::Array<rty!($($e)+)> };
    (or_null ( $($x:tt)+ )) => { ty::OrNull<rty!($($x)+)> };
    (or_undefined ( $($x:tt)+ )) => { ty::OrUndefined<rty!($($x)+)> };
    (Promise ( $t:tt $(($($ta:tt)*))?, $($e:tt)* )) => { ty::Promise<rty!($t $(($($ta)*))?)> };
    (tuple $x:tt) => { ty::Tuple };
    (union $x:tt) => { ty::Union };
    (object $x:tt) => { ty::Object };
    (Fn $x:tt) => { ty::Func };
    (FnRest $x:tt) => { ty::Func };
    ($n:ident $(($($a:tt)*))?) => { ty::$n };
}

/// Names a call site uses for what it passes: `ty::Str::arg(buf)`, `ty::Any::shared(place)`.
pub trait HookTy: Sized {
    /// The argument as the call passes it: a scalar or a pointer by value, an aggregate (a
    /// string, an `any`) by its address.
    #[inline]
    fn arg(op: Operand) -> Arg<Self> {
        Arg(op, PhantomData)
    }
    /// For an owned parameter: a value the caller gives up, as [`Self::arg`] passes it.
    #[inline]
    fn moved(op: Operand) -> Own<Self> {
        Own(How::As(op), PhantomData)
    }
    /// For an owned parameter: a new reference to the value at `place`, which the caller keeps.
    #[inline]
    fn shared(place: Place) -> Own<Self> {
        Own(How::Share(place), PhantomData)
    }
}

impl<T> HookTy for T {}

/// An argument of type `T` for a `Copy` or borrowed parameter.
pub struct Arg<T>(Operand, PhantomData<fn() -> T>);

/// An argument of type `T` for an owned (`move`) parameter.
pub struct Own<T>(How, PhantomData<fn() -> T>);

pub(super) enum How {
    As(Operand),
    Share(Place),
}

/// One argument, with the mode the table gives its parameter.
pub(super) struct Passed(How, Mode);

impl<T> Arg<T> {
    #[inline]
    fn pass(self, mode: Mode) -> Passed {
        Passed(How::As(self.0), mode)
    }
}

impl<T> Own<T> {
    #[inline]
    fn pass(self, _: Mode) -> Passed {
        Passed(self.0, Mode::Owned)
    }
}

/// A hook call with its arguments ready, of result type `R`.
#[must_use = "a hook call does nothing until it is emitted"]
pub(super) struct Call<R> {
    def: DefId,
    targs: Vec<TyId>,
    argv: Vec<Operand>,
    _r: PhantomData<fn() -> R>,
}

/// Whether `op` has the VIR type a parameter of HIR type `param` (under `targs`) is passed as:
/// its own, `Ptr` for an aggregate (passed by address). An operand whose type the projections
/// do not determine passes.
fn operand_fits(lw: &mut FnLower<'_, '_>, op: &Operand, param: TyId, targs: &[TyId]) -> bool {
    let t = lw.cx.subst(param, targs);
    let want = match lw.cx.ty(t) {
        Ty::Agg(_) => Ty::Ptr,
        vt => vt,
    };
    let got = match op {
        Operand::Const(_, t) => *t,
        Operand::Copy(pl) => {
            let mut t = lw.locals[pl.local.0 as usize].ty;
            for pr in &pl.proj {
                t = match (pr, t) {
                    (Proj::Field(n), Ty::Agg(a)) => {
                        match lw
                            .cx
                            .aggs
                            .get(a.0 as usize)
                            .and_then(|l| l.fields.get(*n as usize))
                        {
                            Some(f) => f.0,
                            None => return true,
                        }
                    }
                    (Proj::Deref(to), _) => *to,
                    (Proj::Cast(a), _) => Ty::Agg(*a),
                    _ => return true,
                };
            }
            t
        }
    };
    got == want
}

/// A hook call's result, of type `R`.
pub(super) struct Val<R>(Operand, PhantomData<fn() -> R>);

impl<R> Val<R> {
    #[inline]
    pub(super) fn op(self) -> Operand {
        self.0
    }

    /// The temporary holding the result.
    pub(super) fn place(self) -> Place {
        match self.0 {
            Operand::Copy(p) => p,
            _ => ice("a hook's result is a temporary"),
        }
    }
}

impl<R> Call<R> {
    /// The call of hook `name` (a fallback hook): `None` when std lacks it or sema left it
    /// unchecked.
    #[inline]
    fn find<const N: usize>(
        lw: &mut FnLower<'_, '_>,
        name: &str,
        targs: Vec<TyId>,
        args: [Passed; N],
    ) -> Option<Self> {
        let def = (lw.cx.prelude_def(name)).filter(|d| !lw.cx.hir.unchecked.contains(d))?;
        Some(Self::with(lw, def, targs, args))
    }

    /// The call of hook `name`, which std must define.
    #[inline]
    fn must<const N: usize>(
        lw: &mut FnLower<'_, '_>,
        name: &str,
        targs: Vec<TyId>,
        args: [Passed; N],
    ) -> Self {
        let def = (lw.cx.prelude_def(name))
            .unwrap_or_else(|| ice(format_args!("std without the hook `{name}`")));
        Self::with(lw, def, targs, args)
    }

    /// The call of hook `name` (entry `index` of the table) for construct `c`: the entry must
    /// list `c` and sema must have demanded it, which a release compiler checks too (a construct
    /// missing from the table).
    #[inline]
    fn demanded<const N: usize>(
        lw: &mut FnLower<'_, '_>,
        index: usize,
        c: Construct,
        name: &str,
        targs: Vec<TyId>,
        args: [Passed; N],
    ) -> Self {
        let hook = &velt_common::std_hooks::HOOKS[index];
        assert!(
            hook.constructs.contains(&c),
            "ICE: hook {} called for a construct its table entry doesn't list (construct {})",
            hook.name,
            c.name()
        );
        assert!(
            lw.cx.hir.hooks_demanded.contains(index),
            "ICE: hook {} reached by lowering but not demanded by sema (construct {})",
            hook.name,
            c.name()
        );
        Self::must(lw, name, targs, args)
    }

    fn with<const N: usize>(
        lw: &mut FnLower<'_, '_>,
        def: DefId,
        targs: Vec<TyId>,
        args: [Passed; N],
    ) -> Self {
        let params = &lw.cx.fn_def(def).params;
        // The table and std are checked against each other by velt_sema's `std_hooks` test;
        // these checks keep a drift from building a wrong call in a release compiler too.
        assert_eq!(
            params.len(),
            N,
            "ICE: hook arity differs from its table entry"
        );
        let mut argv = Vec::with_capacity(N + 1);
        for (Passed(how, mode), p) in args.into_iter().zip(params) {
            assert!(
                matches!(
                    (mode, p.mode),
                    (Mode::Value, PassMode::Copy)
                        | (Mode::Borrow, PassMode::Borrow)
                        | (Mode::BorrowMut, PassMode::BorrowMut)
                        | (Mode::Owned, PassMode::Owned)
                ),
                "ICE: a hook's passing mode differs from its table entry"
            );
            argv.push(match how {
                How::As(op) => {
                    debug_assert!(
                        operand_fits(lw, &op, p.ty, &targs),
                        "ICE: an argument of `{}` is not of its parameter's type (the marker \
                         and the operand of a hook call disagree)",
                        lw.cx.fn_def(def).name
                    );
                    op
                }
                How::Share(place) => {
                    let t = lw.cx.subst(p.ty, &targs);
                    let v = lw.share_value(Operand::Copy(place), t);
                    match lw.cx.ty(t) {
                        vt @ Ty::Agg(_) => {
                            let tmp = lw.copy_to_temp(v, vt);
                            lw.addr(Place::local(tmp))
                        }
                        _ => v,
                    }
                }
            });
        }
        Call {
            def,
            targs,
            argv,
            _r: PhantomData,
        }
    }

    /// The result type and its VIR form, for a call that cannot throw.
    fn ret(&self, lw: &mut FnLower<'_, '_>) -> (TyId, Ty) {
        let fd = lw.cx.fn_def(self.def);
        let (ret, throws) = lw.cx.call_sig(fd);
        if throws.is_some() {
            ice("a throwing hook emitted without its errors (`Call::emit_std`)");
        }
        let ret = lw.cx.subst(ret, &self.targs);
        let vt = lw.cx.ty(ret);
        (ret, vt)
    }

    /// The call, its result (if any) in a fresh temporary: an aggregate through the trailing
    /// out-pointer.
    #[inline]
    pub(super) fn emit(self, lw: &mut FnLower<'_, '_>) -> Val<R> {
        let (_, vt) = self.ret(lw);
        let f = lw.cx.func_for_direct(self.def, self.targs, lw.mode);
        let mut argv = self.argv;
        let res = match vt {
            Ty::Unit => {
                lw.call(Callee::Func(f), argv, None, false);
                return Val(unit(), PhantomData);
            }
            Ty::Agg(_) => {
                let out = lw.temp(vt);
                let a = lw.addr(Place::local(out));
                argv.push(a);
                lw.call(Callee::Func(f), argv, None, false);
                out
            }
            _ => {
                let out = lw.temp(vt);
                lw.call(Callee::Func(f), argv, Some(Place::local(out)), false);
                out
            }
        };
        Val(Operand::Copy(Place::local(res)), PhantomData)
    }

    /// The call, its result written to `dest` (an aggregate through the out-pointer).
    #[inline]
    pub(super) fn emit_to(self, lw: &mut FnLower<'_, '_>, dest: Place) {
        let (_, vt) = self.ret(lw);
        let f = lw.cx.func_for_direct(self.def, self.targs, lw.mode);
        let mut argv = self.argv;
        match vt {
            Ty::Unit => ice("a hook without a result written to a place"),
            Ty::Agg(_) => {
                let a = lw.addr(dest);
                argv.push(a);
                lw.call(Callee::Func(f), argv, None, false);
            }
            _ => lw.call(Callee::Func(f), argv, Some(dest), false),
        }
    }

    /// The call as glue calls std (`call_std_hook`): its owned result, which the caller moves
    /// on; an error it throws is uncaught.
    #[inline]
    pub(super) fn emit_std(self, lw: &mut FnLower<'_, '_>) -> Operand {
        lw.call_std_hook(self.def, self.targs, self.argv)
    }

    /// The call as a user call (`finish_call`): its owned result, registered for drop.
    #[inline]
    pub(super) fn emit_owned(self, lw: &mut FnLower<'_, '_>) -> Operand {
        let (ret, _) = self.ret(lw);
        let f = lw.cx.func_for(self.def, self.targs);
        lw.finish_call(Callee::Func(f), self.argv, ret, None)
    }
}

/// The builders, from the table's entries (`std_hook_table!`): one per lowered function hook.
macro_rules! builders {
    // Each entry: which flags it has (lowered, fallback, constructs), then the builder if it is
    // lowered and a function.
    (@flags $name:ident $s:tt [$l:ident $f:ident $c:ident] []) => {
        builders!(@emit $l $f $c $name $s);
    };
    (@flags $name:ident $s:tt [$l:ident $f:ident $c:ident] [lowered $($r:ident)*]) => {
        builders!(@flags $name $s [yes $f $c] [$($r)*]);
    };
    (@flags $name:ident $s:tt [$l:ident $f:ident $c:ident] [fallback $($r:ident)*]) => {
        builders!(@flags $name $s [$l yes $c] [$($r)*]);
    };
    (@flags $name:ident $s:tt [$l:ident $f:ident $c:ident] [constructs $($r:ident)*]) => {
        builders!(@flags $name $s [$l $f yes] [$($r)*]);
    };
    (@flags $name:ident $s:tt [$l:ident $f:ident $c:ident] [$skip:ident $($r:ident)*]) => {
        builders!(@flags $name $s [$l $f $c] [$($r)*]);
    };
    (@emit no $f:ident $c:ident $name:ident $s:tt) => {};
    (@emit yes $f:ident $c:ident $name:ident [class]) => {};
    (@emit yes $f:ident $c:ident $name:ident [sig(<$g0:ident> $($sig:tt)*)]) => {
        builders!(@sig [$f $c] $name [targs: [TyId; 1],] [targs.to_vec()] $($sig)*);
    };
    (@emit yes $f:ident $c:ident $name:ident [sig(<$g0:ident, $g1:ident> $($sig:tt)*)]) => {
        builders!(@sig [$f $c] $name [targs: [TyId; 2],] [targs.to_vec()] $($sig)*);
    };
    (@emit yes $f:ident $c:ident $name:ident [sig($($sig:tt)*)]) => {
        builders!(@sig [$f $c] $name [] [Vec::new()] $($sig)*);
    };
    (@sig $f:tt $name:ident $decl:tt $targs:tt ( $($p:tt)* ) -> $r:tt
        $(($($ra:tt)*))? $(throws $($e:tt)*)?) => {
        builders!(@params $f $name [$r $(($($ra)*))?] $targs $decl [] []
            [a0 a1 a2 a3 a4 a5 a6 a7] $($p)*);
    };
    // The parameters, each named from the list and typed by its mode.
    (@params $f:tt $name:ident $ret:tt $targs:tt [$($decl:tt)*] [$($pass:tt)*] [$($tys:ty,)*]
        [$($names:ident)*]) => {
        builders!(@fn $f $name $ret $targs [$($decl)*] [$($pass)*]);
        #[doc = concat!("The arguments of [`", stringify!($name), "`](super::", stringify!($name),
            ") after its type arguments.")]
        pub mod $name {
            #[allow(unused_imports)] // What the hook's parameters need.
            use super::super::{ty, Arg, Own};
            pub type Args = ($($tys,)*);
        }
    };
    (@params $f:tt $name:ident $ret:tt $targs:tt [$($decl:tt)*] [$($pass:tt)*] [$($tys:tt)*]
        [$n:ident $($names:ident)*] & mut $t:tt $(($($a:tt)*))? $(, $($rest:tt)*)?) => {
        builders!(@params $f $name $ret $targs [$($decl)* $n: Arg<rty!($t $(($($a)*))?)>,]
            [$($pass)* $n.pass(Mode::BorrowMut),] [$($tys)* Arg<rty!($t $(($($a)*))?)>,]
            [$($names)*] $($($rest)*)?);
    };
    (@params $f:tt $name:ident $ret:tt $targs:tt [$($decl:tt)*] [$($pass:tt)*] [$($tys:tt)*]
        [$n:ident $($names:ident)*] & $t:tt $(($($a:tt)*))? $(, $($rest:tt)*)?) => {
        builders!(@params $f $name $ret $targs [$($decl)* $n: Arg<rty!($t $(($($a)*))?)>,]
            [$($pass)* $n.pass(Mode::Borrow),] [$($tys)* Arg<rty!($t $(($($a)*))?)>,]
            [$($names)*] $($($rest)*)?);
    };
    (@params $f:tt $name:ident $ret:tt $targs:tt [$($decl:tt)*] [$($pass:tt)*] [$($tys:tt)*]
        [$n:ident $($names:ident)*] move $t:tt $(($($a:tt)*))? $(, $($rest:tt)*)?) => {
        builders!(@params $f $name $ret $targs [$($decl)* $n: Own<rty!($t $(($($a)*))?)>,]
            [$($pass)* $n.pass(Mode::Owned),] [$($tys)* Own<rty!($t $(($($a)*))?)>,]
            [$($names)*] $($($rest)*)?);
    };
    (@params $f:tt $name:ident $ret:tt $targs:tt [$($decl:tt)*] [$($pass:tt)*] [$($tys:tt)*]
        [$n:ident $($names:ident)*] $t:tt $(($($a:tt)*))? $(, $($rest:tt)*)?) => {
        builders!(@params $f $name $ret $targs [$($decl)* $n: Arg<rty!($t $(($($a)*))?)>,]
            [$($pass)* $n.pass(Mode::Value),] [$($tys)* Arg<rty!($t $(($($a)*))?)>,]
            [$($names)*] $($($rest)*)?);
    };
    (@fn [yes no] $name:ident [$($ret:tt)*] [$targs:expr] [$($decl:tt)*] [$($pass:tt)*]) => {
        #[doc = concat!("A call of the fallback hook `", stringify!($name), "`, if std has it checked.")]
        #[inline]
        pub(in crate::lower) fn $name(lw: &mut FnLower<'_, '_>, $($decl)*)
            -> Option<Call<rty!($($ret)*)>> {
            Call::find(lw, velt_common::std_hook!($name), $targs, [$($pass)*])
        }
    };
    (@fn [no no] $name:ident [$($ret:tt)*] [$targs:expr] [$($decl:tt)*] [$($pass:tt)*]) => {
        #[doc = concat!("A call of the hook `", stringify!($name), "`.")]
        #[inline]
        pub(in crate::lower) fn $name(lw: &mut FnLower<'_, '_>, $($decl)*) -> Call<rty!($($ret)*)> {
            Call::must(lw, velt_common::std_hook!($name), $targs, [$($pass)*])
        }
    };
    (@fn [no yes] $name:ident [$($ret:tt)*] [$targs:expr] [$($decl:tt)*] [$($pass:tt)*]) => {
        #[doc = concat!("A call of the hook `", stringify!($name), "` for construct `c`, which \
            sema must have demanded (velt_common `hook_demand.rs`).")]
        #[inline]
        pub(in crate::lower) fn $name(lw: &mut FnLower<'_, '_>, c: Construct, $($decl)*)
            -> Call<rty!($($ret)*)> {
            Call::demanded(lw, velt_common::std_hooks::hook_index::$name, c,
                velt_common::std_hook!($name), $targs, [$($pass)*])
        }
    };
    ($($name:ident => $item:literal, $kind:ident $(($($sig:tt)*))? [$($flag:ident)*];)*) => {
        $(builders!(@flags $name [$kind $(($($sig)*))?] [no no no] [$($flag)*]);)*
    };
}

// Builders lowering does not call yet (hooks it reaches by name: track_caller.rs, numrep) are
// generated too, so each lowered hook has one. Next to each builder, a module of its name holds
// its argument types (`args::int_push::Args`), public for the doctest of `velt_vir::hook_args`.
#[allow(dead_code, non_snake_case)]
pub mod args {
    use super::{ty, Arg, Call, Construct, FnLower, Mode, Own, TyId};
    velt_common::std_hook_table!(builders);
}

pub(super) use args::*;
