//! How a value of each type is shared (`Intrinsic::Share` and lowering's implicit copies of
//! borrowed values): docs/design/semantics-stage2.md §3.1.

use velt_sema::hir::{self, AdtKind, TyId, TyKind};

use crate::lower::Cx;

/// The sharing behaviour of a type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::lower) enum ShareKind {
    /// Owns nothing: a share is a bitwise copy (numbers, bool, Copy structs, literal types,
    /// C-like enums).
    Plain,
    /// `string`: `velt_rt_str_clone` (stage 1).
    Str,
    /// A reference type (class instance, array, object type whose fields are assigned or that
    /// has `dispose` or a `Mutex`): every share is the same counted object.
    Object,
    /// A value type owning parts (`T | null`, unions, tuples, object types never changed in
    /// place): a copy that shares each part.
    Value,
    /// Function values: the copy shares the environment.
    Closure,
    /// Interface values.
    Dyn,
    /// `shared<T>`: its own atomic count.
    Shared,
    /// Promise values have one owner (sema never shares them).
    Promise,
}

impl Cx<'_> {
    /// How values of the concrete type `t` are shared.
    pub(in crate::lower) fn share_kind(&mut self, t: TyId) -> ShareKind {
        if self.boxed(t) {
            return ShareKind::Object;
        }
        match self.kind(t) {
            TyKind::Str => ShareKind::Str,
            TyKind::Array(_) => ShareKind::Object,
            TyKind::FnPtr { .. } | TyKind::Closure(_) => ShareKind::Closure,
            TyKind::Dyn(..) => ShareKind::Dyn,
            TyKind::Shared(_) => ShareKind::Shared,
            TyKind::Promise(..) => ShareKind::Promise,
            TyKind::Adt(d, _) => match self.hir.def(d) {
                hir::Def::Adt(a) if a.kind == AdtKind::Class => ShareKind::Object,
                hir::Def::Adt(a) if a.is_copy => ShareKind::Plain,
                hir::Def::Adt(a) if a.assigned || a.dispose.is_some() => ShareKind::Object,
                hir::Def::Adt(_) if self.holds_mutex(t, 0) => ShareKind::Object,
                _ => self.value_kind(t),
            },
            TyKind::Option(_) | TyKind::Tuple(_) | TyKind::Result(..) => self.value_kind(t),
            _ => ShareKind::Plain,
        }
    }

    /// Can a value of `t` be shared without counting another type? A share of an uncounted
    /// reference type, or of a value holding one, asks the next pass to count it (`close_share`).
    /// Interface values qualify: every vtable's share entry is built whether or not it is used.
    pub(in crate::lower) fn shares_as_counted(&mut self, t: TyId) -> bool {
        match self.share_kind(t) {
            ShareKind::Object => self.counted(t),
            ShareKind::Value => {
                let parts = self.part_types(t);
                parts.into_iter().all(|p| self.shares_as_counted(p))
            }
            ShareKind::Promise => false,
            ShareKind::Plain | ShareKind::Str | ShareKind::Closure => true,
            ShareKind::Dyn | ShareKind::Shared => true,
        }
    }

    /// The value types inside a `t` (stored inline, `t` itself included) that can be changed in
    /// place and that a share would copy: tuples, and object types stored inline whose parts
    /// include such a type. Strings, plain values, counted objects, function and interface
    /// values have none; options, results and unions have their payloads'.
    pub(in crate::lower) fn in_place_parts(&mut self, t: TyId) -> Vec<TyId> {
        let mut out = vec![];
        self.collect_in_place(t, &mut out, &mut Vec::new());
        out
    }

    fn collect_in_place(&mut self, t: TyId, out: &mut Vec<TyId>, seen: &mut Vec<TyId>) {
        if seen.contains(&t) || self.counted(t) {
            return;
        }
        seen.push(t);
        match self.kind(t) {
            TyKind::Tuple(_) => out.push(t),
            TyKind::Option(x) => self.collect_in_place(x, out, seen),
            TyKind::Result(a, b) => {
                self.collect_in_place(a, out, seen);
                self.collect_in_place(b, out, seen);
            }
            TyKind::Adt(d, _) => match self.hir.def(d) {
                hir::Def::Enum(_) => {
                    for p in self.part_types(t) {
                        self.collect_in_place(p, out, seen);
                    }
                }
                hir::Def::Adt(a) if a.kind != AdtKind::Class && !a.is_copy => {
                    let mut inner = vec![];
                    for p in self.part_types(t) {
                        self.collect_in_place(p, &mut inner, seen);
                    }
                    if !inner.is_empty() {
                        out.push(t);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// A value type: `Value` when some part owns resources, else `Plain`.
    fn value_kind(&mut self, t: TyId) -> ShareKind {
        if self.needs_drop(t) {
            ShareKind::Value
        } else {
            ShareKind::Plain
        }
    }
}
