//! The intrinsic table: `__intrinsic_<snake_case>` names (std only) and each intrinsic's
//! signature as a callable generic over `T = Param(0)`.

use velt_common::Span;

use super::args::Callable;
use crate::body::FnCx;
use crate::defs::ParamSig;
use crate::hir::{IntTy, Intrinsic, PassMode, TyId, TyKind};

/// `__intrinsic_<name>` → intrinsic (snake_case of the `Intrinsic` variant).
pub(super) fn intrinsic_named(name: &str) -> Option<Intrinsic> {
    use Intrinsic as I;
    Some(match name {
        "print" => I::Print,
        "print_err" => I::PrintErr,
        "to_string" => I::ToString,
        "str_concat" => I::StrConcat,
        "str_len" => I::StrLen,
        "str_char_code_at" => I::StrCharCodeAt,
        "exit" => I::Exit,
        "panic" => I::Panic,
        "array_with_capacity" => I::ArrayWithCapacity,
        "array_len" => I::ArrayLen,
        "array_push" => I::ArrayPush,
        "array_pop" => I::ArrayPop,
        "array_swap" => I::ArraySwap,
        "array_remove" => I::ArrayRemove,
        "array_truncate" => I::ArrayTruncate,
        "hash" => I::Hash,
        "eq" => I::Eq,
        "same" => I::Same,
        "clone" => I::Clone,
        "share" => I::Share,
        "sqrt" => I::Sqrt,
        "floor" => I::Floor,
        "ceil" => I::Ceil,
        "round" => I::Round,
        "trunc" => I::Trunc,
        "f_abs" => I::FAbs,
        "shared_new" => I::SharedNew,
        "array_data_ptr" => I::ArrayDataPtr,
        "json_stringify" => I::JsonStringify,
        "json_parse" => I::JsonParse,
        "http_handler" => I::HttpHandler,
        "promise_any" => I::PromiseAny,
        _ => return None,
    })
}

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
    /// Signature of an intrinsic as a generic callable over `T = Param(0)`.
    pub(super) fn intrinsic_sig(&mut self, i: Intrinsic, span: Span) -> Callable {
        let (params, ret, generic) = match self.m2_sig(i) {
            Some(s) => s,
            None => self.m3_sig(i),
        };
        let slot_names: Vec<String> = match (generic, i) {
            (false, _) => vec![],
            (
                true,
                Intrinsic::Spawn
                | Intrinsic::PromiseAll
                | Intrinsic::PromiseRace
                | Intrinsic::PromiseAny,
            ) => {
                vec!["T".into(), "E".into()]
            }
            (true, _) => vec!["T".into()],
        };
        Callable {
            what: format!("intrinsic `{i:?}`"),
            params: params.into_iter().map(|(t, m)| param(t, m, span)).collect(),
            ret,
            bounds: vec![vec![]; slot_names.len().max(1)],
            slot_names,
        }
    }

    fn m2_sig(&mut self, i: Intrinsic) -> Option<Sig> {
        use Intrinsic as I;
        use PassMode::{Borrow as B, BorrowMut as M, Copy as C, Owned as O};
        let ty = &mut self.cx.ty;
        let t = ty.param(0);
        let arr = ty.array(t);
        let (usize_, f64_, str_, unit, bool_) = (ty.usize, ty.f64, ty.str_, ty.unit, ty.bool_);
        Some(match i {
            I::ArrayWithCapacity => (vec![(usize_, C)], arr, true),
            I::ArrayLen => (vec![(arr, B)], usize_, true),
            I::ArrayPush => (vec![(arr, M), (t, O)], unit, true),
            I::ArrayPop => (vec![(arr, M)], ty.option(t), true),
            I::ArraySwap => (vec![(arr, M), (usize_, C), (usize_, C)], unit, true),
            I::ArrayRemove => (vec![(arr, M), (usize_, C)], t, true),
            I::ArrayTruncate => (vec![(arr, M), (usize_, C)], unit, true),
            I::Hash => (vec![(t, B)], ty.u64, true),
            I::Eq | I::Same => (vec![(t, B), (t, B)], bool_, true),
            I::Clone | I::Share => (vec![(t, B)], t, true),
            I::ToString => (vec![(t, B)], str_, true),
            I::SharedNew => {
                let s = ty.intern(TyKind::Shared(t));
                (vec![(t, O)], s, true)
            }
            I::Sqrt | I::Floor | I::Ceil | I::Round | I::Trunc | I::FAbs => {
                (vec![(f64_, C)], f64_, false)
            }
            I::StrConcat => (vec![(str_, B), (str_, B)], str_, false),
            I::StrLen => (vec![(str_, B)], usize_, false),
            I::StrCharCodeAt => (vec![(str_, B), (ty.i64, C)], ty.i64, false),
            I::Exit => (vec![(ty.i32, C)], ty.never, false),
            I::Panic => (vec![(str_, B)], ty.never, false),
            I::ArrayDataPtr => (vec![(arr, B)], ty.u64, true),
            I::Print | I::PrintErr => (vec![], unit, false),
            _ => return None,
        })
    }

    /// M3/M4 intrinsics. `MutexWith` and `HttpHandler` take closures and are checked by their
    /// own code; their signatures here only describe the shapes.
    fn m3_sig(&mut self, i: Intrinsic) -> Sig {
        use Intrinsic as I;
        use PassMode::{Borrow as B, Copy as C, Owned as O};
        let mutex = self.cx.mutex_ty();
        let ty = &mut self.cx.ty;
        let (t, e) = (ty.param(0), ty.param(1));
        let (unit, str_, i64_, u64_) = (ty.unit, ty.str_, ty.i64, ty.u64);
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
            I::PromiseRace | I::PromiseAny => (vec![(ty.array(pt), O)], pt, true),
            I::PerfNow => (vec![], ty.f64, false),
            I::DateNow => (vec![], i64_, false),
            I::SharedAdd => (vec![(shared, B), (t, C)], t, true),
            I::SharedGet => (vec![(shared, B)], t, true),
            I::SharedSet => (vec![(shared, B), (t, C)], unit, true),
            I::MutexNew => {
                let m = mutex.map_or(ty.error, |d| ty.intern(TyKind::Adt(d, vec![t])));
                (vec![(t, O)], m, true)
            }
            I::MutexWith => (vec![(t, B)], ty.error, true),
            I::JsonStringify => (vec![(t, B)], str_, true),
            I::JsonParse => (vec![(str_, B)], t, true),
            I::HttpHandler => {
                let f = http_handler_fn(ty);
                (vec![(f, O)], ty.intern(TyKind::Tuple(vec![u64_; 6])), false)
            }
            _ => unreachable!("ICE: intrinsic {i:?} has an M2 signature"),
        }
    }
}

/// `(raw: u64) => Promise<u64>`: the closure `__intrinsic_http_handler` turns into a handler.
pub(super) fn http_handler_fn(ty: &mut crate::types::Types) -> TyId {
    let u64_ = ty.intern(TyKind::Int(IntTy::U64));
    let ret = ty.promise(u64_);
    ty.fn_ptr(vec![u64_], ret)
}
