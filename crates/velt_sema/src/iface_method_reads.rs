//! A method of an interface read as a value (`const t = ctx.update`) where the program also
//! writes that method through interface values (`crate::iface_method_writes`).
//!
//! A method value is a function bound to the receiver that calls the method through its slot
//! when it is called (`body/expr/method_value.rs`). Once the method is written, that call
//! reaches the function stored at the time of the call. In JavaScript the read takes the
//! function stored at the time of the read, so a later write must not change what the value
//! calls (`t = a.update; a.update = b.update; b.update = t` would otherwise call itself
//! forever). Such a read is refused, with the arrow function that does what the bound value
//! did. Reads are recorded where they are checked and reported once every write is known, so a
//! read before the write in the source is refused too.

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::DefId;

impl Ctx<'_> {
    /// Method `j` of interface `iface` is read as a value at `span` through a value of the
    /// interface (or of a type parameter bounded by it).
    pub(crate) fn note_method_value_read(&mut self, iface: DefId, j: usize, span: Span) {
        let entry = (iface, j, span);
        if !self.method_writes.value_reads.contains(&entry) {
            self.method_writes.value_reads.push(entry);
        }
    }

    /// The written method a read of method `j` of `iface` as a value would see change: one of
    /// that name of `iface`, of an interface it extends, or of one extending it (whose writes
    /// reach the same field). With where it is first written.
    fn written_counterpart(&self, iface: DefId, j: usize) -> Option<(DefId, Span)> {
        let name = self.iface(iface)?.methods.get(j)?.name.clone();
        let mut found: Option<(DefId, Span)> = None;
        for ((w, k), at) in self.written_method_pairs() {
            let same = self
                .iface(w)
                .and_then(|i| i.methods.get(k))
                .is_some_and(|m| m.name == name);
            let related =
                self.extends_or_is_iface(iface, w, 0) || self.extends_or_is_iface(w, iface, 0);
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
