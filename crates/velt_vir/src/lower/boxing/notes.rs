//! The facts lowering records while it builds functions (`Cx::note_*`), which
//! [`Cx::close_boxing`](super) turns into the next [`super::Boxing`].

use velt_sema::hir::{DefId, TyId, TyKind};

use crate::lower::Cx;

impl Cx<'_> {
    /// Record that the program makes or calls a function in `shared(...)`.
    pub(in crate::lower) fn note_shared_fn(&mut self) {
        self.facts.shared_fns = true;
    }

    /// Record that a value of type `t` is shared.
    pub(in crate::lower) fn note_share(&mut self, t: TyId) {
        self.facts.shares.insert(t);
        // A shared body's are replayed with the site of the code its dictionary is for (shape/).
        if !self.mentions_ph(t) {
            self.note_share_site(t);
        }
    }

    /// The user code sharing `t` now, if it is the first (`counted_atomic_diagnostics`).
    pub(in crate::lower) fn note_share_site(&mut self, t: TyId) {
        if self.user_site != velt_common::Span::DUMMY {
            self.share_sites.entry(t).or_insert(self.user_site);
        }
    }

    /// Record that values of the object type `t` are compared by identity.
    pub(in crate::lower) fn note_identity(&mut self, t: TyId) {
        // A C struct and a `struct` value (#641) have no identity (same.rs compares their
        // content); a C struct keeps C's layout.
        if self.c_struct(t) || self.value_struct(t) {
            return;
        }
        self.facts.identity.insert(t);
    }

    /// Record that a value of type `t` (one of `in_place_parts`) may be changed in place by a
    /// callee while it is borrowed out of an array other references reach.
    pub(in crate::lower) fn note_identity_borrow(&mut self, t: TyId) {
        if self.c_struct(t) || self.value_struct(t) {
            return;
        }
        self.facts.identity_borrows.insert(t);
    }

    /// Record an interface upcast of a `from` value to `to` (upcast.rs).
    pub(in crate::lower) fn note_upcast(&mut self, from: TyId, to: TyId) {
        self.facts.upcasts.insert((from, to));
    }

    /// Record that values of the interface `iface` are converted to `any` / `unknown`.
    pub(in crate::lower) fn note_dyn_to_any(&mut self, iface: DefId) {
        self.facts.dyn_to_any.insert(iface);
    }

    /// Record that `any` values are compared by identity (see [`Boxing::any_identity`]).
    pub(in crate::lower) fn note_any_identity(&mut self) {
        self.facts.any_identity = true;
    }

    /// Record that function values are compared by identity.
    pub(in crate::lower) fn note_fn_identity(&mut self) {
        self.facts.fn_compared = true;
    }

    /// Record that a function wrapper is made (`Boxing::fn_wrappers`).
    pub(in crate::lower) fn note_fn_wrapper(&mut self) {
        self.facts.fn_wrapper = true;
    }

    /// Record that a member is written through `any`.
    /// A member slot is called in a program sema gave none (`Boxing::any_members`).
    pub(in crate::lower) fn note_any_member_call(&mut self) {
        self.facts.any_member_call = true;
    }

    pub(in crate::lower) fn note_any_set(&mut self) {
        self.facts.any_set = true;
    }

    /// Record that `key in x` is tested on an `any`.
    pub(in crate::lower) fn note_any_has(&mut self) {
        self.facts.any_has = true;
    }

    /// Record that a property of an `any` is deleted.
    pub(in crate::lower) fn note_any_delete(&mut self) {
        self.facts.any_delete = true;
    }

    /// Record that `Object.keys` of an `any` is listed.
    pub(in crate::lower) fn note_any_keys(&mut self) {
        self.facts.any_keys = true;
    }

    /// Record that an `any` is reflected on (velt/reflect).
    pub(in crate::lower) fn note_any_reflect(&mut self) {
        self.facts.any_reflect = true;
    }

    /// Record that a method named `name` (`""`: any name) is called through `any`.
    pub(in crate::lower) fn note_any_call_key(&mut self, name: &str) {
        if !self.facts.any_call_keys.contains(name) {
            self.facts.any_call_keys.insert(name.to_string());
        }
    }

    /// Record that an `any` is converted to a number.
    pub(in crate::lower) fn note_any_number(&mut self) {
        self.facts.any_number = true;
    }

    /// Do closures need an identity of their own (see [`Boxing::fn_identity`])?
    pub(in crate::lower) fn fn_identity(&self) -> bool {
        self.boxing.fn_identity
    }

    /// The interface value type `dyn_ty` is compared by identity, and its implementor `t` is an
    /// object type that a conversion would copy: record that `t` must be counted.
    pub(in crate::lower) fn note_dyn_identity(&mut self, dyn_ty: TyId, t: TyId) {
        // A C struct and a `struct` value (#641) have no identity: an `object`, `any` or
        // interface value holds a copy.
        if self.c_struct(t) || self.value_struct(t) {
            return;
        }
        // Once values convert to `object` / `Function` (issue #926), which key collections by
        // identity, every dynamic type holds objects by identity: a value may reach `object`
        // through `any` or `unknown` (`u as object`).
        let identity = self.boxing.identity_dyns.contains(&dyn_ty)
            || (self.is_dynamic(dyn_ty)
                && (self.boxing.any_identity
                    || self
                        .boxing
                        .identity_dyns
                        .iter()
                        .any(|d| self.is_narrow_dynamic(*d))));
        if identity && self.copied_object(t) {
            self.facts.identity.insert(t);
            self.facts.shares.insert(t);
        }
    }

    /// Record a borrow of a `value`-typed place reached through a `container`-typed value.
    pub(in crate::lower) fn note_projection(&mut self, container: TyId, value: TyId) {
        self.facts.projections.insert((container, value));
        self.note_share_site(value);
    }

    /// Does a value of `t` carry a reference count?
    pub(in crate::lower) fn counted(&self, t: TyId) -> bool {
        self.counted_in(&self.boxing, t)
    }

    /// Are references to values of `t` provably unique while a call borrows them (no other
    /// owner can reach the value: it is neither counted nor borrowed inside a counted object)?
    /// Only then may a param of type `t` be `noalias` (vir.rs invariant 9).
    pub(in crate::lower) fn unique_refs(&self, t: TyId) -> bool {
        let t = self.ph_rep(t);
        !self.counted(t) && !self.boxing.interior.contains(&t)
    }

    /// Errors for the `Atomic<T>` types something shares: generic code instantiated with one
    /// (`xs.slice()` of an `Atomic<u64>[]`, a `Map` of them), which sema's check of shares
    /// (`atomic_copies`) cannot see. A counted `Atomic` would be a box, not the word in place
    /// every program's atomics rely on (docs/internals/design/atomics.md). Each is reported at
    /// the user code that first shared it, or the value holding it (`note_share_site`): the
    /// call of the generic code, never the standard library's code itself.
    pub(in crate::lower) fn counted_atomic_diagnostics(&self) -> Vec<velt_common::Diagnostic> {
        let mut found: Vec<(velt_common::Span, String)> = (self.boxing.boxes.iter())
            .filter(|&&t| matches!(self.kind(t), TyKind::Adt(d, _) if crate::lower::atomic::is_atomic(self, d)))
            .map(|&t| {
                let from = self.share_from.get(&t).copied().unwrap_or(t);
                let site = (self.share_sites.get(&from))
                    .or_else(|| self.share_sites.get(&t))
                    .copied()
                    .unwrap_or_default();
                (site, self.atomic_name(t))
            })
            .collect();
        found.sort_by(|a, b| (a.0.file, a.0.lo, &a.1).cmp(&(b.0.file, b.0.lo, &b.1)));
        found.into_iter().map(|(site, n)| {
            velt_common::Diagnostic::error(
                format!("an `{n}` cannot be copied or shared, but the generic code called here shares one"),
                site,
            )
            .with_note("a generic function or method that shares its values (`slice`, `filter`, `concat`, a `Map` of them) was called with `Atomic`s; keep them in place and read them with `.load()`")
        }).collect()
    }

    /// A `Mutex` or an `RwLock` the program shares by value (`const q = m`, a copy for a generic
    /// function), which would make it a second lock. Each is reported at the user code that first
    /// shared it, or the value holding it.
    pub(in crate::lower) fn counted_lock_diagnostics(&self) -> Vec<velt_common::Diagnostic> {
        let mut found: Vec<(velt_common::Span, String)> = (self.shared_locks.iter())
            .map(|&t| {
                let from = self.share_from.get(&t).copied().unwrap_or(t);
                let site = (self.share_sites.get(&from))
                    .or_else(|| self.share_sites.get(&t))
                    .copied()
                    .unwrap_or_default();
                (site, self.type_name(t))
            })
            .collect();
        found.sort_by(|a, b| (a.0.file, a.0.lo, &a.1).cmp(&(b.0.file, b.0.lo, &b.1)));
        found.dedup();
        found
            .into_iter()
            .map(|(site, n)| {
                velt_common::Diagnostic::error(
                    format!("this shares a `{n}` held by value, which would make it a second lock"),
                    site,
                )
                .with_note(format!(
                    "use the variable holding it in place, or share the lock itself: `shared(new {n}(…))`"
                ))
            })
            .collect()
    }

    /// `Atomic<u64>` for the type `t` of an `Atomic`.
    fn atomic_name(&self, t: TyId) -> String {
        let arg = match self.kind(t) {
            TyKind::Adt(_, args) => args.first().map(|&a| match self.kind(a) {
                TyKind::Int(i) => format!("{i:?}").to_lowercase(),
                TyKind::Bool => "bool".to_string(),
                _ => self.type_name(a),
            }),
            _ => None,
        };
        match arg {
            Some(a) => format!("Atomic<{a}>"),
            None => self.type_name(t),
        }
    }

    /// Display names of the counted types (`VELT_DEBUG_COUNTED=1`, bench/rc_stats.ps1).
    pub(in crate::lower) fn counted_names(&self) -> Vec<String> {
        let roots = self.boxing.roots.iter().map(|d| match self.hir.def(*d) {
            velt_sema::hir::Def::Adt(a) => format!("class {}", a.name),
            _ => "?".into(),
        });
        let boxes = self.boxing.boxes.iter().map(|&t| self.type_name(t));
        let recursive = self
            .boxing
            .recursive
            .iter()
            .map(|d| match self.hir.def(*d) {
                velt_sema::hir::Def::Adt(a) => format!("{} (recursive)", a.name),
                _ => "?".into(),
            });
        let mut out: Vec<String> = roots.chain(boxes).chain(recursive).collect();
        out.sort();
        out
    }
}
