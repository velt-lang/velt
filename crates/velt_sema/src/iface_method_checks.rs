//! The checks of methods written through interface values (`crate::iface_method_writes`) made
//! once every write is known.
//!
//! **Fields that are not sites.** An implementation may store a written method in a field whose
//! function type has its own `throws` clause (or is written in std), so what it throws is not
//! inferred: what the functions assigned to the method throw, or reject their promises with,
//! must fit it, or the write is refused.
//!
//! **A written method read as a value** (`const t = ctx.update`). A method value is a function
//! bound to the receiver that calls the method through its slot when it is called
//! (`body/expr/method_value.rs`). Once the method is written, that call reaches the function
//! stored at the time of the call. In JavaScript the read takes the function stored at the time
//! of the read, so a later write must not change what the value calls (`t = a.update; a.update =
//! b.update; b.update = t` would otherwise call itself forever). Such a read is refused, with the
//! arrow function that does what the bound value did. Reads are recorded where they are checked
//! and reported here, so a read before the write in the source is refused too.

use rustc_hash::FxHashSet;
use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{DefId, TyId};

impl Ctx<'_> {
    /// The fields storing a written method that are not sites: what the functions assigned to
    /// the method throw (or, returning a promise, reject it with) must fit the field's function
    /// type, or the write is refused.
    pub(crate) fn check_unlinked_fields(&mut self) {
        let unlinked = std::mem::take(&mut self.method_writes.unlinked);
        for &(m, d, index, fty) in &unlinked {
            let crate::hir::TyKind::FnPtr { throws, ret, .. } = *self.ty.kind(fty) else {
                continue;
            };
            // A promise method's site holds what its promises reject with.
            let (allowed, verb) = match self.promise_rejection_error(ret) {
                Some(e) => (e, "rejects with"),
                None => (throws, "throws"),
            };
            let thrown = self.site_errors(m);
            let Some(bad) = self.error_outside(Some(allowed), thrown) else {
                continue;
            };
            let at = (self.method_writes.sites.iter())
                .find(|(_, &(s, _))| s == m)
                .and_then(|(key, _)| self.method_writes.methods.get(key).copied());
            let Some(at) = at else {
                continue;
            };
            let Some(a) = self.adt(d) else {
                continue;
            };
            let owner = a.name.clone();
            let field = (a.fields.get(index)).map_or(String::new(), |f| f.name.clone());
            let (bad, fty) = (self.display(bad), self.display(fty));
            self.error(
                Diagnostic::error(
                    format!("a function assigned to method `{field}` {verb} `{bad}`, which a field storing it does not allow"),
                    at,
                )
                .with_note(format!("`{owner}` stores it in its field `{field}` of type `{fty}`"))
                .with_note(format!("catch the error inside the assigned function, or let that field's function type allow `{bad}`")),
            );
        }
        self.method_writes.unlinked = unlinked;
    }

    /// Method `j` of interface `iface` is read as a value at `span` through a value of the
    /// interface (or of a type parameter bounded by it).
    pub(crate) fn note_method_value_read(&mut self, iface: DefId, j: usize, span: Span) {
        let entry = (iface, j, span);
        if !self.method_writes.value_reads.contains(&entry) {
            self.method_writes.value_reads.push(entry);
        }
    }

    /// The types implementing `iface`.
    fn implementing_types(&self, iface: DefId) -> FxHashSet<TyId> {
        (self.impls.iter())
            .filter(|m| m.iface == iface)
            .map(|m| m.ty)
            .collect()
    }

    /// The written method a read of method `j` of `iface` as a value would see change: one of
    /// that name of `iface`, of an interface it extends or that extends it, or of another
    /// interface implemented by a type that implements `iface` too (one object held as both, so
    /// both slots call the field the write replaces). With where it is first written.
    fn written_counterpart(&self, iface: DefId, j: usize) -> Option<(DefId, Span)> {
        let name = self.iface(iface)?.methods.get(j)?.name.clone();
        let mine = self.implementing_types(iface);
        let mut found: Option<(DefId, Span)> = None;
        for ((w, k), at) in self.written_method_pairs() {
            let same = self
                .iface(w)
                .and_then(|i| i.methods.get(k))
                .is_some_and(|m| m.name == name);
            let related = self.extends_or_is_iface(iface, w, 0)
                || self.extends_or_is_iface(w, iface, 0)
                || !self.implementing_types(w).is_disjoint(&mine);
            if same && related && found.is_none_or(|(_, f)| (at.file, at.lo) < (f.file, f.lo)) {
                found = Some((w, at));
            }
        }
        found
    }

    /// Report each recorded read of a written method as a value (once every body is checked).
    pub(crate) fn report_written_method_reads(&mut self) {
        let reads = std::mem::take(&mut self.method_writes.value_reads);
        for &(iface, j, span) in &reads {
            let Some((w, at)) = self.written_counterpart(iface, j) else {
                continue;
            };
            let Some(name) =
                (self.iface(iface).and_then(|i| i.methods.get(j))).map(|m| m.name.clone())
            else {
                continue;
            };
            let wname = self.iface_name(w);
            let diag = Diagnostic::error(
                format!("method `{name}` is read as a value, and the program assigns to it through `{wname}`"),
                span,
            )
            .with_note(format!(
                "a value read from `{name}` would call the function assigned to it later, not the one stored when it is read"
            ))
            .with_label(at, format!("`{name}` is assigned here"))
            .with_note(format!(
                "use an arrow function that calls `.{name}()` when it runs, or keep the function in a variable of its own before assigning it to `{name}`"
            ));
            self.error(diag);
        }
        self.method_writes.value_reads = reads;
    }
}
