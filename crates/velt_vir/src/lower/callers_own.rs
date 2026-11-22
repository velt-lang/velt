//! The caller's own objects a `read` callback stores into the caller's containers (#3126):
//! objects of the code outside every read, counted by that code's counts, with their parts
//! counted by them. A keep transfer (caller_stores.rs `keep_for_caller`) would take one outside
//! the lock set for an object the reader made, whose parts the reader held uncounted, and count
//! those parts once more. So the store of such an object as a whole takes only its reference,
//! and one inside a value the reader makes (`{ o: out }`) is recorded as kept as itself first
//! (velt_rt `velt_rt_lk_kept_again`), which keeps the transfer out of its parts.

use velt_sema::hir::{self, ExprKind as K, Intrinsic, LocalId, StmtKind as S, TyId, TyKind};

use super::rt::Rt;
use super::FnLower;
use crate::vir::{Operand, Place, Rvalue, Ty};

/// How deep `callers_object` follows conditions and a callback's variables.
const DEPTH: u32 = 8;

impl FnLower<'_, '_> {
    /// Is `ty` a function value's type, or an option of one (`(() => T) | null`)?
    pub(super) fn fn_value_type(&mut self, ty: TyId) -> bool {
        let t = match self.cx.kind(ty) {
            TyKind::Option(e, _) => e,
            _ => ty,
        };
        matches!(self.cx.kind(t), TyKind::FnPtr { .. } | TyKind::Closure(_))
    }

    /// Does the caller count a value of type `ty` as any object, its parts counted by it: a
    /// counted object outside the lock set, or a function value, that reaches the lock set? (A
    /// lock-set object of the caller's the reader holds uncounted: its store counts it.)
    pub(super) fn callers_own_type(&mut self, ty: TyId) -> bool {
        if !self.cx.reaches_lock_set(ty) {
            return false;
        }
        if self.fn_value_type(ty) {
            return true;
        }
        let t = match self.cx.kind(ty) {
            TyKind::Option(e, _) => e,
            _ => ty,
        };
        self.cx.ty(ty) == Ty::Ptr && self.cx.counted(t) && !self.cx.lock_counted(t)
    }

    /// Is `e`, in a `read` callback that code outside read mode calls, one of that code's own
    /// objects as a whole: a variable the callback captured from it (`caller_capture`) that
    /// nothing can assign during the reads, since it lives in no cell (`LocalDef::boxed`: any
    /// closure assigning a captured variable shares a cell with it); also chosen by a condition
    /// or a `match`, or through a variable of the callback's own bound to such a value by its
    /// `let` and never assigned after. Its object stays alive until the reads end: the code
    /// holding it waits for them.
    pub(super) fn callers_object(&mut self, e: &hir::Expr) -> bool {
        self.own_captures && self.callers_object_at(e, 0)
    }

    fn callers_object_at(&mut self, e: &hir::Expr, depth: u32) -> bool {
        if depth > DEPTH {
            return false;
        }
        match &e.kind {
            K::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::Share),
                args,
            } if args.len() == 1 => self.callers_object_at(&args[0], depth + 1),
            K::WrapSome(b) | K::Upcast(b) => self.callers_object_at(b, depth + 1),
            K::If { then, els, .. } => {
                self.callers_object_at(then, depth + 1) && self.callers_object_at(els, depth + 1)
            }
            K::Match { arms, .. } => {
                !arms.is_empty()
                    && arms.iter().all(|a| {
                        let t = self.sub(a.body.ty);
                        self.cx.is_never(t) || self.callers_object_at(&a.body, depth + 1)
                    })
            }
            K::Local(l, _) => {
                let l = *l;
                let Some(hir::Def::Fn(f)) = self.cx.cur_def.map(|d| self.cx.hir.def(d)) else {
                    return false;
                };
                if f.body.locals[l.0 as usize].boxed {
                    return false;
                }
                if f.captures.iter().any(|c| c.inner == l) {
                    return self.caller_capture(l);
                }
                // A variable of the callback's own, never assigned after its `let`.
                if f.body.locals[l.0 as usize].mutable {
                    return false;
                }
                match let_init(&f.body.block, l) {
                    Some(init) => self.callers_object_at(&init, depth + 1),
                    None => false,
                }
            }
            _ => false,
        }
    }

    /// Before the store of `e` (`held_by_caller`), a value the reader makes holding the caller's
    /// own objects (`callers_object`) outside the lock set (`{ o: out }`, `[[7]], out`): each is
    /// recorded as kept as itself in these reads (velt_rt `velt_rt_lk_kept_again`), so the
    /// transfer leaves its parts alone. Each branch of a condition is recorded, the one not
    /// taken too: the record only keeps its parts from being counted again, which the caller
    /// holds counted, and it stays alive until the reads end (`callers_object`), as the record
    /// requires (velt_rt `KEPT`).
    pub(super) fn note_callers_own(&mut self, e: &hir::Expr) {
        if self.dead() {
            return;
        }
        match &e.kind {
            K::ArrayLit(xs) | K::Tuple(xs) => xs.iter().for_each(|x| self.note_callers_own(x)),
            K::AdtLit { fields: xs, .. }
            | K::New { args: xs, .. }
            | K::Variant { args: xs, .. } => xs.iter().for_each(|x| self.note_callers_own(x)),
            K::WrapSome(b) | K::Upcast(b) => self.note_callers_own(b),
            K::If { then, els, .. } => {
                self.note_callers_own(then);
                self.note_callers_own(els);
            }
            K::Match { arms, .. } => arms.iter().for_each(|a| self.note_callers_own(&a.body)),
            K::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::Share),
                args,
            } if args.len() == 1 => self.note_callers_own(&args[0]),
            K::Local(l, _) => {
                let ty = self.sub(e.ty);
                let object = self.cx.ty(ty) == Ty::Ptr && self.callers_own_type(ty);
                if !object || !self.callers_object(e) {
                    return;
                }
                let Some(p) = self.local_target(*l) else {
                    return;
                };
                let o = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(p)));
                let seen = self.temp(Ty::U8);
                self.call_rt(Rt::LkKeptAgain, vec![o], Some(Place::local(seen)));
            }
            _ => {}
        }
    }
}

/// The value `l` is bound to by its `let` in `b`, if it has one.
fn let_init(b: &hir::Block, l: LocalId) -> Option<hir::Expr> {
    b.stmts.iter().find_map(|s| match &s.kind {
        S::Let {
            local,
            init: Some(init),
        } if *local == l => Some(init.clone()),
        S::If { then, els, .. } => {
            let_init(then, l).or_else(|| els.as_ref().and_then(|e| let_init(e, l)))
        }
        S::While { body, .. } | S::ForOf { body, .. } | S::Block(body) => let_init(body, l),
        S::Try {
            body,
            catch,
            finally,
        } => let_init(body, l)
            .or_else(|| catch.as_ref().and_then(|(_, c)| let_init(c, l)))
            .or_else(|| finally.as_ref().and_then(|f| let_init(f, l))),
        _ => None,
    })
}
