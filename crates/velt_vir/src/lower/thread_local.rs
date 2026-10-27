//! `ThreadLocal<T>` (std/prelude/sync.vlt): the `ThreadLocal*` intrinsics as `velt_tls_*` externs
//! (crate::tls), which backends emit inline on thread-local storage they define. The receiver is
//! the module constant holding the value (sema `thread_locals`): it is never evaluated; its
//! qualified name names the storage and its initializer's literal is the starting value.

use velt_sema::hir::{self, ExprKind as K, Intrinsic, Lit, TyKind, UnOp};

use super::operand::proj;
use super::{ice, unit, FnLower};
use crate::atomic::word;
use crate::tls::TlsCall;
use crate::vir::{BinOp, Callee, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    pub(super) fn thread_local_intrinsic(&mut self, i: Intrinsic, args: &[hir::Expr]) -> Operand {
        let Some(K::Global(d)) = args.first().map(|a| &a.kind) else {
            ice("a ThreadLocal intrinsic whose receiver is not a module constant");
        };
        let hir::Def::Global(g) = self.cx.hir.def(*d) else {
            ice("a ThreadLocal intrinsic on a non-global");
        };
        let K::AdtLit { fields, .. } = &g.init.kind else {
            ice("a ThreadLocal constant not made by `new ThreadLocal`");
        };
        let aty = self.sub(args[0].ty);
        let vt = match self.cx.kind(aty) {
            TyKind::Adt(_, targs) if targs.len() == 1 => self.cx.ty(targs[0]),
            k => ice(format_args!("ThreadLocal intrinsic on {k:?}")),
        };
        let bits = match vt {
            Ty::Bool => 8,
            t => t.scalar_size().unwrap_or(8) * 8,
        };
        let init = literal_word(&fields[0], vt, bits);
        // Named by its qualified name and type, not its `DefId`: the same constant keeps its
        // storage across `velt dev` versions in which other declarations come and go, and an
        // edit changing its type gets new storage.
        let id = crate::tls::stable_id(&format!("{}:{vt:?}", g.name));
        let w = word(bits);
        match (i, args) {
            (Intrinsic::ThreadLocalGet, [_]) => {
                let call = TlsCall::Get { id, bits, init };
                let v = self.tls_call(call, vec![]);
                match vt {
                    Ty::Bool => {
                        let zero = super::cint(0, w);
                        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, v, zero))
                    }
                    t if t.is_float() => self.float_word(v, w, t),
                    t => self.cast_to(v, w, t),
                }
            }
            (Intrinsic::ThreadLocalSet, [_, v]) => {
                let v = self.expr(v);
                let v = match vt.is_float() {
                    true => self.float_word(v, vt, w),
                    false => self.cast_to(v, vt, w),
                };
                self.tls_call(TlsCall::Set { id, bits, init }, vec![v])
            }
            _ => ice(format_args!(
                "intrinsic {i:?} called with {} arguments",
                args.len()
            )),
        }
    }

    /// The call of the extern `call`, inline in every backend.
    fn tls_call(&mut self, call: TlsCall, args: Vec<Operand>) -> Operand {
        let (params, ret) = call.signature();
        let id = self.cx.extern_sym(&call.symbol(), params, ret, false);
        if ret == Ty::Unit {
            self.call(Callee::Extern(id), args, None, false);
            return unit();
        }
        let out = self.temp(ret);
        self.call(Callee::Extern(id), args, Some(Place::local(out)), false);
        Operand::Copy(Place::local(out))
    }

    /// `v`'s bits as a value of type `to`, of the same width (one a float).
    fn float_word(&mut self, v: Operand, from: Ty, to: Ty) -> Operand {
        let fa = self.cx.new_agg(format!("{from:?} bits"), &[from]);
        let ta = self.cx.new_agg(format!("{to:?} bits"), &[to]);
        let t = self.temp(Ty::Agg(fa));
        self.assign(Place::local(t), Rvalue::Aggregate(fa, vec![v]));
        let p = Place::local(t);
        Operand::Copy(proj(&proj(&p, Proj::Cast(ta)), Proj::Field(0)))
    }
}

/// The word of `bits` a literal of VIR type `vt` is (sema `thread_local` checked the shape).
pub(super) fn literal_word(e: &hir::Expr, vt: Ty, bits: u32) -> u64 {
    let (neg, lit) = match &e.kind {
        K::Unary {
            op: UnOp::Neg,
            expr,
        } => (true, &expr.kind),
        k => (false, k),
    };
    let w = match lit {
        K::Lit(Lit::Bool(b)) => u64::from(*b),
        K::Lit(Lit::Int(n)) if vt.is_float() => float_bits(*n as f64, neg, vt),
        K::Lit(Lit::Float(f)) if vt.is_float() => float_bits(*f, neg, vt),
        K::Lit(Lit::Int(n)) => {
            let n = *n as u64;
            if neg {
                n.wrapping_neg()
            } else {
                n
            }
        }
        k => ice(format_args!("ThreadLocal starting value {k:?}")),
    };
    match bits {
        64 => w,
        _ => w & ((1u64 << bits) - 1),
    }
}

fn float_bits(f: f64, neg: bool, vt: Ty) -> u64 {
    let f = if neg { -f } else { f };
    match vt {
        Ty::F32 => u64::from((f as f32).to_bits()),
        _ => f.to_bits(),
    }
}
