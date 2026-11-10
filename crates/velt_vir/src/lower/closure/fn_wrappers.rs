//! Function wrappers keep the identity of the function they wrap (#2688, `hir::Program::
//! fn_wrappers`, hir_encodings.md "Function wrappers"). Sema makes a closure around a function
//! value to fit another function type (`{ const f = value; (p…) => f(p…) }`, or `(p…) =>
//! named(p…)`), a new value at each conversion. In JavaScript the converted function is the same
//! object, so `===`, `Object.is` and `Set` / `Map` keys look through the wrapper at the function
//! it stands for, through every wrapper in a chain.
//!
//! In a program that compares function values (`Boxing::fn_identity`) and makes a wrapper
//! (`Boxing::fn_wrappers`), a wrapper's environment is
//! `{ drop, clone: the marker, transfer, its own clone, kind, the wrapped value, … }`: the clone
//! entry is one glue function for every wrapper ([`Work::FnWrapperClone`]), which calls the
//! wrapper's own clone (as velt_rt's function identities mark themselves by their clone entry).
//! The wrapped value follows the kind word: the wrapper's one capture, or the named function's
//! value (`KIND_FN`), or the `any` a dynamic value converted to a function type holds
//! (`KIND_ANY`), whose table says `FN_VALUE_ID` in its class id word when it holds a function
//! value. The comparison follows the wrapped function while the environment's clone entry is the
//! marker ([`FnLower::fn_root`]), and two `any`s of two function types compare the functions
//! they lead to (any_eq.rs). Other programs are unchanged.

use velt_sema::hir::{DefId, FnWrapped, PassMode, TyId, TyKind};

use super::super::operand::proj;
use super::super::{cfunc, cint, Cx, FnLower, ThunkKind, Work};
use crate::vir::{AggId, BinOp, Const, Function, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// The field of a wrapper's environment holding its own clone entry.
const OWN_CLONE: u32 = 3;
/// The field of a wrapper's environment saying what the wrapped value is (`KIND_*`).
const KIND: u32 = 4;
/// The field of a wrapper's environment holding the value it wraps.
const WRAPPED: u32 = 5;
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
    /// and transfer entries, and a marked wrapper's own clone entry and kind.
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
        let fields = [Ty::Ptr, Ty::Ptr, Ty::Ptr, Ty::Ptr, Ty::Ptr, ca];
        let a = self.new_agg("fn wrapper env".into(), &fields);
        self.lay.fn_wrapper_env = Some(a);
        a
    }

    /// The marker clone entry of every wrapper's environment.
    fn wrapper_mark(&mut self) -> Operand {
        cfunc(self.func(Work::FnWrapperClone))
    }

    /// The class id word of the `any` table of `ty`, when it is a function type (`FN_VALUE_ID`).
    pub(in crate::lower) fn fn_value_id(&self, ty: TyId) -> Option<u64> {
        let f = matches!(self.kind(ty), TyKind::FnPtr { .. } | TyKind::Closure(_));
        (self.boxing.fn_wrappers && f).then_some(FN_VALUE_ID)
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// Record that closure `def` is made, when it is a wrapper (`Boxing::fn_wrappers`).
    pub(in crate::lower) fn note_wrapper_made(&mut self, def: DefId) {
        if self.cx.fn_wrapper(def).is_some() {
            self.cx.note_fn_wrapper();
        }
    }

    /// The header fields a marked wrapper `def` adds to its environment at `base` (whose drop,
    /// clone and transfer entries are written): the marker as its clone entry, its own after
    /// the transfer entry, the kind, and a named function's value.
    pub(in crate::lower) fn fill_wrapper_header(&mut self, def: DefId, base: &Place) {
        let Some(w) = self.cx.marked_wrapper(def) else {
            return;
        };
        let targs = self.targs.clone();
        let own = cfunc(self.cx.func(Work::EnvClone(def, targs)));
        let mark = self.cx.wrapper_mark();
        self.assign(proj(base, Proj::Field(1)), Rvalue::Use(mark));
        self.assign(proj(base, Proj::Field(OWN_CLONE)), Rvalue::Use(own));
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

    /// `===` of the function values at `a` and `b` whose words compare as `same`: the same
    /// value, or two that lead to one function (module docs). Two values that differ are told
    /// apart by a look at a clone entry, without following anything, unless one is a wrapper:
    /// at one code (one closure) both are wrappers or neither is, so a look at `a`'s does.
    pub(in crate::lower) fn fn_same_through_wrappers(
        &mut self,
        a: &Place,
        b: &Place,
        same: Operand,
    ) -> Operand {
        if !self.cx.boxing.fn_wrappers {
            return same;
        }
        let out = Place::local(self.temp(Ty::Bool));
        self.assign(out.clone(), Rvalue::Use(same.clone()));
        let (differ, one_code, two_codes) = (self.new_block(), self.new_block(), self.new_block());
        let (follow, done) = (self.new_block(), self.new_block());
        self.branch(same, done, differ);
        self.switch_to(differ);
        let (ca, cb) = (proj(a, Proj::Field(0)), proj(b, Proj::Field(0)));
        let codes = Rvalue::Binary(BinOp::Eq, Operand::Copy(ca), Operand::Copy(cb));
        let codes = self.rvalue_temp(Ty::Bool, codes);
        self.branch(codes, one_code, two_codes);
        self.switch_to(one_code);
        let wa = self.is_fn_wrapper(a);
        self.branch(wa, follow, done);
        self.switch_to(two_codes);
        let wa = self.is_fn_wrapper(a);
        let wb = self.is_fn_wrapper(b);
        let either = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitOr, wa, wb));
        self.branch(either, follow, done);
        self.switch_to(follow);
        let r = self.fn_roots_same(a, b);
        self.assign(out.clone(), Rvalue::Use(r));
        self.goto(done);
        self.switch_to(done);
        Operand::Copy(out)
    }

    /// `===` of the functions the values at `a` and `b` lead to.
    fn fn_roots_same(&mut self, a: &Place, b: &Place) -> Operand {
        let (ra, rb) = (self.fn_root(a), self.fn_root(b));
        let w = |lw: &mut Self, k: u32| {
            let (x, y) = (proj(&ra, Proj::Field(k)), proj(&rb, Proj::Field(k)));
            let (x, y) = (Operand::Copy(x), Operand::Copy(y));
            lw.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, x, y))
        };
        let (code, env) = (w(self, 0), w(self, 1));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::BitAnd, code, env))
    }

    /// Is the function value at `place` a wrapper (its environment's clone entry the marker)?
    fn is_fn_wrapper(&mut self, place: &Place) -> Operand {
        let wa = self.cx.wrapper_env_agg();
        let mark = self.cx.wrapper_mark();
        let out = Place::local(self.temp(Ty::Bool));
        let no = Operand::Const(Const::Bool(false), Ty::Bool);
        self.assign(out.clone(), Rvalue::Use(no));
        let env = Operand::Copy(proj(place, Proj::Field(1)));
        let env = self.rvalue_temp(Ty::Ptr, Rvalue::Use(env));
        let (look, done) = (self.new_block(), self.new_block());
        let nn = self.non_null(env.clone());
        self.branch(nn, look, done);
        self.switch_to(look);
        let ep = self.operand_place(env, Ty::Ptr);
        let clone = Operand::Copy(proj(&proj(&ep, Proj::Deref(Ty::Agg(wa))), Proj::Field(1)));
        self.assign(out.clone(), Rvalue::Binary(BinOp::Eq, clone, mark));
        self.goto(done);
        self.switch_to(done);
        Operand::Copy(out)
    }

    /// The function value at `place` (a closure aggregate), or the one the wrappers around it
    /// stand for (module docs): what `===` and a key's hash see.
    pub(in crate::lower) fn fn_root(&mut self, place: &Place) -> Place {
        if !self.cx.boxing.fn_wrappers {
            return place.clone();
        }
        let ca = self.cx.closure_agg();
        let wa = self.cx.wrapper_env_agg();
        let mark = self.cx.wrapper_mark();
        let cur = Place::local(self.temp(Ty::Agg(ca)));
        self.assign(cur.clone(), Rvalue::Use(Operand::Copy(place.clone())));
        let (head, test, wraps_bb, body, done) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        let (held, body_any) = (self.new_block(), self.new_block());
        self.goto(head);
        self.switch_to(head);
        let env = Operand::Copy(proj(&cur, Proj::Field(1)));
        let env = self.rvalue_temp(Ty::Ptr, Rvalue::Use(env));
        let nn = self.non_null(env.clone());
        self.branch(nn, test, done);
        self.switch_to(test);
        let ep = self.operand_place(env, Ty::Ptr);
        let base = proj(&ep, Proj::Deref(Ty::Agg(wa)));
        let clone = Operand::Copy(proj(&base, Proj::Field(1)));
        let wraps = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, clone, mark));
        self.branch(wraps, wraps_bb, done);
        self.switch_to(wraps_bb);
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
        self.branch(is_fn, body_any, done);
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

    /// `(env: ptr) -> ptr`, the marker clone entry of every wrapper's environment: the
    /// wrapper's own clone entry, called.
    pub(in crate::lower) fn build_wrapper_clone(cx: &'c mut Cx<'h>) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        let env = lw.new_local(Ty::Ptr, Some("env".into()));
        let wa = lw.cx.wrapper_env_agg();
        let base = proj(&Place::local(env), Proj::Deref(Ty::Agg(wa)));
        let own = Operand::Copy(proj(&base, Proj::Field(OWN_CLONE)));
        let own = lw.rvalue_temp(Ty::Ptr, Rvalue::Use(own));
        let ev = Operand::Copy(Place::local(env));
        let new = lw.call_entry(own, vec![ev], vec![Ty::Ptr], Ty::Ptr);
        lw.terminate(Terminator::Return(new));
        lw.finish("_Gfn_wrapper_clone".into(), vec![Ty::Ptr], Ty::Ptr)
    }
}
