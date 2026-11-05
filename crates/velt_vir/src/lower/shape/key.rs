//! The shape of a type argument (design note section 1) and the checks that keep it exact.
//!
//! Inside a shared instance, every question about a placeholder that lowering answers from
//! layout or ownership is answered for the representative (`Cx::ph_rep` at the top of those
//! queries, for a placeholder and for every type built from one). So the key is every answer
//! such code may depend on for the type itself and for the derived types the shared helpers
//! use (`T[]`); `shapes_close` checks, at the end of the pass, that every member of each shape
//! gives the representative's answers for every type built from a placeholder that was asked
//! about.

use rustc_hash::FxHashSet;
use velt_sema::hir::{TyId, TyKind};

use super::super::{ice, Cx};
use crate::vir::Ty;

/// A type's layout as shared code sees it: aggregates by size and alignment.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Lay {
    Scalar(Ty),
    Agg(u32, u32),
}

/// The shape key of a type argument.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::lower) struct Key {
    /// The storage float it is (`Cx::half`), which arithmetic on it converts through.
    half: Option<String>,
    /// `ShareKind` as a number.
    share: u8,
    /// The answers `answers` gives for `T` and for each type of the footprint
    /// (`footprint`): what the end-of-pass check compares, so a key it accepts never fails it.
    answers: Vec<Answers>,
}

/// The answers to the shape questions about one type (`Cx::answers`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::lower) struct Answers {
    /// Its layout (`Cx::ty`), aggregates by size and alignment.
    lay: Option<Lay>,
    bits: u32,
    /// Membership in each of `Boxing::ty_sets`.
    sets: Vec<bool>,
}

/// Test hook (debug builds of the compiler only): `VELT_SHAPES_TEST_DROP=<row>` leaves one
/// row (`counted`, `weak`, `niche`, `align`, or `array`: `T[]`) out of the key, never out of
/// the end-of-pass check, so tests show that the check catches a key missing it.
pub(super) fn dropped(row: &str) -> bool {
    cfg!(debug_assertions)
        && !CHECKING.with(std::cell::Cell::get)
        && std::env::var("VELT_SHAPES_TEST_DROP").is_ok_and(|v| v == row)
}

thread_local! {
    /// The end-of-pass check is running: rows are never dropped then (`dropped`).
    static CHECKING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Key {
    /// `~` and the layout (`~f64`, `~agg64x8`), then a hash of the other answers.
    pub(super) fn name(&self) -> String {
        let lay = match self.answers.first().and_then(|a| a.lay.as_ref()) {
            Some(Lay::Scalar(t)) => format!("{t}"),
            Some(Lay::Agg(s, a)) => format!("agg{s}x{a}"),
            None => "unit".into(),
        };
        // FNV-1a of the answers: the same in every build of the same compiler.
        let text = format!("{:?}{}{:?}", self.half, self.share, self.answers);
        let h = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
        });
        format!("~{lay}.{:x}", h & 0xffff_ffff)
    }
}

impl Cx<'_> {
    /// `t` with each placeholder replaced by its representative, when that type exists:
    /// the type whose answers every shape question gives for `t` (module docs). `t` itself
    /// otherwise. Each type asked about is remembered for `verify`.
    #[inline]
    pub(in crate::lower) fn ph_rep(&self, t: TyId) -> TyId {
        // Most programs make no placeholder: nothing to look up, on lowering's hottest paths.
        if self.shapes.no_placeholders() || !self.mentions_ph(t) {
            return t;
        }
        self.shapes.asked.borrow_mut().insert(t);
        self.rep_of(t).unwrap_or(t)
    }

    /// [`ph_rep`](Self::ph_rep) without interning: `None` when a part of the result was never
    /// interned (then no counted-types set holds it either).
    fn rep_of(&self, t: TyId) -> Option<TyId> {
        let k = self.types.kind(t);
        if let TyKind::Param(n) = k {
            if *n >= super::SHAPE_BASE {
                return Some(self.shapes.ph[(*n - super::SHAPE_BASE) as usize].rep);
            }
        }
        if !self.mentions_ph(t) {
            return Some(t);
        }
        let r = |x: &TyId| self.rep_of(*x);
        let rs = |xs: &Vec<TyId>| xs.iter().map(r).collect::<Option<Vec<TyId>>>();
        let k = match k {
            TyKind::Adt(d, a) => TyKind::Adt(*d, rs(a)?),
            TyKind::Dyn(d, a) => TyKind::Dyn(*d, rs(a)?),
            TyKind::Tuple(a) => TyKind::Tuple(rs(a)?),
            TyKind::Array(e) => TyKind::Array(r(e)?),
            TyKind::Option(e, n) => TyKind::Option(r(e)?, *n),
            TyKind::Shared(e) => TyKind::Shared(r(e)?),
            TyKind::Map(a, b) => TyKind::Map(r(a)?, r(b)?),
            TyKind::Result(a, b) => TyKind::Result(r(a)?, r(b)?),
            TyKind::Promise(a, b) => TyKind::Promise(r(a)?, r(b)?),
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            } => TyKind::FnPtr {
                params: rs(params)?,
                ret: r(ret)?,
                throws: r(throws)?,
                rest: *rest,
            },
            _ => return Some(t),
        };
        self.types.get(&k)
    }

    /// The shape key of concrete `t`, or `None` when `t` is not shared in this step.
    pub(super) fn shape_key(&mut self, t: TyId) -> Option<Key> {
        let shareable = matches!(
            self.kind(t),
            TyKind::Int(_)
                | TyKind::Float(_)
                | TyKind::Bool
                | TyKind::Str
                | TyKind::Adt(..)
                | TyKind::Array(_)
                | TyKind::Option(..)
                | TyKind::Tuple(_)
        );
        if !shareable || self.mentions_any_param(t) || self.is_dynamic(t) {
            return None;
        }
        if matches!(self.ty(t), Ty::Unit | Ty::V128) {
            return None;
        }
        let share = self.share_kind(t) as u8;
        let half = self.half(t).map(|f| format!("{f:?}"));
        let answers = (self.footprint(t).into_iter())
            .map(|f| self.answers(f))
            .collect();
        Some(Key {
            half,
            share,
            answers,
        })
    }

    /// `t` and `T[]`, the mirror. A nested view's element (`string[]` in `string[][][]`)
    /// differs from another at this level, and so does a recursive object type holding an
    /// array of itself. The helpers also ask about `ArrayView<T>`, which is left out of the
    /// key: no program is known where it differs while `T[]` agrees, and the end-of-pass check
    /// (`shapes_close`) catches one that does.
    fn footprint(&mut self, t: TyId) -> Vec<TyId> {
        let arr = self.intern(TyKind::Array(t));
        match dropped("array") {
            true => vec![t],
            false => vec![t, arr],
        }
    }

    /// The answers to the shape questions about `t`: the yes/no queries lowering's inline code
    /// depends on, and which counted-types sets hold it.
    pub(super) fn answers(&mut self, t: TyId) -> Answers {
        let mut bits = self.bits(t);
        let sets = self
            .boxing
            .ty_sets()
            .iter()
            .map(|s| s.contains(&t))
            .collect();
        let mut lay = self.lay(t);
        if let (Some(Lay::Agg(_, a)), true) = (&mut lay, dropped("align")) {
            *a = 0;
        }
        for (row, bit) in [("niche", 1), ("counted", 4), ("weak", 8)] {
            if dropped(row) {
                bits &= !(1 << bit);
            }
        }
        Answers { lay, bits, sets }
    }

    /// The layout of `t`: aggregates by size and alignment (`None`: no value).
    fn lay(&mut self, t: TyId) -> Option<Lay> {
        match self.ty(t) {
            Ty::Agg(a) => {
                let l = &self.aggs[a.0 as usize];
                Some(Lay::Agg(l.size, l.align))
            }
            Ty::Unit => None,
            s => Some(Lay::Scalar(s)),
        }
    }

    /// The yes/no answers lowering may depend on for `t` (`Key::bits`).
    fn bits(&mut self, t: TyId) -> u32 {
        let answers = [
            self.needs_drop(t),
            self.has_null_niche(t),
            self.is_class(t),
            self.boxed(t),
            self.counted(t),
            self.proxiable(t),
            self.unique_refs(t),
            self.lock_counted(t),
            self.weak_capable(t),
            self.is_union(t),
            self.reaches_lock_set(t),
            self.shared_reads(t),
            self.copied_across(t),
            self.transfers(t),
            self.holds_mutex(t, 0, true),
            self.holds_mutex(t, 0, false),
            self.array_view(t),
        ];
        answers
            .iter()
            .enumerate()
            .fold(0, |acc, (i, &b)| acc | u32::from(b) << i)
    }

    /// Does `t` mention a type parameter (a generic leftover, or a placeholder)?
    fn mentions_any_param(&self, t: TyId) -> bool {
        let mut stack = vec![t];
        let mut seen = FxHashSet::default();
        while let Some(x) = stack.pop() {
            if !seen.insert(x) {
                continue;
            }
            match self.types.kind(x) {
                TyKind::Param(_) => return true,
                k => stack.extend(super::parts(k)),
            }
        }
        false
    }

    /// `t` with each type `from` of `map` replaced by its `to`.
    pub(in crate::lower) fn replace_tys(&mut self, t: TyId, map: &[(TyId, TyId)]) -> TyId {
        if let Some(&(_, to)) = map.iter().find(|(from, _)| *from == t) {
            return to;
        }
        let r = |cx: &mut Self, x: TyId| cx.replace_tys(x, map);
        let k = match self.types.kind(t).clone() {
            TyKind::Adt(d, a) => TyKind::Adt(d, a.iter().map(|&x| r(self, x)).collect()),
            TyKind::Dyn(d, a) => TyKind::Dyn(d, a.iter().map(|&x| r(self, x)).collect()),
            TyKind::Tuple(a) => TyKind::Tuple(a.iter().map(|&x| r(self, x)).collect()),
            TyKind::Array(e) => TyKind::Array(r(self, e)),
            TyKind::Option(e, n) => TyKind::Option(r(self, e), n),
            TyKind::Shared(e) => TyKind::Shared(r(self, e)),
            TyKind::Map(a, b) => TyKind::Map(r(self, a), r(self, b)),
            TyKind::Result(a, b) => TyKind::Result(r(self, a), r(self, b)),
            TyKind::Promise(a, b) => TyKind::Promise(r(self, a), r(self, b)),
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            } => TyKind::FnPtr {
                params: params.iter().map(|&x| r(self, x)).collect(),
                ret: r(self, ret),
                throws: r(self, throws),
                rest,
            },
            _ => return t,
        };
        self.intern(k)
    }

    /// At the end of the pass: every member of every shape gives the representative's answers
    /// for each type built from a placeholder that shared code asked about (module docs).
    pub(in crate::lower) fn shapes_close(&mut self) {
        CHECKING.with(|c| c.set(true));
        let mut derived: Vec<TyId> = self.shapes.asked.borrow().iter().copied().collect();
        // Every placeholder itself too, asked about or not.
        derived.extend(self.shapes.ph.iter().map(|p| p.ty));
        derived.sort_by_key(|t| t.0);
        derived.dedup();
        for i in 0..self.shapes.ph.len() {
            let (p, rep) = (self.shapes.ph[i].ty, self.shapes.ph[i].rep);
            let members = self.shapes.ph[i].members.clone();
            for &d in &derived {
                let rest: Vec<(TyId, TyId)> =
                    self.shapes.ph.iter().map(|q| (q.ty, q.rep)).collect();
                let want_ty = self.replace_tys(d, &rest);
                let want = self.answers(want_ty);
                for &m in &members {
                    let mut map = rest.clone();
                    map[i] = (p, m);
                    let got = self.replace_tys(d, &map);
                    let have = self.answers(got);
                    if have != want {
                        let (a, b) = (self.type_key(rep), self.type_key(m));
                        let (x, y) = (self.type_key(want_ty), self.type_key(got));
                        ice(format_args!(
                            "shape of `{a}` and `{b}` differs for a type built from them \
                             (`{x}`: {want:?}, `{y}`: {have:?}): the shape key is missing a \
                             question"
                        ));
                    }
                }
            }
        }
        CHECKING.with(|c| c.set(false));
    }
}
