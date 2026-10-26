//! Closures each evaluation of which is a new function value where JavaScript has one function
//! (`hir::Program::fresh_closures`, #943): a generic function value converted to another
//! function type, a function of fixed types asserted to a generic one, and a generic function's
//! value at a type over the caller's type parameters or at more than one type. Comparing such a
//! value would give a different answer than JavaScript, so a program that makes one and compares
//! function values of its type (`===`, `includes`, `indexOf`, a `Set` or `Map` key, through
//! generic code too) is reported, as are `unknown` values compared while one is held as one.
//!
//! The types are told apart as instantiated, without what they throw: a function type of fixed
//! types by its parameter and result types, a generic function type by its interface and that
//! interface's arguments. A comparison counts where it is lowered in a function (`===`, a `Set`
//! instance at the type), not in the glue built for a type (an `any` table's equality entry is
//! there whether or not anything is compared: comparing `any` values is `any_identity`).

use velt_common::Diagnostic;
use velt_sema::hir::{self, DefId, FreshClosure, TyId, TyKind};

use super::{Cx, FnLower};

/// What [`Cx::fn_key`] tells function types apart by.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum FnKey {
    Fixed(Vec<TyId>, TyId, bool),
    Generic(DefId, Vec<TyId>),
}

impl Cx<'_> {
    /// The key of a function type, or of a generic function value's interface type (`None` for
    /// a generic closure's own type: its values are those of its interface type).
    pub(super) fn fn_key(&self, ty: TyId) -> Option<FnKey> {
        match self.kind(ty) {
            TyKind::FnPtr {
                params, ret, rest, ..
            } => Some(FnKey::Fixed(params, ret, rest)),
            TyKind::Closure(_) if self.hir.impls.iter().any(|i| i.ty == ty) => None,
            TyKind::Closure(d) => {
                let f = self.fn_def(d);
                let params = f.params.iter().map(|p| p.ty).collect();
                Some(FnKey::Fixed(params, f.ret, false))
            }
            TyKind::Dyn(d, args) if self.is_generic_fn_value(ty) => Some(FnKey::Generic(d, args)),
            _ => None,
        }
    }

    /// `t` with what its function types throw and its promises reject with left out (`never`),
    /// error-set sites among them, which sema reduces only per site: a key's part.
    fn without_errors(&mut self, t: TyId, depth: u32) -> TyId {
        if depth > 16 {
            return t;
        }
        let w = |cx: &mut Self, x: TyId| cx.without_errors(x, depth + 1);
        let never = self.intern(TyKind::Never);
        let k = match self.kind(t) {
            TyKind::Op(..) => return never,
            TyKind::FnPtr {
                params, ret, rest, ..
            } => TyKind::FnPtr {
                params: params.iter().map(|&p| w(self, p)).collect(),
                ret: w(self, ret),
                throws: never,
                rest,
            },
            TyKind::Promise(v, _) => TyKind::Promise(w(self, v), never),
            TyKind::Adt(d, args) => TyKind::Adt(d, args.iter().map(|&a| w(self, a)).collect()),
            TyKind::Dyn(d, args) => TyKind::Dyn(d, args.iter().map(|&a| w(self, a)).collect()),
            TyKind::Array(e) => TyKind::Array(w(self, e)),
            TyKind::Map(k, v) => TyKind::Map(w(self, k), w(self, v)),
            TyKind::Tuple(es) => TyKind::Tuple(es.iter().map(|&a| w(self, a)).collect()),
            TyKind::Option(e, n) => TyKind::Option(w(self, e), n),
            TyKind::Result(a, b) => TyKind::Result(w(self, a), w(self, b)),
            TyKind::Shared(e) => TyKind::Shared(w(self, e)),
            _ => return t,
        };
        self.intern(k)
    }

    /// The keys of the function values `ty` holds as a member ([`FnLower::note_fn_compared_in`]).
    fn fn_keys_in(&mut self, ty: TyId, depth: u32, out: &mut Vec<FnKey>) {
        if depth > 8 {
            return;
        }
        if let Some(k) = self.fn_key(ty) {
            out.push(k);
            return;
        }
        let payloads: Vec<TyId> = match self.kind(ty) {
            TyKind::Option(x, _) => vec![x],
            TyKind::Result(a, b) => vec![a, b],
            TyKind::Tuple(ts) => ts,
            TyKind::Adt(d, args) => match self.hir.def(d) {
                // A declared struct compares (and hashes) field by field as a key (same.rs).
                hir::Def::Adt(a) if a.declared_struct => {
                    let fs: Vec<TyId> = a.fields.iter().map(|f| f.ty).collect();
                    fs.into_iter().map(|f| self.subst(f, &args)).collect()
                }
                hir::Def::Enum(e) => {
                    let ps: Vec<TyId> = e.variants.iter().flat_map(|v| v.payload.clone()).collect();
                    ps.into_iter().map(|p| self.subst(p, &args)).collect()
                }
                _ => vec![],
            },
            _ => vec![],
        };
        for t in payloads {
            self.fn_keys_in(t, depth + 1, out);
        }
    }

    /// Errors for the fresh closures made whose values may be compared (module docs).
    pub(super) fn fresh_fn_diagnostics(&self) -> Vec<Diagnostic> {
        let mut made: Vec<(DefId, FnKey)> = self.facts.fresh_made.iter().cloned().collect();
        made.sort_unstable_by_key(|(d, _)| d.0);
        let mut out = vec![];
        let mut spans = rustc_hash::FxHashSet::default();
        for (d, key) in made {
            // Its own type and those it is converted to (`FreshClosure::Converts`).
            let mut keys = vec![key];
            let mut i = 0;
            while let Some(k) = keys.get(i).cloned() {
                i += 1;
                for (a, b) in &self.facts.fn_edges {
                    if *a == k && !keys.contains(b) {
                        keys.push(b.clone());
                    }
                }
            }
            let typed = keys.iter().any(|k| self.facts.fn_compared_keys.contains(k));
            let held = |k: &FnKey| {
                self.facts
                    .dyn_types
                    .iter()
                    .any(|t| self.fn_key(*t).as_ref() == Some(k))
                    || self.facts.generic_any_keys.contains(k)
            };
            let dynamic = self.facts.any_identity && keys.iter().any(held);
            if !typed && !dynamic {
                continue;
            }
            let span = match self.hir.def(d) {
                hir::Def::Fn(f) => f.span,
                _ => continue,
            };
            // A function's values at several types are reported once, at the function.
            if !spans.insert(span) {
                continue;
            }
            let how = match typed {
                true => "the program compares function values of its type (`===`, `includes`, `indexOf`, a `Set` or `Map` key)",
                false => "the program compares `unknown` or `any` values while one holds it",
            };
            out.push(
                Diagnostic::error(
                    "this function value can't be compared yet: each evaluation makes a new function, where JavaScript keeps one (#943)",
                    span,
                )
                .with_note(how)
                .with_note("a generic function converted to another function type, a function asserted to a generic type, and a generic function used as a value at more than one type, or at a type over type parameters, are wrapped at each use; compare the functions themselves, or keep one value in a variable"),
            );
        }
        out
    }
}

impl FnLower<'_, '_> {
    /// Record that function values of type `ty` are compared (or hashed) here.
    pub(super) fn note_fn_compared(&mut self, ty: TyId) {
        self.cx.note_fn_identity();
        if self.glue {
            return;
        }
        if let Some(k) = self.cx.fn_key(ty) {
            self.cx.facts.fn_compared_keys.insert(k);
        }
    }

    /// Record that closure `def` is made here: a fresh one at its instantiated type, or a
    /// conversion of function values from one type to another (`FreshClosure`).
    pub(super) fn note_closure_made(&mut self, def: DefId) {
        match self.cx.hir.fresh_closures.get(&def) {
            Some(&FreshClosure::New(t)) => {
                if let Some(key) = self.instantiated_key(t) {
                    self.cx.facts.fresh_made.insert((def, key));
                }
            }
            Some(&FreshClosure::Converts { from, to }) => {
                let (a, b) = (self.instantiated_key(from), self.instantiated_key(to));
                if let (Some(a), Some(b)) = (a, b) {
                    if a != b {
                        self.cx.facts.fn_edges.insert((a, b));
                    }
                }
            }
            None => {}
        }
    }

    /// The key of function type `t` instantiated here, without what it throws (an error-set
    /// site sema reduces only per site).
    fn instantiated_key(&mut self, t: TyId) -> Option<FnKey> {
        let t = self.cx.without_errors(t, 0);
        Some(match self.cx.kind(t) {
            TyKind::FnPtr {
                params, ret, rest, ..
            } => FnKey::Fixed(
                params.iter().map(|p| self.sub(*p)).collect(),
                self.sub(ret),
                rest,
            ),
            TyKind::Dyn(d, args) => FnKey::Generic(d, args.iter().map(|a| self.sub(*a)).collect()),
            _ => return None,
        })
    }

    /// Record that values of type `ty` are compared (or hashed) through glue here: the function
    /// values it holds, also inside a union, an option, a tuple, a result or a declared struct
    /// (which the glue compares member by member). Nothing for a type holding none, so closures keep a frame
    /// environment in a program comparing only other values.
    pub(super) fn note_fn_compared_in(&mut self, ty: TyId) {
        let mut keys = vec![];
        self.cx.fn_keys_in(ty, 0, &mut keys);
        if keys.is_empty() {
            return;
        }
        self.cx.note_fn_identity();
        if !self.glue {
            self.cx.facts.fn_compared_keys.extend(keys);
        }
    }
}
