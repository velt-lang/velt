//! Typed calls of std's hooks (velt_common `std_hooks.rs`) in the code sema writes, one builder
//! per function hook, generated from the table's signatures ([`velt_common::std_hook_table!`]),
//! as `velt_vir`'s `lower/hook_calls.rs` does for lowering: the signature is written once, in the
//! table, and `velt_sema`'s test `std_hooks.rs` checks it against std.
//!
//! A builder takes each argument as the hook's parameter type ([`ty`]'s markers, which the call
//! site names: `ty::Any::of(v)`); a generic hook takes its type arguments first. What that
//! checks, and what it does not:
//!
//! - **At compile time:** the arity, the order and kind of the markers and, for HIR, the number
//!   of type arguments (`[TyId; N]`). A swap of two markers or a missing argument does not
//!   compile (`velt_sema::hook_args`'s doctests).
//! - **Not by the types:** the expression inside a marker (`ty::Any::of(site)` compiles whatever
//!   `site` is), the source form's type-argument count (`Vec<ast::TypeExpr>`), and the result
//!   type given to [`Call::emit`] / [`Call::plain`] (the `R` of [`Call`] does not constrain it).
//! - **Source arguments** are checked by sema as the call they become, so a wrong expression
//!   there is a diagnostic.
//! - **HIR arguments** are checked when the call is built: [`Call::emit`] and [`Call::plain`]
//!   `debug_assert!` that each argument's type, and the result type they are given, are the
//!   hook's under the type arguments (`fits`), as velt_vir's `operand_fits` does for lowering.
//!   A parameter or result whose type mentions a type parameter other than as the whole type
//!   (`A[]`, `Promise<A>`) is not compared, and an argument of type `never` fits anything.
//!
//! The arguments are source to check (`ast::Expr`) or checked HIR (`hir::Expr`), the same for all of a call ([`Form`]): checked
//! HIR carries each argument's use (a borrow, a move) itself, so the table's passing modes are
//! not applied here. The builder returns a [`Call`], which the site emits as it needs:
//!
//! - source: [`Call::ast`] (part of a larger expression), [`Call::check`], [`Call::check_as`]
//!   (with the "needs the prelude" error when std lacks the hook);
//! - HIR: [`Call::emit`] (its errors tracked as any call's) and [`Call::plain`] (no throw source;
//!   `None` when std lacks the hook, which each site handles as it did before).
//!
//! Builders are `#[inline]` and allocate only the call's argument list.

use std::marker::PhantomData;

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::ctx::Ctx;
use crate::hir::{self, Callee, DefId, ExprKind as H, TyId, TyKind};

/// The marker types of the hooks' type language (velt_common `hook_sig.rs`): what a builder's
/// argument or result is. A std class is the marker of its name, without its type arguments;
/// a type parameter is [`ty::Gen`].
#[allow(dead_code)] // Type-level only: a marker is never a value.
pub mod ty {
    use std::marker::PhantomData;

    macro_rules! markers {
        ($($m:ident),* $(,)?) => { $(pub struct $m;)* };
    }
    markers!(I8, I16, I32, I64, ISize, U8, U16, U32, U64, USize, F32, F64, Bool, Str, Void, Never);
    markers!(Symbol, Any, Unknown, Gen, Tuple, Union, Object, Func);
    // The std classes in the hooks' signatures.
    markers!(
        ArrayView,
        TupleElements,
        BufferView,
        CString,
        ClassMarker,
        Error,
        HostRef,
        HostSettled,
        Map,
        ObjectPrototype,
        Ptr,
        Record,
        RegExp,
        RegExpMatch,
        Set
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

/// What a call's arguments are: source sema checks as the call ([`ast::Expr`]), or checked HIR
/// ([`hir::Expr`]).
pub trait Form: Sized {
    /// A generic hook's `N` type arguments: as the source writes them, which checking counts
    /// (none: inferred), or the types themselves.
    type TyArgs<const N: usize>;
    /// The type arguments as the call holds them.
    #[doc(hidden)]
    type TyArgList: Default;
    #[doc(hidden)]
    fn ty_args<const N: usize>(t: Self::TyArgs<N>) -> Self::TyArgList;
}

impl Form for ast::Expr {
    type TyArgs<const N: usize> = Vec<ast::TypeExpr>;
    type TyArgList = Vec<ast::TypeExpr>;
    #[inline]
    fn ty_args<const N: usize>(t: Vec<ast::TypeExpr>) -> Vec<ast::TypeExpr> {
        t
    }
}

impl Form for hir::Expr {
    type TyArgs<const N: usize> = [TyId; N];
    type TyArgList = Vec<TyId>;
    #[inline]
    fn ty_args<const N: usize>(t: [TyId; N]) -> Vec<TyId> {
        t.to_vec()
    }
}

/// Names a call site uses for what it passes: `ty::Str::of(e)`.
pub trait HookTy: Sized {
    /// `e` as an argument of type `Self`.
    #[inline]
    fn of<E: Form>(e: E) -> Arg<Self, E> {
        Arg(e, PhantomData)
    }
}

impl<T> HookTy for T {}

/// An argument of type `T`, source or HIR (`E`).
pub struct Arg<T, E>(E, PhantomData<fn() -> T>);

/// A hook call with its arguments ready, of result type `R`.
#[must_use = "a hook call does nothing until it is emitted"]
pub(crate) struct Call<R, E: Form> {
    name: &'static str,
    targs: E::TyArgList,
    args: Vec<E>,
    _r: PhantomData<fn() -> R>,
}

impl<R, E: Form> Call<R, E> {
    #[inline]
    fn new<const N: usize>(name: &'static str, targs: E::TyArgList, args: [E; N]) -> Self {
        Call {
            name,
            targs,
            args: Vec::from(args),
            _r: PhantomData,
        }
    }

    /// The last argument: a text helper's call site, which [`FnCx::text_helper_call`] fills.
    #[inline]
    pub(crate) fn site_mut(&mut self) -> &mut E {
        self.args
            .last_mut()
            .unwrap_or_else(|| panic!("ICE: `{}` takes no call site", self.name))
    }
}

/// `e` as a synthesized source expression at `span`.
fn synth(kind: ast::ExprKind, span: Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

impl<R> Call<R, ast::Expr> {
    /// The call as source, `hook(args)` at `span`, for a larger expression checked as a whole.
    #[inline]
    pub(crate) fn ast(self, span: Span) -> ast::Expr {
        let name = self.name.to_string();
        let callee = synth(ast::ExprKind::Ident(ast::Ident { name, span }), span);
        let kind = ast::ExprKind::Call {
            callee: Box::new(callee),
            type_args: self.targs,
            args: self.args,
            optional: false,
        };
        synth(kind, span)
    }

    /// The call, checked (expecting no type).
    #[inline]
    pub(crate) fn check(self, cx: &mut FnCx<'_, '_>, span: Span) -> hir::Expr {
        self.check_exp(cx, None, span)
    }

    /// The call, checked expecting `exp`.
    #[inline]
    pub(crate) fn check_exp(
        self,
        cx: &mut FnCx<'_, '_>,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let call = self.ast(span);
        cx.expr(&call, exp, Want::Move)
    }

    /// The call of a non-generic hook standing for the source construct `what`, checked against
    /// the hook's definition; without the hook (no prelude), the error that `what` needs it.
    #[inline]
    pub(crate) fn check_as(
        self,
        cx: &mut FnCx<'_, '_>,
        what: &str,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        debug_assert!(self.targs.is_empty(), "ICE: `check_as` of a generic hook");
        cx.prelude_call(self.name, what, &[], &self.args, exp, span)
    }
}

/// Whether a value of type `got` is what a hook's parameter or result of declared type `decl`
/// is under `targs`: `decl` itself, or the type argument when `decl` is a type parameter. A
/// `decl` that mentions a type parameter deeper (`A[]`) is not compared (comparing it would
/// intern the substituted type, which a debug check must not do); `never` (an argument that
/// does not return) fits anything, and so does any dynamic type (`unknown`, `object`,
/// `Function`) where the hook takes an `any`: they are one representation, and sema passes
/// such a value to the hook as it is (`Array.isArray` of an `unknown`).
fn fits(cx: &Ctx<'_>, decl: TyId, got: TyId, targs: &[TyId]) -> bool {
    if got == decl || got == cx.ty.never {
        return true;
    }
    if cx.dynamic(decl) == Some(crate::any::Dynamic::Any) && cx.dynamic(got).is_some() {
        return true;
    }
    if let TyKind::Param(i) = cx.ty.kind(decl) {
        return targs.get(*i as usize).is_none_or(|t| *t == got);
    }
    let mut generic = false;
    cx.ty
        .visit(decl, &mut |k| generic |= matches!(k, TyKind::Param(_)));
    generic
}

/// `debug_assert!`s that a HIR call of hook `d` passes arguments and expects a result of the
/// hook's types ([`fits`]): a marker that holds the wrong expression is an ICE, not a wrong call.
fn debug_check(cx: &Ctx<'_>, d: DefId, name: &str, targs: &[TyId], args: &[hir::Expr], ret: TyId) {
    if !cfg!(debug_assertions) {
        return;
    }
    let f = cx.fn_info(d);
    debug_assert_eq!(
        f.params.len(),
        args.len(),
        "ICE: `{name}` called with {} arguments, the hook takes {}",
        args.len(),
        f.params.len()
    );
    for (i, (p, a)) in f.params.iter().zip(args).enumerate() {
        debug_assert!(
            fits(cx, p.ty, a.ty, targs),
            "ICE: argument {i} of `{name}` is not of its parameter's type (the marker and the \
             expression of a hook call disagree)"
        );
    }
    debug_assert!(
        fits(cx, f.ret, ret, targs),
        "ICE: `{name}` emitted with a result type that is not the hook's"
    );
}

impl<R> Call<R, hir::Expr> {
    /// The call, of type `ret`; it throws what the hook throws, as any call does. std without
    /// the hook is an ICE.
    #[inline]
    pub(crate) fn emit(self, cx: &mut FnCx<'_, '_>, ret: TyId, span: Span) -> hir::Expr {
        let d = (cx.cx.prelude_fn(self.name))
            .unwrap_or_else(|| panic!("ICE: the prelude without `{}`", self.name));
        debug_check(cx.cx, d, self.name, &self.targs, &self.args, ret);
        cx.call_throws(d, &self.targs, ret, span);
        let kind = H::Call {
            callee: Callee::Def(d, self.targs),
            args: self.args,
        };
        cx.mk(kind, ret, span)
    }

    /// The call, of type `ret`, with no throw source (a hook that cannot throw, or code checked
    /// before throws are inferred); `None` when std lacks the hook.
    #[inline]
    pub(crate) fn plain(self, cx: &Ctx<'_>, ret: TyId, span: Span) -> Option<hir::Expr> {
        let d = cx.prelude_fn(self.name)?;
        debug_check(cx, d, self.name, &self.targs, &self.args, ret);
        let kind = H::Call {
            callee: Callee::Def(d, self.targs),
            args: self.args,
        };
        Some(hir::Expr {
            kind,
            ty: ret,
            span,
        })
    }
}

/// The builders, from the table's entries (`std_hook_table!`): one per function hook.
macro_rules! builders {
    (@emit $name:ident [class]) => {};
    (@emit $name:ident [decorator]) => {};
    (@emit $name:ident [sig(<$g0:ident> $($sig:tt)*)]) => {
        builders!(@sig $name [targs: E::TyArgs<1>,] [E::ty_args(targs)] $($sig)*);
    };
    (@emit $name:ident [sig(<$g0:ident, $g1:ident> $($sig:tt)*)]) => {
        builders!(@sig $name [targs: E::TyArgs<2>,] [E::ty_args(targs)] $($sig)*);
    };
    (@emit $name:ident [sig($($sig:tt)*)]) => {
        builders!(@sig $name [] [Default::default()] $($sig)*);
    };
    (@sig $name:ident $decl:tt $targs:tt ( $($p:tt)* ) -> $r:tt
        $(($($ra:tt)*))? $(throws $($e:tt)*)?) => {
        builders!(@params $name [$r $(($($ra)*))?] $targs $decl [] [] [a0 a1 a2 a3 a4 a5 a6 a7]
            $($p)*);
    };
    // The parameters, each named from the list; HIR carries its passing mode itself.
    (@params $name:ident $ret:tt $targs:tt [$($decl:tt)*] [$($pass:tt)*] [$($tys:ty,)*]
        [$($names:ident)*]) => {
        builders!(@fn $name $ret $targs [$($decl)*] [$($pass)*]);
        #[doc = concat!("The arguments of [`", stringify!($name), "`](super::", stringify!($name),
            ") after its type arguments, as HIR.")]
        pub mod $name {
            #[allow(unused_imports)] // What the hook's parameters need.
            use super::super::{ty, Arg};
            pub type Args = ($($tys,)*);
        }
    };
    (@params $name:ident $ret:tt $targs:tt $decl:tt $pass:tt $tys:tt $names:tt
        & mut $($rest:tt)*) => {
        builders!(@params $name $ret $targs $decl $pass $tys $names $($rest)*);
    };
    (@params $name:ident $ret:tt $targs:tt $decl:tt $pass:tt $tys:tt $names:tt
        & $($rest:tt)*) => {
        builders!(@params $name $ret $targs $decl $pass $tys $names $($rest)*);
    };
    (@params $name:ident $ret:tt $targs:tt $decl:tt $pass:tt $tys:tt $names:tt
        move $($rest:tt)*) => {
        builders!(@params $name $ret $targs $decl $pass $tys $names $($rest)*);
    };
    (@params $name:ident $ret:tt $targs:tt [$($decl:tt)*] [$($pass:tt)*] [$($tys:tt)*]
        [$n:ident $($names:ident)*] $t:tt $(($($a:tt)*))? $(, $($rest:tt)*)?) => {
        builders!(@params $name $ret $targs [$($decl)* $n: Arg<rty!($t $(($($a)*))?), E>,]
            [$($pass)* $n.0,] [$($tys)* Arg<rty!($t $(($($a)*))?), crate::hir::Expr>,]
            [$($names)*] $($($rest)*)?);
    };
    (@fn $name:ident [$($ret:tt)*] [$targs:expr] [$($decl:tt)*] [$($pass:tt)*]) => {
        #[doc = concat!("A call of the hook `", stringify!($name), "`.")]
        #[inline]
        pub(crate) fn $name<E: Form>($($decl)*) -> Call<rty!($($ret)*), E> {
            Call::new(velt_common::std_hook!($name), $targs, [$($pass)*])
        }
    };
    ($($name:ident => $item:literal, $kind:ident $(($($sig:tt)*))? [$($flag:ident)*];)*) => {
        $(builders!(@emit $name [$kind $(($($sig)*))?]);)*
    };
}

// Builders sema does not call (the hooks only lowering calls) are generated too, so each
// function hook has one. Next to each builder, a module of its name holds its argument types
// (`args::any_text::Args`), public for the doctest of `velt_sema::hook_args`.
#[allow(dead_code, non_snake_case)]
pub mod args {
    use super::{ty, Arg, Call, Form};
    velt_common::std_hook_table!(builders);
}

pub(crate) use args::*;

/// The argument types of sema's typed hook builders (this module's), generated from
/// velt_common's hook table: public only so that this doctest can show a call with the wrong
/// types does not compile. `anyText(v, at)` takes the `any`, then the call site:
///
/// ```
/// use velt_sema::hir::{Expr, ExprKind, Lit, TyId};
/// use velt_sema::hook_args::{any_text, ty, HookTy};
/// let e = || Expr { kind: ExprKind::Lit(Lit::Unit), ty: TyId(0), span: Default::default() };
/// let _: any_text::Args = (ty::Any::of(e()), ty::Str::of(e()));
/// ```
///
/// Swapped, or with one argument too few, it is a type error:
///
/// ```compile_fail,E0308
/// use velt_sema::hir::{Expr, ExprKind, Lit, TyId};
/// use velt_sema::hook_args::{any_text, ty, HookTy};
/// let e = || Expr { kind: ExprKind::Lit(Lit::Unit), ty: TyId(0), span: Default::default() };
/// let _: any_text::Args = (ty::Str::of(e()), ty::Any::of(e()));
/// ```
///
/// ```compile_fail,E0308
/// use velt_sema::hir::{Expr, ExprKind, Lit, TyId};
/// use velt_sema::hook_args::{any_text, ty, HookTy};
/// let e = || Expr { kind: ExprKind::Lit(Lit::Unit), ty: TyId(0), span: Default::default() };
/// let _: any_text::Args = (ty::Any::of(e()),);
/// ```
pub mod hook_args {
    pub use crate::hook_calls::args::*;
    pub use crate::hook_calls::{ty, HookTy};
}
