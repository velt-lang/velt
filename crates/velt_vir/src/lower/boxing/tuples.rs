//! Tuples that are changed in place (#1423). A tuple is a value type stored inline: a copy of
//! one is a new tuple, where JavaScript has a second reference to the same array, so after
//! `const t = tp; t[0] = 5`, `tp[0]` must be `5`. A tuple type the program both writes in place
//! (`t[i] = v`, `t[i] += v`) and copies to a second name that keeps it (a variable, a binding
//! of a pattern, an element, a field, a capture, a returned value, a parameter the callee
//! changes) is counted, as an object type whose fields are assigned is (`ShareKind::Object`):
//! every copy is then the same counted object. A tuple type that is never written, or never
//! copied, cannot tell a copy from the original and stays inline, so tuples used as values
//! (`[number, number]` pairs returned and destructured) cost what they did.
//!
//! A copy only read where it is made is not a second name: the value a destructuring pattern
//! takes apart (its bindings are recorded one by one), and the argument of a `Copy` parameter
//! the callee only reads. Such a parameter is a snapshot of the caller's tuple, which differs
//! from it only when the tuple is written while the callee runs: it is recorded when the callee
//! (or anything it calls, as far as the program's calls are known) writes a tuple of its type in
//! place, calls a function value or a runtime function that may call back, or suspends. A
//! closure's copy of a captured tuple is a second name, read or not: it outlives the call.

use rustc_hash::FxHashSet;

use velt_sema::hir::{self, AdtKind, PassMode, TyId, TyKind};

use super::{Boxing, ShareKind};
use crate::lower::{Cx, FnLower};
use crate::vir::{Callee, ExternFn, FuncId, Terminator, Ty};

/// Runtime functions that never call back into the program, and families of them.
const LEAF_RT: &[&str] = &[
    "velt_rt_alloc",
    "velt_rt_realloc",
    "velt_rt_free",
    "velt_rt_panic",
];
const LEAF_RT_FAMILIES: &[&str] = &["velt_rt_write_", "velt_rt_str", "velt_rt_math_"];

/// Can a call of runtime function `e` run code of the program? Only through a pointer to it,
/// or by running drop glue queued earlier.
fn calls_back(e: &ExternFn) -> bool {
    let s = e.symbol.as_str();
    let leaf = LEAF_RT.contains(&s)
        || LEAF_RT_FAMILIES.iter().any(|p| s.starts_with(p)) && !s.contains("drop");
    !leaf && (e.params.contains(&Ty::Ptr) || s.contains("drop") || s.contains("drain"))
}

/// What the functions a callee reaches write in place (`close_tuples`).
enum Reach {
    /// Code that cannot be followed (a function value, a runtime callback): anything.
    Unknown,
    Writes(FxHashSet<TyId>),
}

impl Cx<'_> {
    /// Record that a tuple of type `t` (any type: only tuples count) has an element assigned.
    pub(in crate::lower) fn note_tuple_write(&mut self, t: TyId) {
        if matches!(self.kind(t), TyKind::Tuple(_)) {
            self.facts.tuple_writes.insert(t);
            self.cur_tuple_writes.push(t);
        }
    }

    /// Record that a value of type `t` is copied to a second name that keeps it.
    pub(in crate::lower) fn note_tuple_copy(&mut self, t: TyId) {
        self.facts.tuple_copies.insert(t);
    }

    /// Start recording the tuple facts of a function about to be built; returns those of the
    /// one being built around it (`finish_fn_tuples`).
    pub(in crate::lower) fn start_fn_tuples(&mut self) -> (Vec<TyId>, Vec<TyId>) {
        (
            std::mem::take(&mut self.cur_tuple_writes),
            std::mem::take(&mut self.cur_read_params),
        )
    }

    /// Keep what function `fid` writes in place and which parameters it only reads.
    pub(in crate::lower) fn finish_fn_tuples(
        &mut self,
        fid: FuncId,
        outer: (Vec<TyId>, Vec<TyId>),
    ) {
        let (writes, reads) = outer;
        let w = std::mem::replace(&mut self.cur_tuple_writes, writes);
        let r = std::mem::replace(&mut self.cur_read_params, reads);
        if !w.is_empty() {
            self.facts.fn_tuple_writes.insert(fid, w);
        }
        if !r.is_empty() {
            self.facts.fn_read_params.insert(fid, r);
        }
    }

    /// Count each tuple type written in place that a copy of a value copies (module docs).
    pub(super) fn close_tuples(&mut self, next: &mut Boxing) {
        if self.facts.tuple_writes.is_empty() {
            return;
        }
        self.close_read_params();
        let copies: Vec<TyId> = self.facts.tuple_copies.iter().copied().collect();
        let mut seen = FxHashSet::default();
        let mut found = vec![];
        for t in copies {
            self.copied_tuples(t, &mut found, &mut seen);
        }
        for t in found {
            if self.facts.tuple_writes.contains(&t) {
                next.boxes.insert(t);
            }
        }
    }

    /// Record as copies the read-only `Copy` parameters whose tuples the function or its
    /// callees may write in place while it holds them (module docs).
    fn close_read_params(&mut self) {
        let mut fns: Vec<(FuncId, Vec<TyId>)> = std::mem::take(&mut self.facts.fn_read_params)
            .into_iter()
            .collect();
        fns.sort_by_key(|f| f.0 .0);
        for (fid, tys) in fns {
            // Only tuple types written somewhere can tell a snapshot from the original.
            let mut wanted = vec![];
            for t in tys {
                let mut found = vec![];
                self.copied_tuples(t, &mut found, &mut FxHashSet::default());
                if found.iter().any(|t| self.facts.tuple_writes.contains(t)) {
                    wanted.push((t, found));
                }
            }
            if wanted.is_empty() {
                continue;
            }
            let reach = self.callee_writes(fid);
            for (t, found) in wanted {
                let written = match &reach {
                    Reach::Unknown => true,
                    Reach::Writes(w) => found.iter().any(|t| w.contains(t)),
                };
                if written {
                    self.facts.tuple_copies.insert(t);
                }
            }
        }
    }

    /// The tuple types function `fid` and the functions it calls write in place, as far as
    /// calls are followed.
    fn callee_writes(&self, fid: FuncId) -> Reach {
        let mut writes = FxHashSet::default();
        let mut seen = FxHashSet::default();
        let mut work = vec![fid];
        let callees = |f: FuncId, work: &mut Vec<FuncId>| -> bool {
            let Some(func) = &self.funcs[f.0 as usize] else {
                return false;
            };
            for b in &func.blocks {
                if let Terminator::Call { callee, .. } = &b.term {
                    match callee {
                        Callee::Func(g) => work.push(*g),
                        Callee::Extern(e) if !calls_back(&self.externs[e.0 as usize]) => {}
                        _ => return false,
                    }
                }
            }
            true
        };
        while let Some(g) = work.pop() {
            if !seen.insert(g) {
                continue;
            }
            if let Some(w) = self.facts.fn_tuple_writes.get(&g) {
                writes.extend(w.iter().copied());
            }
            if !callees(g, &mut work) {
                return Reach::Unknown;
            }
        }
        Reach::Writes(writes)
    }

    /// The tuple types a copy of a value of `t` copies: `t` itself, and those stored inline in
    /// it (in a tuple, an option, a union, an object type copied field by field). A counted
    /// object or array is shared by the copy, not copied.
    fn copied_tuples(&mut self, t: TyId, out: &mut Vec<TyId>, seen: &mut FxHashSet<TyId>) {
        if !seen.insert(t) || self.counted(t) || self.share_kind(t) == ShareKind::Object {
            return;
        }
        let inline = match self.kind(t) {
            TyKind::Tuple(_) => {
                out.push(t);
                true
            }
            TyKind::Option(..) | TyKind::Result(..) => true,
            TyKind::Adt(d, _) => match self.hir.def(d) {
                hir::Def::Enum(_) => true,
                hir::Def::Adt(a) => a.kind != AdtKind::Class,
                _ => false,
            },
            _ => false,
        };
        if inline {
            for p in self.part_types(t) {
                self.copied_tuples(p, out, seen);
            }
        }
    }
}

impl FnLower<'_, '_> {
    /// Is local `id` a tuple this function does not own (a borrowed parameter)? Moving it into
    /// a closure takes another reference to it, in case its type is counted.
    pub(in crate::lower) fn borrowed_tuple(&mut self, id: hir::LocalId) -> bool {
        let info = &self.info[id.0 as usize];
        let ty = info.ty;
        !info.droppable && matches!(self.cx.kind(ty), TyKind::Tuple(_))
    }

    /// After the body of `f`: its `Copy` parameters it only reads, kept for `close_tuples`. An
    /// async function or generator suspends while it holds them, and others run: they are
    /// copies. (One the body changes is recorded by abi.rs `copy_modified_param`.)
    pub(in crate::lower) fn note_read_params(&mut self, f: &hir::FnDef) {
        let suspends = self.asyncx.is_some() || self.in_generator();
        for p in &f.params {
            let l = p.local.0 as usize;
            if p.mode != PassMode::Copy || f.body.locals[l].mutable {
                continue;
            }
            let ty = self.info[l].ty;
            match suspends {
                true => self.cx.note_tuple_copy(ty),
                false => self.cx.cur_read_params.push(ty),
            }
        }
    }
}
