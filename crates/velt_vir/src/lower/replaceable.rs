//! What can replace a value while a call borrows it (stabilize.rs): a borrow through a retained
//! container stays valid only as long as nothing assigns the place it points at, and sharing the
//! value costs a count update per call. Two facts keep the share to the calls that need it:
//!
//! - a field written nowhere but by its own class's constructor (`this.f = v`), or its
//!   initializer, keeps its value for good. Every body is scanned once, on the first question,
//!   for assignments and for places used to modify or move. Fields are told apart by class and
//!   name, and by name for object types, which are structural. A write through `any` or a proxy
//!   can reach any field, so a program with one has no such fields.
//! - a direct callee, with the type arguments of the call, writes only the fields its body names,
//!   in itself and in what it calls directly. One that runs code it can't see (a function value, a virtual or interface method,
//!   a `new`, an intrinsic not known to run none) may write anything, and so may one with a value
//!   whose drop can run a `dispose`, which no body names.

use std::rc::Rc;

use rustc_hash::FxHashSet;
use velt_sema::hir::{self, AdtKind, Callee, DefId, ExprKind as K, StmtKind as S, TyId, TyKind};

use super::frame_envs::children;
use super::quiet::pure_intrinsic;
use super::Cx;

/// A callee that writes more field names than this counts as writing anything: it can hardly help,
/// and each caller on a long chain would copy the names of those below it.
const MAX_STORES: usize = 64;

/// The fields the program writes outside constructors (module docs).
#[derive(Default, PartialEq, Eq)]
pub(super) struct FieldWrites {
    classes: FxHashSet<(DefId, String)>,
    /// Fields of object types and, through a write on such a view, of any class.
    plain: FxHashSet<String>,
    /// Every field name written.
    names: FxHashSet<String>,
    /// An element of a tuple is written.
    tuples: bool,
    /// A write the scan cannot name: any field of a type with an assigned field may change.
    keyed: bool,
    /// The direct subclasses of each class.
    subclasses: rustc_hash::FxHashMap<DefId, Vec<DefId>>,
}

/// What a direct callee writes, with what it calls directly (module docs).
#[derive(Default, PartialEq, Eq)]
pub(super) struct Stores {
    names: FxHashSet<String>,
    tuples: bool,
}

/// A direct callee with the type arguments of a call (`Cx::callee_id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CalleeId(u32);

impl CalleeId {
    /// The id numbered `n` (`Cx::callee_stores` keys).
    pub(super) fn of(n: u32) -> CalleeId {
        CalleeId(n)
    }
}

/// What one body does.
#[derive(Default)]
struct Scan {
    classes: Vec<(DefId, String)>,
    plain: Vec<String>,
    names: Vec<String>,
    tuples: bool,
    keyed: bool,
    /// It runs code the scan can't see.
    opaque: bool,
    calls: Vec<(DefId, Vec<TyId>)>,
    /// The type arguments the body is asked about with.
    targs: Vec<TyId>,
    /// Asked of a callee: is a value it may drop one whose drop can run a `dispose`?
    drops: bool,
    /// `this` in a constructor: a write of a field of it is the field's first value.
    ctor_this: Option<hir::LocalId>,
}

impl<'h> Cx<'h> {
    /// Does some code write field `index` of a value of type `base` after it was made?
    pub(super) fn field_written(&mut self, base: TyId, index: u32) -> bool {
        if self.boxing.any_writes || self.has_proxies() {
            return true;
        }
        if self.field_writes.is_none() {
            self.field_writes = Some(Rc::new(self.scan_field_writes()));
        }
        let Some(w) = self.field_writes.clone() else {
            return true;
        };
        match self.kind(base) {
            TyKind::Tuple(_) => w.tuples,
            TyKind::Adt(d, _) => {
                let hir::Def::Adt(a) = self.hir.def(d) else {
                    return true;
                };
                if w.keyed && self.hir.assigned_objects.contains(&d) {
                    return true;
                }
                let Some((root, name)) = self.field_root(d, index) else {
                    return true;
                };
                match a.kind {
                    AdtKind::Class => {
                        w.plain.contains(name) || w.classes.contains(&(root, name.to_string()))
                    }
                    _ => w.names.contains(name),
                }
            }
            _ => true,
        }
    }

    /// Can a call of direct callee `def` write field `index` of a value of type `base`? `None`
    /// for a function value or a method chosen at run time.
    pub(super) fn callee_may_write(
        &mut self,
        callee: Option<CalleeId>,
        base: TyId,
        index: u32,
    ) -> bool {
        let Some(s) = callee.and_then(|c| self.stores_of(c)) else {
            return true;
        };
        match self.kind(base) {
            TyKind::Tuple(_) => s.tuples,
            TyKind::Adt(d, _) => match self.field_root(d, index) {
                Some((_, name)) => s.names.contains(name),
                None => true,
            },
            _ => true,
        }
    }

    /// Can a call of direct callee `def` run a closure? `None` as for
    /// [`callee_may_write`](Self::callee_may_write).
    pub(super) fn callee_runs_closures(&mut self, callee: Option<CalleeId>) -> bool {
        callee.and_then(|c| self.stores_of(c)).is_none()
    }

    /// Of a call of `callee` with `args` arguments, those that may reach a function value
    /// (`velt_sema::value_params`).
    pub(super) fn value_param_bits(&mut self, callee: CalleeId, args: usize) -> u64 {
        if self.value_params.is_none() {
            self.value_params = Some(Rc::new(velt_sema::value_params::of_program(self.hir)));
        }
        let def = self.callees[callee.0 as usize].0;
        self.value_params
            .as_ref()
            .map_or(0, |v| v.of_args(def, args))
    }

    /// The id of direct callee `def` with type arguments `targs`.
    pub(super) fn callee_id(&mut self, def: DefId, targs: &[TyId]) -> CalleeId {
        let known = self.callee_ids.entry(def).or_default();
        if let Some((_, id)) = known.iter().find(|(t, _)| t == targs) {
            return *id;
        }
        let id = CalleeId(self.callees.len() as u32);
        known.push((targs.to_vec(), id));
        self.callees.push((def, targs.to_vec()));
        id
    }

    /// The class or object type declaring field `index` of `d`, and its name.
    fn field_root(&self, d: DefId, index: u32) -> Option<(DefId, &'h str)> {
        let hir::Def::Adt(first) = self.hir.def(d) else {
            return None;
        };
        let mut a = first;
        let mut root = d;
        while let Some(TyKind::Adt(b, _)) = a.base.map(|t| self.raw_kind(t)) {
            let hir::Def::Adt(up) = self.hir.def(*b) else {
                break;
            };
            if index as usize >= up.fields.len() {
                break;
            }
            (root, a) = (*b, up);
        }
        a.fields
            .get(index as usize)
            .map(|f| (root, f.name.as_str()))
    }

    pub(super) fn scan_field_writes(&mut self) -> FieldWrites {
        let mut out = FieldWrites::default();
        let hir = self.hir;
        for (i, def) in hir.defs.iter().enumerate() {
            let mut s = Scan::default();
            match def {
                hir::Def::Adt(a) => {
                    if let Some(TyKind::Adt(b, _)) = a.base.map(|t| self.raw_kind(t)) {
                        let sub = DefId(i as u32);
                        out.subclasses.entry(*b).or_default().push(sub);
                    }
                    for f in &a.fields {
                        f.default.iter().for_each(|e| self.scan_expr(e, &mut s));
                    }
                }
                hir::Def::Global(g) => self.scan_expr(&g.init, &mut s),
                hir::Def::Fn(f) => {
                    s.ctor_this = ctor_this(f);
                    self.scan_block(&f.body.block, &mut s);
                }
                _ => {}
            }
            out.classes.extend(s.classes);
            out.plain.extend(s.plain);
            out.names.extend(s.names);
            out.tuples |= s.tuples;
            out.keyed |= s.keyed;
        }
        out
    }

    /// Memoized; a function reached again while it is being decided (recursion) counts as one
    /// that runs code the scan can't see, as in quiet.rs.
    pub(super) fn stores_of(&mut self, callee: CalleeId) -> Option<Rc<Stores>> {
        if let Some(s) = self.callee_stores.get(&callee.0) {
            return s.clone();
        }
        self.callee_stores.insert(callee.0, None);
        let s = self.compute_stores(callee).map(Rc::new);
        self.callee_stores.insert(callee.0, s.clone());
        s
    }

    fn compute_stores(&mut self, callee: CalleeId) -> Option<Stores> {
        let (def, targs) = self.callees[callee.0 as usize].clone();
        if self.field_writes.is_none() {
            self.field_writes = Some(Rc::new(self.scan_field_writes()));
        }
        if self.has_proxies() {
            return None;
        }
        let f = match self.hir.def(def) {
            hir::Def::Fn(f) if !f.is_async && !f.is_generator => f,
            hir::Def::ExternFn(x) if !x.is_async => {
                let takes_fn = x.params.iter().any(|&p| {
                    matches!(self.raw_kind(p), TyKind::FnPtr { .. } | TyKind::Closure(_))
                });
                return (!takes_fn).then(Stores::default);
            }
            _ => return None,
        };
        let mut s = Scan {
            drops: true,
            targs,
            ..Scan::default()
        };
        for l in &f.body.locals {
            let t = self.subst(l.ty, &s.targs);
            s.opaque |= self.drops_dispose(t);
        }
        self.scan_block(&f.body.block, &mut s);
        if s.opaque || s.keyed {
            return None;
        }
        let mut out = Stores {
            names: s.names.into_iter().collect(),
            tuples: s.tuples,
        };
        for (d, t) in s.calls {
            let c = self.callee_id(d, &t);
            let inner = self.stores_of(c)?;
            out.names.extend(inner.names.iter().cloned());
            out.tuples |= inner.tuples;
            if out.names.len() > MAX_STORES {
                return None;
            }
        }
        Some(out)
    }

    /// Can dropping a value of type `t` run a `dispose`? A class counts with its subclasses, as a
    /// value of it may be one. An interface or `any` value, a function value's environment, a
    /// promise's task and a type parameter may hold anything; inside a generic type, the type
    /// arguments stand for its parameters.
    fn drops_dispose(&mut self, t: TyId) -> bool {
        if let Some(&d) = self.dispose_types.get(&t) {
            return d;
        }
        // A type shared by several instances answers for all of them, through its representative.
        let rep = self.ph_rep(t);
        let d = self.mentions_ph(rep) || self.drops_dispose_in(rep, false, &mut Vec::new());
        self.dispose_types.insert(t, d);
        d
    }

    fn drops_dispose_in(&mut self, t: TyId, in_adt: bool, seen: &mut Vec<TyId>) -> bool {
        if seen.contains(&t) {
            return false;
        }
        seen.push(t);
        match self.raw_kind(t).clone() {
            TyKind::Param(_) => !in_adt,
            TyKind::Array(x) | TyKind::Option(x, _) | TyKind::Shared(x) => {
                self.drops_dispose_in(x, in_adt, seen)
            }
            TyKind::Map(a, b) | TyKind::Result(a, b) => {
                self.drops_dispose_in(a, in_adt, seen) || self.drops_dispose_in(b, in_adt, seen)
            }
            TyKind::Tuple(xs) => xs.iter().any(|&x| self.drops_dispose_in(x, in_adt, seen)),
            TyKind::Adt(d, args) => {
                args.iter().any(|&x| self.drops_dispose_in(x, in_adt, seen))
                    || self.def_drops_dispose(d, seen)
            }
            TyKind::Dyn(..)
            | TyKind::FnPtr { .. }
            | TyKind::Closure(_)
            | TyKind::Promise(..)
            | TyKind::Op(..) => true,
            _ => false,
        }
    }

    /// [`drops_dispose_in`](Self::drops_dispose_in) for what definition `d` holds.
    fn def_drops_dispose(&mut self, d: DefId, seen: &mut Vec<TyId>) -> bool {
        let hir = self.hir;
        match hir.def(d) {
            hir::Def::Adt(a) => {
                let subclasses = self
                    .field_writes
                    .clone()
                    .and_then(|w| w.subclasses.get(&d).cloned());
                a.dispose.is_some()
                    || a.fields
                        .iter()
                        .any(|f| self.drops_dispose_in(f.ty, true, seen))
                    || subclasses
                        .iter()
                        .flatten()
                        .any(|&c| self.def_drops_dispose(c, seen))
            }
            hir::Def::Enum(e) => (e.variants.iter())
                .flat_map(|v| v.payload.iter())
                .any(|&p| self.drops_dispose_in(p, true, seen)),
            _ => true,
        }
    }

    fn scan_block(&mut self, b: &hir::Block, s: &mut Scan) {
        for st in &b.stmts {
            self.scan_stmt(st, s);
        }
        if let Some(e) = &b.value {
            self.scan_expr(e, s);
        }
    }

    fn scan_stmt(&mut self, st: &hir::Stmt, s: &mut Scan) {
        match &st.kind {
            S::Let { init, .. } => init.iter().for_each(|e| self.scan_expr(e, s)),
            S::LetPat { init, .. } | S::Expr(init) | S::Return(Some(init)) => {
                self.scan_expr(init, s)
            }
            S::Return(None) | S::Break(_) | S::Continue(_) => {}
            S::If { cond, then, els } => {
                self.scan_expr(cond, s);
                self.scan_block(then, s);
                els.iter().for_each(|b| self.scan_block(b, s));
            }
            S::While {
                cond, body, step, ..
            } => {
                self.scan_expr(cond, s);
                self.scan_block(body, s);
                step.iter().for_each(|e| self.scan_expr(e, s));
            }
            S::ForOf { iter, body, .. } => {
                // Only an array's loop calls nothing.
                s.opaque |= !matches!(self.raw_kind(iter.ty), TyKind::Array(_));
                self.scan_expr(iter, s);
                self.scan_block(body, s);
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.scan_block(body, s);
                catch.iter().for_each(|(_, b)| self.scan_block(b, s));
                finally.iter().for_each(|b| self.scan_block(b, s));
            }
            S::Block(b) => self.scan_block(b, s),
        }
    }

    fn scan_expr(&mut self, e: &hir::Expr, s: &mut Scan) {
        if s.drops && !s.opaque {
            let t = self.subst(e.ty, &s.targs);
            s.opaque = self.drops_dispose(t);
        }
        match &e.kind {
            K::Assign { place, value } | K::CompoundAssign { place, value, .. } => {
                self.scan_place(place, s);
                self.scan_expr(value, s);
                return;
            }
            K::Block(b) => return self.scan_block(b, s),
            K::Field {
                base,
                index,
                mode: hir::UseMode::BorrowMut | hir::UseMode::Move,
            } => self.mark_field(base.ty, *index, s),
            K::UnwrapSome(base, hir::UseMode::BorrowMut | hir::UseMode::Move)
            | K::UnwrapVariant {
                expr: base,
                mode: hir::UseMode::BorrowMut | hir::UseMode::Move,
                ..
            } => self.scan_place(base, s),
            K::New { .. } | K::Await(_) => s.opaque = true,
            K::Call { callee, args } => match callee {
                Callee::Def(d, t) => {
                    let t = t.iter().map(|&x| self.subst(x, &s.targs)).collect();
                    s.calls.push((*d, t));
                }
                Callee::Intrinsic(hir::Intrinsic::KeyWrite) => s.keyed = true,
                // On strings and numbers these only compare or format.
                Callee::Intrinsic(
                    hir::Intrinsic::Hash
                    | hir::Intrinsic::KeyEq
                    | hir::Intrinsic::IncludesEq
                    | hir::Intrinsic::Same
                    | hir::Intrinsic::ToString,
                ) if s.drops
                    && args
                        .iter()
                        .all(|a| self.holds_text_or_number(a.ty, &s.targs)) => {}
                Callee::Intrinsic(i) => s.opaque |= !runs_nothing(*i),
                _ => s.opaque = true,
            },
            _ => {}
        }
        children(e, &mut |x| self.scan_expr(x, s));
    }

    /// Is a value of type `t` a string, a number or a boolean, or one of them or nothing?
    fn holds_text_or_number(&mut self, t: TyId, targs: &[TyId]) -> bool {
        let t = self.subst(t, targs);
        match self.kind(t) {
            TyKind::Str | TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Literal(_) => {
                true
            }
            TyKind::Option(x, _) => self.holds_text_or_number(x, &[]),
            _ => false,
        }
    }

    /// An assignment's target: every field on the way changes (a part stored inline), except
    /// `this.f` in a constructor. Index operands are evaluated.
    fn scan_place(&mut self, p: &hir::Expr, s: &mut Scan) {
        if let (K::Field { base, .. }, Some(this)) = (&p.kind, s.ctor_this) {
            if matches!(base.kind, K::Local(l, _) if l == this) {
                return;
            }
        }
        match &p.kind {
            K::Field { base, index, .. } => {
                self.mark_field(base.ty, *index, s);
                self.scan_place(base, s);
            }
            K::Index { base, index, .. } => {
                self.scan_expr(index, s);
                self.scan_place(base, s);
            }
            K::UnwrapSome(base, _)
            | K::UnwrapVariant { expr: base, .. }
            | K::Downcast(base)
            | K::Upcast(base)
            | K::Cast(base) => self.scan_place(base, s),
            K::Local(..) => {}
            _ => self.scan_expr(p, s),
        }
    }

    fn mark_field(&mut self, base: TyId, index: u32, s: &mut Scan) {
        match self.raw_kind(base) {
            TyKind::Tuple(_) => s.tuples = true,
            TyKind::Adt(d, _) => {
                let d = *d;
                let Some((root, name)) = self.field_root(d, index) else {
                    s.keyed = true;
                    return;
                };
                let name = name.to_string();
                match self.hir.def(d) {
                    hir::Def::Adt(a) if a.kind == AdtKind::Class => {
                        s.classes.push((root, name.clone()))
                    }
                    _ => s.plain.push(name.clone()),
                }
                s.names.push(name);
            }
            _ => s.keyed = true,
        }
    }
}

/// `this` of a constructor.
fn ctor_this(f: &hir::FnDef) -> Option<hir::LocalId> {
    let ctor = f.self_ty.is_some() && f.name.ends_with(".constructor");
    ctor.then(|| f.params.first().map(|p| p.local)).flatten()
}

/// Does intrinsic `i` run no code but its own, whatever it is given?
fn runs_nothing(i: hir::Intrinsic) -> bool {
    use hir::Intrinsic as I;
    pure_intrinsic(i)
        || matches!(
            i,
            I::Share
                | I::HoldsNull
                | I::NullAs
                | I::Panic
                | I::StrConcat
                | I::ArrayWithCapacity
                | I::ArrayPush
                | I::ArrayPop
                | I::ArraySwap
                | I::ArrayRemove
                | I::ArrayTruncate
                | I::ArraySetLen
                | I::ArrayReserve
                | I::ArrayDataPtr
                | I::StrbufPushAsciiWord
                | I::StrbufPushAsciiWords
        )
}
