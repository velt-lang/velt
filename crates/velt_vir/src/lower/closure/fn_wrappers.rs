//! Function wrappers keep the identity of the function they wrap (#2688, `hir::Program::
//! fn_wrappers`, hir_encodings.md "Function wrappers"). Sema makes a closure around a function
//! value to fit another function type (`{ const f = value; (p…) => f(p…) }`, or `(p…) =>
//! named(p…)`), a new value at each conversion. In JavaScript the converted function is the same
//! object, so `===`, `Object.is` and `Set` / `Map` keys look through the wrapper at the function
//! it stands for, through every wrapper in a chain.
//!
//! In a program that compares function values (`Boxing::fn_identity`) and makes a wrapper
//! (`Boxing::fn_wrappers`), a wrapper's value has the low bit of its environment pointer set
//! (environments are aligned to 8), and its environment is
//! `{ drop, clone, transfer, kind, the wrapped value, … }`. The wrapped value is the wrapper's
//! one capture, or the named function's value (`KIND_FN`), or the `any` a dynamic value
//! converted to a function type holds (`KIND_ANY`), whose table says `FN_VALUE_ID` in its class
//! id word when it holds a function value.
//!
//! What reads an environment through a function value takes the bit off first
//! ([`FnLower::closure_env`]): the drop, clone, share, transfer and many-threads glue of
//! function values, which put it back on the copy they make, and a wrapper's own code
//! (abi.rs). `===` of two values neither of which has the bit compares their words as before;
//! one with it follows the wrapped values to the function they lead to ([`FnLower::fn_root`]),
//! and so does the hash of a key. Two `any`s of two function types compare the functions they
//! lead to (any_eq.rs). Other programs are unchanged.

use velt_sema::hir::{DefId, FnWrapped, PassMode, TyId, TyKind};

use super::super::operand::proj;
use super::super::{cfunc, cint, Cx, FnLower, ThunkKind, Work};
use crate::vir::{
    AggId, BinOp, Callee, Function, Inline, Local, Operand, Place, Proj, Rvalue, Terminator, Ty,
};

/// The field of a wrapper's environment saying what the wrapped value is (`KIND_*`).
const KIND: u32 = 3;
/// The field of a wrapper's environment holding the value it wraps.
const WRAPPED: u32 = 4;
/// The wrapped value is a function value.
const KIND_FN: i128 = 0;
/// The wrapped value is an `any`.
const KIND_ANY: i128 = 1;

/// The class id word of the `any` table of a function type in a program whose wrappers keep the
/// identity: its values are `{ code, env }`.
pub(in crate::lower) const FN_VALUE_ID: u64 = u64::MAX - 3;

// The reserved class id words are distinct.
const _: () = assert!(
    FN_VALUE_ID != super::super::adapters::ASK_INNER
        && FN_VALUE_ID != super::super::class_test::ASK_OBJECT
        && FN_VALUE_ID != super::super::any_set_site::ARRAY_ID
);

/// What a wrapper's environment holds after its header.
#[derive(Clone, Debug)]
pub(in crate::lower) enum Wrapper {
    /// Its one capture, the function value it calls.
    Captured,
    /// Its one capture, the `any` it calls.
    CapturedAny,
    /// The value of the module-level function it calls by name (not a capture).
    Named(DefId, Vec<TyId>),
}

impl Cx<'_> {
    /// Closure `def` as a wrapper whose environment can lead to the function it wraps: one that
    /// captures that function value (or an `any`) by move, or calls a named function and
    /// captures nothing.
    pub(in crate::lower) fn fn_wrapper(&self, def: DefId) -> Option<Wrapper> {
        let w = self.hir.fn_wrappers.get(&def)?;
        let f = self.fn_def(def);
        if f.is_async || f.is_generator {
            return None;
        }
        match w {
            FnWrapped::Local(l) => {
                let [c] = f.captures.as_slice() else {
                    return None;
                };
                let local = &f.body.locals[c.inner.0 as usize];
                if c.outer != *l || c.mode != PassMode::Owned || local.boxed {
                    return None;
                }
                match self.kind(local.ty) {
                    TyKind::FnPtr { .. } | TyKind::Closure(_) => Some(Wrapper::Captured),
                    _ if self.is_dynamic(local.ty) => Some(Wrapper::CapturedAny),
                    _ => None,
                }
            }
            FnWrapped::Fn(d, targs) => f
                .captures
                .is_empty()
                .then(|| Wrapper::Named(*d, targs.clone())),
        }
    }

    /// [`Cx::fn_wrapper`] in a program whose wrappers keep the identity (module docs).
    pub(in crate::lower) fn marked_wrapper(&self, def: DefId) -> Option<Wrapper> {
        match self.boxing.fn_wrappers {
            true => self.fn_wrapper(def),
            false => None,
        }
    }

    /// The header fields of closure `def`'s environment in front of its captures: drop, clone
    /// and transfer entries, and a marked wrapper's kind.
    pub(in crate::lower) fn env_header(&self, def: DefId) -> u32 {
        match self.marked_wrapper(def) {
            Some(_) => WRAPPED,
            None => super::ENV_HEADER,
        }
    }

    /// The fields closure `def`'s environment has in front of its captures (layout.rs), and
    /// the ones after them.
    pub(in crate::lower) fn wrapper_env_fields(&mut self, def: DefId) -> (Vec<Ty>, Vec<Ty>) {
        let header = vec![Ty::Ptr; self.env_header(def) as usize];
        let after = match self.marked_wrapper(def) {
            Some(Wrapper::Named(..)) => vec![Ty::Agg(self.closure_agg())],
            _ => vec![],
        };
        (header, after)
    }

    /// The layout every wrapper's environment starts with: the header and the wrapped value (a
    /// function value or an `any`, two words either way).
    fn wrapper_env_agg(&mut self) -> AggId {
        if let Some(a) = self.lay.fn_wrapper_env {
            return a;
        }
        let ca = Ty::Agg(self.closure_agg());
        let fields = [Ty::Ptr, Ty::Ptr, Ty::Ptr, Ty::Ptr, ca];
        let a = self.new_agg("fn wrapper env".into(), &fields);
        self.lay.fn_wrapper_env = Some(a);
        a
    }

    /// The class id word of the `any` table of `ty`, when it is a function type (`FN_VALUE_ID`).
    pub(in crate::lower) fn fn_value_id(&self, ty: TyId) -> Option<u64> {
        let f = matches!(self.kind(ty), TyKind::FnPtr { .. } | TyKind::Closure(_));
        (self.boxing.fn_wrappers && f).then_some(FN_VALUE_ID)
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// The env parameter `env` of closure `def`'s code, the wrapper bit taken off when `def` is
    /// a marked wrapper (its values have it).
    pub(in crate::lower) fn wrapper_own_env(&mut self, def: DefId, env: Local) -> Local {
        if self.cx.marked_wrapper(def).is_none() {
            return env;
        }
        let bare = self.untag_env(Operand::Copy(Place::local(env)));
        let l = self.new_local(Ty::Ptr, Some("env".into()));
        self.assign(Place::local(l), Rvalue::Use(bare));
        l
    }

    /// Record that closure `def` is made, when it is a wrapper (`Boxing::fn_wrappers`).
    pub(in crate::lower) fn note_wrapper_made(&mut self, def: DefId) {
        if self.cx.fn_wrapper(def).is_some() {
            self.cx.note_fn_wrapper();
        }
    }

    /// The header field a marked wrapper `def` adds to its environment at `base`, its kind, and
    /// a named function's value.
    pub(in crate::lower) fn fill_wrapper_header(&mut self, def: DefId, base: &Place) {
        let Some(w) = self.cx.marked_wrapper(def) else {
            return;
        };
        let kind = match w {
            Wrapper::CapturedAny => KIND_ANY,
            _ => KIND_FN,
        };
        self.assign(
            proj(base, Proj::Field(KIND)),
            Rvalue::Use(cint(kind, Ty::Ptr)),
        );
        if let Wrapper::Named(d, targs) = w {
            let targs: Vec<TyId> = targs.iter().map(|&t| self.sub(t)).collect();
            let thunk = self.cx.func(Work::Thunk(ThunkKind::Env(None), d, targs));
            let ca = self.cx.closure_agg();
            let v = Rvalue::Aggregate(ca, vec![cfunc(thunk), cint(0, Ty::Ptr)]);
            self.assign(proj(base, Proj::Field(WRAPPED)), v);
        }
    }

    /// The environment pointer `env` of a new value of closure `def`, with the wrapper bit when
    /// `def` is a marked wrapper.
    pub(in crate::lower) fn tag_wrapper_env(&mut self, def: DefId, env: Operand) -> Operand {
        match self.cx.marked_wrapper(def) {
            Some(_) => self.rvalue_temp(
                Ty::Ptr,
                Rvalue::Binary(BinOp::PtrAdd, env, cint(1, Ty::I64)),
            ),
            None => env,
        }
    }

    /// The wrapper bit of the environment pointer `env`, as an `i64` (0 or 1).
    fn env_bit(&mut self, env: Operand) -> Operand {
        let w = self.cast_to(env, Ty::Ptr, Ty::U64);
        let b = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::BitAnd, w, cint(1, Ty::U64)));
        self.cast_to(b, Ty::U64, Ty::I64)
    }

    /// `env` without the wrapper bit, which is `bit` (`env_bit`).
    fn env_without(&mut self, env: Operand, bit: Operand) -> Operand {
        let neg = self.rvalue_temp(Ty::I64, Rvalue::Binary(BinOp::Sub, cint(0, Ty::I64), bit));
        self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, env, neg))
    }

    /// The environment pointer `env` with the wrapper bit taken off (in a program whose
    /// wrappers keep the identity; as it is in any other).
    pub(in crate::lower) fn untag_env(&mut self, env: Operand) -> Operand {
        if !self.cx.boxing.fn_wrappers {
            return env;
        }
        let bit = self.env_bit(env.clone());
        self.env_without(env, bit)
    }

    /// The environment of the function value at `place`, the wrapper bit taken off, and the bit
    /// (`None` in a program without wrapper bits) to put back on a copy made of it.
    pub(in crate::lower) fn closure_env(&mut self, place: &Place) -> (Operand, Option<Operand>) {
        let env = Operand::Copy(proj(place, Proj::Field(1)));
        let env = self.rvalue_temp(Ty::Ptr, Rvalue::Use(env));
        if !self.cx.boxing.fn_wrappers {
            return (env, None);
        }
        let bit = self.env_bit(env.clone());
        let bare = self.env_without(env, bit.clone());
        (bare, Some(bit))
    }

    /// `env` (a new environment) with the wrapper bit `bit` of the one it was made from.
    pub(in crate::lower) fn retag_env(&mut self, env: Operand, bit: Option<Operand>) -> Operand {
        match bit {
            Some(b) => self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, env, b)),
            None => env,
        }
    }

    /// `===` of the function values at `a` and `b` whose words compare as `same`: the same
    /// value, or, when either has the wrapper bit, two that lead to one function.
    pub(in crate::lower) fn fn_same_through_wrappers(
        &mut self,
        a: &Place,
        b: &Place,
        same: Operand,
    ) -> Operand {
        if !self.cx.boxing.fn_wrappers {
            return same;
        }
        let _ = same;
        let word = |lw: &mut Self, p: &Place, k: u32| {
            let w = Operand::Copy(proj(p, Proj::Field(k)));
            lw.cast_to(w, Ty::Ptr, Ty::U64)
        };
        let (ca, cb, ea, eb) = (
            word(self, a, 0),
            word(self, b, 0),
            word(self, a, 1),
            word(self, b, 1),
        );
        let out = Place::local(self.temp(Ty::Bool));
        let no = Operand::Const(crate::vir::Const::Bool(false), Ty::Bool);
        self.assign(out.clone(), Rvalue::Use(no));
        let (one_code, two_codes, envs_differ) =
            (self.new_block(), self.new_block(), self.new_block());
        let (b_plain, yes, follow, done) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        let codes = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, ca, cb));
        self.branch(codes, one_code, two_codes);
        // One code: the same value, or two values of one closure, both wrappers or neither, so
        // `b`'s bit says (`b` is the value looked for in `indexOf`, `includes` and a key lookup:
        // a test the optimizer can take out of the loop).
        self.switch_to(one_code);
        let envs = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, ea.clone(), eb.clone()));
        self.branch(envs, yes, envs_differ);
        self.switch_to(envs_differ);
        let tb = self.low_bit(eb.clone());
        self.branch(tb, follow, done);
        // Two codes: equal only through a wrapper on either side.
        self.switch_to(two_codes);
        let tb = self.low_bit(eb);
        self.branch(tb, follow, b_plain);
        self.switch_to(b_plain);
        let ta = self.low_bit(ea);
        self.branch(ta, follow, done);
        self.switch_to(yes);
        let t = Operand::Const(crate::vir::Const::Bool(true), Ty::Bool);
        self.assign(out.clone(), Rvalue::Use(t));
        self.goto(done);
        self.switch_to(follow);
        // `a` by its address (an element, where the loops of `indexOf` and of a key lookup
        // read it, so its words are compared in place), `b` by value (the value looked for: an
        // address would keep it in memory).
        let f = cfunc(self.cx.func(Work::FnSame));
        let w = |p: &Place, k: u32| Operand::Copy(proj(p, Proj::Field(k)));
        let pa = self.addr(a.clone());
        let args = vec![pa, w(b, 0), w(b, 1)];
        let callee = Callee::Ptr {
            target: f,
            params: vec![Ty::Ptr; 3],
            ret: Ty::Bool,
        };
        self.call(callee, args, Some(out.clone()), false);
        self.goto(done);
        self.switch_to(done);
        Operand::Copy(out)
    }

    /// Is the low bit of the word `w` set?
    fn low_bit(&mut self, w: Operand) -> Operand {
        let bit = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::BitAnd, w, cint(1, Ty::U64)));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, bit, cint(0, Ty::U64)))
    }

    /// The root of the function value at `place`, from the out-of-line [`Work::FnRoot`].
    fn fn_root_call(&mut self, place: &Place) -> Place {
        let ca = self.cx.closure_agg();
        let out = Place::local(self.temp(Ty::Agg(ca)));
        let f = cfunc(self.cx.func(Work::FnRoot));
        let w = |k: u32| Operand::Copy(proj(place, Proj::Field(k)));
        let dst = self.addr(out.clone());
        let callee = Callee::Ptr {
            target: f,
            params: vec![Ty::Ptr, Ty::Ptr, Ty::Ptr],
            ret: Ty::Unit,
        };
        self.call(callee, vec![w(0), w(1), dst], None, false);
        out
    }

    /// The function value at `place`, or, when it has the wrapper bit, the function it leads
    /// to (out of line): what a key's hash sees, the words of any value that is no wrapper.
    pub(in crate::lower) fn fn_root_out_of_line(&mut self, place: &Place) -> Place {
        if !self.cx.boxing.fn_wrappers {
            return place.clone();
        }
        let ca = self.cx.closure_agg();
        let out = Place::local(self.temp(Ty::Agg(ca)));
        self.assign(out.clone(), Rvalue::Use(Operand::Copy(place.clone())));
        let env = Operand::Copy(proj(place, Proj::Field(1)));
        let bit = self.env_bit(env);
        let tagged = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, bit, cint(0, Ty::I64)));
        let (slow, done) = (self.new_block(), self.new_block());
        self.branch(tagged, slow, done);
        self.switch_to(slow);
        let r = self.fn_root_call(place);
        self.assign(out.clone(), Rvalue::Use(Operand::Copy(r)));
        self.goto(done);
        self.switch_to(done);
        out
    }

    /// `(a: ptr, b_code, b_env) -> bool`: [`Work::FnSame`], kept out of line so comparisons
    /// stay short.
    pub(in crate::lower) fn build_fn_same(cx: &'c mut Cx<'h>) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        let ws: Vec<Local> = ["a", "b_code", "b_env"]
            .iter()
            .map(|n| lw.new_local(Ty::Ptr, Some((*n).into())))
            .collect();
        let ca = lw.cx.closure_agg();
        let pa = proj(&Place::local(ws[0]), Proj::Deref(Ty::Agg(ca)));
        let (c, e) = (
            Operand::Copy(Place::local(ws[1])),
            Operand::Copy(Place::local(ws[2])),
        );
        let pb = Place::local(lw.temp(Ty::Agg(ca)));
        lw.assign(pb.clone(), Rvalue::Aggregate(ca, vec![c, e]));
        let r = lw.fn_roots_same(&pa, &pb);
        lw.terminate(Terminator::Return(r));
        let mut func = lw.finish("_Gfn_same".into(), vec![Ty::Ptr; 3], Ty::Bool);
        func.inline = Inline::Never;
        func
    }

    /// `(code, env, out: ptr)`: [`Work::FnRoot`], kept out of line so hashes stay short.
    pub(in crate::lower) fn build_fn_root(cx: &'c mut Cx<'h>) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        let code = lw.new_local(Ty::Ptr, Some("code".into()));
        let env = lw.new_local(Ty::Ptr, Some("env".into()));
        let out = lw.new_local(Ty::Ptr, Some("out".into()));
        let ca = lw.cx.closure_agg();
        let src = Place::local(lw.temp(Ty::Agg(ca)));
        let words = vec![
            Operand::Copy(Place::local(code)),
            Operand::Copy(Place::local(env)),
        ];
        lw.assign(src.clone(), Rvalue::Aggregate(ca, words));
        let root = lw.fn_root(&src);
        let dst = proj(&Place::local(out), Proj::Deref(Ty::Agg(ca)));
        lw.assign(dst, Rvalue::Use(Operand::Copy(root)));
        lw.terminate(Terminator::Return(super::super::unit()));
        let mut func = lw.finish("_Gfn_root".into(), vec![Ty::Ptr; 3], Ty::Unit);
        func.inline = Inline::Never;
        func
    }

    /// `===` of the functions the values at `a` and `b` lead to.
    fn fn_roots_same(&mut self, a: &Place, b: &Place) -> Operand {
        let (ra, rb) = (self.fn_root(a), self.fn_root(b));
        self.roots_same(&ra, &rb)
    }

    /// `===` of two function values that are no wrappers (roots).
    fn roots_same(&mut self, ra: &Place, rb: &Place) -> Operand {
        let w = |lw: &mut Self, k: u32| {
            let (x, y) = (proj(ra, Proj::Field(k)), proj(rb, Proj::Field(k)));
            let (x, y) = (Operand::Copy(x), Operand::Copy(y));
            lw.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y))
        };
        let (code, env) = (w(self, 0), w(self, 1));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, code, env))
    }

    /// The function value at `place` (a closure aggregate), or the one the wrappers around it
    /// stand for (module docs): what `===` and a key's hash see. Its environment never has the
    /// wrapper bit.
    pub(in crate::lower) fn fn_root(&mut self, place: &Place) -> Place {
        if !self.cx.boxing.fn_wrappers {
            return place.clone();
        }
        let ca = self.cx.closure_agg();
        let wa = self.cx.wrapper_env_agg();
        let cur = Place::local(self.temp(Ty::Agg(ca)));
        self.assign(cur.clone(), Rvalue::Use(Operand::Copy(place.clone())));
        let (head, wraps_bb, body, done) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        let (held, body_any, strip) = (self.new_block(), self.new_block(), self.new_block());
        self.goto(head);
        self.switch_to(head);
        let env = Operand::Copy(proj(&cur, Proj::Field(1)));
        let env = self.rvalue_temp(Ty::Ptr, Rvalue::Use(env));
        let bit = self.env_bit(env.clone());
        let wraps = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, bit.clone(), cint(0, Ty::I64)),
        );
        self.branch(wraps, wraps_bb, done);
        self.switch_to(wraps_bb);
        let bare = self.env_without(env, bit);
        let ep = self.operand_place(bare.clone(), Ty::Ptr);
        let base = proj(&ep, Proj::Deref(Ty::Agg(wa)));
        let kind = Operand::Copy(proj(&base, Proj::Field(KIND)));
        let any = cint(KIND_ANY, Ty::Ptr);
        let is_any = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, kind, any));
        self.branch(is_any, held, body);
        self.switch_to(body);
        let inner = Operand::Copy(proj(&base, Proj::Field(WRAPPED)));
        self.assign(cur.clone(), Rvalue::Use(inner));
        self.goto(head);
        // A wrapper of an `any`: the function value the `any` holds, if it holds one.
        self.switch_to(held);
        let any = proj(&base, Proj::Field(WRAPPED));
        let table = Operand::Copy(proj(&any, Proj::Field(1)));
        let id = self.vtable_class_id(table);
        let f = cint(FN_VALUE_ID as i128, Ty::U64);
        let is_fn = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, id, f));
        self.branch(is_fn, body_any, strip);
        // An `any` holding another callable: the wrapper is its own function, without the bit,
        // so nothing reads its environment through a tagged pointer.
        self.switch_to(strip);
        self.assign(proj(&cur, Proj::Field(1)), Rvalue::Use(bare));
        self.goto(done);
        self.switch_to(body_any);
        let data = Operand::Copy(proj(&any, Proj::Field(0)));
        let data = self.rvalue_temp(Ty::Ptr, Rvalue::Use(data));
        let dp = self.operand_place(data, Ty::Ptr);
        let inner = Operand::Copy(proj(&dp, Proj::Deref(Ty::Agg(ca))));
        self.assign(cur.clone(), Rvalue::Use(inner));
        self.goto(head);
        self.switch_to(done);
        cur
    }

    /// Do the `any` tables `xa` and `xb` both hold function values (`FN_VALUE_ID`)?
    pub(in crate::lower) fn any_both_fns(&mut self, xa: &Operand, xb: &Operand) -> Operand {
        let f = cint(FN_VALUE_ID as i128, Ty::U64);
        let ia = self.vtable_class_id(xa.clone());
        let ib = self.vtable_class_id(xb.clone());
        let fa = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, ia, f.clone()));
        let fb = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, ib, f));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, fa, fb))
    }

    /// `===` of the function values at the data pointers `da` and `db` of two `any`s: the
    /// functions their wrappers lead to.
    pub(in crate::lower) fn fn_values_same(&mut self, da: &Operand, db: &Operand) -> Operand {
        let ca = self.cx.closure_agg();
        let at = |lw: &mut Self, d: &Operand| {
            let d = lw.rvalue_temp(Ty::Ptr, Rvalue::Use(d.clone()));
            let p = lw.operand_place(d, Ty::Ptr);
            proj(&p, Proj::Deref(Ty::Agg(ca)))
        };
        let (a, b) = (at(self, da), at(self, db));
        self.fn_roots_same(&a, &b)
    }
}
