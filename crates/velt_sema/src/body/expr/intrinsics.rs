//! Intrinsic calls. A call of a function std declares with `declare intrinsic function`
//! (std/lang/intrinsics, `crate::intrinsic_decls`) is checked against that declaration like any
//! call and becomes the intrinsic ([`FnCx::intrinsic_call`]). The builtins that are intrinsics
//! without a declaration (`panic`, `sleep`, `xs.push`, `new Mutex`, …) have their signature here
//! ([`FnCx::intrinsic_sig`]), as a callable generic over `T = Param(0)`.

use velt_common::Span;
use velt_syntax::ast;

use super::args::Callable;
use crate::body::FnCx;
use crate::defs::ParamSig;
use crate::hir::{self, DefId, Intrinsic, PassMode, TyId, TyKind};
use crate::intrinsic_decls::DeclaredIntrinsic;

pub(super) fn param(ty: TyId, mode: PassMode, span: Span) -> ParamSig {
    ParamSig {
        name: String::new(),
        span,
        ty,
        mode,
        default: None,
    }
}

/// Parameter types and modes, result type, and whether the signature is generic over `T`
/// (promise intrinsics are also generic over the rejection type `E = Param(1)`).
type Sig = (Vec<(TyId, PassMode)>, TyId, bool);

impl FnCx<'_, '_> {
    /// A call of the declared intrinsic `d` (`decl`): its arguments checked against the
    /// declaration (explicit type arguments as for any generic function), then the intrinsic.
    #[allow(clippy::too_many_arguments)] // mirrors `fn_call`
    pub(super) fn intrinsic_call(
        &mut self,
        d: DefId,
        decl: DeclaredIntrinsic,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let i = match decl {
            DeclaredIntrinsic::Op(i) => i,
            DeclaredIntrinsic::TargetFeature => return self.target_stmt(args, span),
            DeclaredIntrinsic::AnyToPrimitive => return self.primitive_dispatch(args, span),
            DeclaredIntrinsic::Invalid => {
                self.check_args_loose(args);
                return self.error_expr(span);
            }
        };
        let f = self.cx.fn_info(d);
        let c = Callable {
            what: format!("intrinsic `{}`", f.name),
            params: f.params.clone(),
            ret: f.ret,
            slot_names: f.generics.names.clone(),
            bounds: f.generics.bounds.clone(),
            constraints: f.generics.constraints.clone(),
            js_numbers: false,
            positions: false,
            js_api: false,
            rest: false,
            defaults: f.generics.defaults.clone(),
        };
        let n = c.slot_names.len();
        let mut slots = vec![None; n];
        self.explicit_type_args(&mut slots, n, type_args, &c.defaults, span);
        let ck = self.check_call(&c, slots, args, exp, span);
        if matches!(
            i,
            Intrinsic::JsonParse
                | Intrinsic::JsonParseCursor
                | Intrinsic::ReadRows
                | Intrinsic::ReadRow
        ) {
            self.json_parse_throws(span);
        }
        if matches!(
            i,
            Intrinsic::GeneratorResume | Intrinsic::GeneratorResumeWith | Intrinsic::GeneratorThrow
        ) {
            // Resuming throws what the generator's body throws (its `E`).
            if let Some(&e) = ck.type_args.get(1) {
                self.throw_src(crate::defs::ThrowSrc::Direct(e, span));
            }
        }
        let e = self.intrinsic(i, ck.args, ck.ret, span);
        if i == Intrinsic::SpawnHandled {
            self.spawned_promises(&e);
        }
        e
    }

    /// `(x: T) -> C<T>`, `x` copied: the constructor of the prelude's struct `C` holding one
    /// number or `bool` (`new Atomic<T>(x)`, `new ThreadLocal<T>(x)`), whose operations are
    /// intrinsics taking the value copied too.
    pub(super) fn value_cell_ctor(&mut self, c: DefId, what: &str, span: Span) -> Callable {
        let t = self.cx.ty.param(0);
        let ret = self.cx.ty.intern(TyKind::Adt(c, vec![t]));
        Callable {
            what: what.into(),
            params: vec![param(t, PassMode::Copy, span)],
            ret,
            slot_names: vec!["T".into()],
            bounds: vec![vec![]],
            constraints: vec![],
            js_numbers: false,
            positions: false,
            js_api: false,
            rest: false,
            defaults: vec![],
        }
    }

    /// Signature of a builtin intrinsic as a generic callable over `T = Param(0)`. A declared
    /// intrinsic has none here: its declaration is its signature.
    pub(super) fn intrinsic_sig(&mut self, i: Intrinsic, span: Span) -> Callable {
        let (params, ret, generic) = match self.m2_sig(i) {
            Some(s) => s,
            None => self.m3_sig(i),
        };
        let slot_names: Vec<String> = match (generic, i) {
            (false, _) => vec![],
            (true, Intrinsic::Spawn | Intrinsic::PromiseAll | Intrinsic::PromiseRace) => {
                vec!["T".into(), "E".into()]
            }
            (true, _) => vec!["T".into()],
        };
        Callable {
            what: format!("intrinsic `{i:?}`"),
            params: params.into_iter().map(|(t, m)| param(t, m, span)).collect(),
            ret,
            bounds: vec![vec![]; slot_names.len().max(1)],
            constraints: vec![],
            slot_names,
            js_numbers: false,
            positions: false,
            js_api: false,
            rest: false,
            defaults: vec![],
        }
    }

    fn m2_sig(&mut self, i: Intrinsic) -> Option<Sig> {
        use Intrinsic as I;
        use PassMode::{Borrow as B, BorrowMut as M, Copy as C, Owned as O};
        let ty = &mut self.cx.ty;
        let t = ty.param(0);
        let arr = ty.array(t);
        let (str_, unit) = (ty.str_, ty.unit);
        Some(match i {
            I::SimdShuffle(simd, _) => {
                let v = ty.intern(TyKind::Simd(simd));
                (vec![(v, C), (v, C)], v, false)
            }
            I::ArrayPush => (vec![(arr, M), (t, O)], unit, true),
            I::ArrayPop => (
                vec![(arr, M)],
                ty.option_of(t, crate::hir::Nothing::Undefined),
                true,
            ),
            I::Clone => (vec![(t, B)], t, true),
            I::ToString => (vec![(t, B)], str_, true),
            I::SharedNew => {
                let s = ty.intern(TyKind::Shared(t));
                (vec![(t, O)], s, true)
            }
            I::Exit => (vec![(ty.i32, C)], ty.never, false),
            I::Panic => (vec![(str_, B)], ty.never, false),
            _ => return None,
        })
    }

    /// M3/M4 builtin intrinsics.
    fn m3_sig(&mut self, i: Intrinsic) -> Sig {
        use Intrinsic as I;
        use PassMode::{Borrow as B, Copy as C, Owned as O};
        let mutex = self.cx.mutex_ty();
        let rwlock = self.cx.rwlock_ty();
        let ty = &mut self.cx.ty;
        let (t, e) = (ty.param(0), ty.param(1));
        let (unit, i64_) = (ty.unit, ty.i64);
        let pt = ty.promise_rejecting(t, e);
        let shared = ty.intern(TyKind::Shared(t));
        match i {
            I::Spawn => (vec![(pt, O)], pt, true),
            I::Sleep => (vec![(i64_, C)], ty.promise(unit), false),
            I::YieldNow => (vec![], ty.promise(unit), false),
            I::PromiseAll => {
                let ps = ty.array(pt);
                let arr = ty.array(t);
                (vec![(ps, O)], ty.promise_rejecting(arr, e), true)
            }
            I::PromiseRace => (vec![(ty.array(pt), O)], pt, true),
            I::PerfNow => (vec![], ty.f64, false),
            I::DateNow => (vec![], i64_, false),
            I::SharedAdd => (vec![(shared, B), (t, C)], t, true),
            I::SharedGet => (vec![(shared, B)], t, true),
            I::SharedSet => (vec![(shared, B), (t, C)], unit, true),
            I::MutexNew => {
                let m = mutex.map_or(ty.error, |d| ty.intern(TyKind::Adt(d, vec![t])));
                (vec![(t, O)], m, true)
            }
            I::RwLockNew => {
                let r = rwlock.map_or(ty.error, |d| ty.intern(TyKind::Adt(d, vec![t])));
                (vec![(t, O)], r, true)
            }
            _ => match crate::intrinsic_decls::name_of(i) {
                Some(name) => unreachable!(
                    "ICE: intrinsic {i:?} is declared in std/lang/intrinsics as `{name}`: its declaration is its signature"
                ),
                None => unreachable!("ICE: intrinsic {i:?} has no builtin signature"),
            },
        }
    }
}
