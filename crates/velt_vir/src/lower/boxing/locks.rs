//! The lock set (velt_rt `read_mode`): the counted types a value inside an `RwLock<T>` can
//! reach. Code that may run in a `read` callback counts and writes their objects by read mode's
//! rules (rc.rs), which leave the objects other readers read (the store) uncounted and
//! unwritten, so readers on many threads share them without copies or atomics. An `RwLock<T>` whose `T` can
//! reach what the compiler cannot follow there (a function or interface value, a class in a
//! hierarchy, a weak-capable or proxiable type, a promise) is *exclusive*: `read` takes the lock
//! exclusively, as `write` does.

use rustc_hash::FxHashSet;

use velt_sema::hir::{DefId, TyId, TyKind};

use super::Boxing;
use crate::lower::Cx;

impl Cx<'_> {
    /// Record that `t` is the value type of an `RwLock` the program makes or locks.
    pub(in crate::lower) fn note_lock_value(&mut self, t: TyId) {
        self.facts.lock_values.insert(t);
    }

    /// The lock set and the exclusive locks of `next` (whose counted, weak and proxy sets are
    /// final for this pass).
    pub(super) fn close_locks(&mut self, next: &mut Boxing) {
        next.lock_values
            .extend(self.facts.lock_values.iter().copied());
        let values: Vec<TyId> = next.lock_values.iter().copied().collect();
        // Where code in read mode calls a function value, every class hierarchy, array and
        // object type a shared read's value reaches is counted, also one nothing aliases: read
        // mode tells the store's objects from a reader's own by their count word (the write
        // barrier, `LOCAL`), and the function value may write any of them, which sema cannot
        // see. Elsewhere sema rejects the writes, and only what is shared is counted.
        next.read_indirect |= self.facts.read_indirect;
        if next.read_indirect {
            for &v in &values {
                self.count_reached(next, v);
            }
        }
        for v in values {
            let mut roots = FxHashSet::default();
            let mut boxes = FxHashSet::default();
            if self.lock_reach(next, v, &mut roots, &mut boxes) {
                next.lock_roots.extend(roots);
                next.lock_boxes.extend(boxes);
            } else {
                next.lock_exclusive.insert(v);
            }
        }
    }

    /// Make the class hierarchies, arrays and object types `v` reaches counted in `next`, unless
    /// `v`'s `read` is exclusive (it reaches what read mode cannot follow).
    fn count_reached(&mut self, next: &mut Boxing, v: TyId) {
        let mut reached = vec![];
        if !self.lock_reach_all(next, v, &mut reached) {
            return;
        }
        // Counted as sharing them would count them (classes by hierarchy, objects and arrays
        // as boxes).
        let mut work = reached;
        let mut seen = FxHashSet::default();
        while let Some(t) = work.pop() {
            if seen.insert(t) {
                self.close_share(t, next, &mut work);
            }
        }
    }

    /// [`lock_reach`](Self::lock_reach) collecting every class, array and object type `v`
    /// reaches, counted or not, into `reached`.
    fn lock_reach_all(&mut self, b: &Boxing, v: TyId, reached: &mut Vec<TyId>) -> bool {
        let mut seen = FxHashSet::default();
        let mut work = vec![v];
        while let Some(t) = work.pop() {
            if !seen.insert(t) {
                continue;
            }
            if self.proxiable_in(b, t) {
                return false;
            }
            let parts = match self.kind(t) {
                TyKind::Shared(_) | TyKind::Str => continue,
                TyKind::Dyn(..)
                | TyKind::FnPtr { .. }
                | TyKind::Closure(_)
                | TyKind::Promise(..) => return false,
                TyKind::Adt(d, _) if self.is_class(t) => {
                    let root = self.class_root(d);
                    if root != d || self.has_subclasses(d) || self.weak_class(t).is_some() {
                        return false;
                    }
                    reached.push(t);
                    self.adt_field_tys(t)
                }
                TyKind::Array(e) => {
                    reached.push(t);
                    vec![e]
                }
                TyKind::Adt(..) => {
                    reached.push(t);
                    self.part_types(t)
                }
                _ => self.part_types(t),
            };
            work.extend(parts);
        }
        true
    }

    /// Collect the counted types `v` reaches into `roots` (class hierarchies) and `boxes`;
    /// false when a `read` of a `v` cannot run beside others (module docs).
    fn lock_reach(
        &mut self,
        b: &Boxing,
        v: TyId,
        roots: &mut FxHashSet<DefId>,
        boxes: &mut FxHashSet<TyId>,
    ) -> bool {
        let mut seen = FxHashSet::default();
        let mut work = vec![v];
        while let Some(t) = work.pop() {
            if !seen.insert(t) {
                continue;
            }
            if self.proxiable_in(b, t) {
                return false;
            }
            let parts = match self.kind(t) {
                // Counted atomically, never by a reader's plain count.
                TyKind::Shared(_) | TyKind::Str => continue,
                TyKind::Dyn(..)
                | TyKind::FnPtr { .. }
                | TyKind::Closure(_)
                | TyKind::Promise(..) => return false,
                TyKind::Adt(d, _) if self.is_class(t) => {
                    let root = self.class_root(d);
                    if root != d || self.has_subclasses(d) || self.weak_class(t).is_some() {
                        return false;
                    }
                    if b.roots.contains(&root) {
                        if b.weak_roots.contains(&root) {
                            return false;
                        }
                        roots.insert(root);
                    }
                    self.adt_field_tys(t)
                }
                TyKind::Array(e) => {
                    if self.counted_in(b, t) {
                        if b.weak.contains(&t) {
                            return false;
                        }
                        boxes.insert(t);
                    }
                    vec![e]
                }
                _ => {
                    if self.counted_in(b, t) {
                        if b.weak.contains(&t) {
                            return false;
                        }
                        boxes.insert(t);
                    }
                    self.part_types(t)
                }
            };
            work.extend(parts);
        }
        true
    }

    /// Does the program have a lock set (an `RwLock` whose readers run at once, also one whose
    /// value holds nothing counted yet: its readers' code is built in read mode, which tells
    /// whether they call a function value, `Boxing::read_indirect`)?
    pub(in crate::lower) fn has_lock_set(&self) -> bool {
        !self.boxing.lock_roots.is_empty()
            || !self.boxing.lock_boxes.is_empty()
            || self
                .boxing
                .lock_values
                .iter()
                .any(|v| !self.boxing.lock_exclusive.contains(v))
    }

    /// Is `t` in the lock set: are its counts and writes read-mode aware (module docs)?
    pub(in crate::lower) fn lock_counted(&self, t: TyId) -> bool {
        if self.boxing.lock_roots.is_empty() && self.boxing.lock_boxes.is_empty() {
            return false;
        }
        match self.types.kind(t) {
            TyKind::Adt(d, _) if self.is_class(t) => {
                self.boxing.lock_roots.contains(&self.class_root(*d))
            }
            _ => self.boxing.lock_boxes.contains(&t),
        }
    }

    /// May `read` on an `RwLock` of `v` run beside other readers (module docs)?
    pub(in crate::lower) fn shared_reads(&self, v: TyId) -> bool {
        self.boxing.lock_values.contains(&v) && !self.boxing.lock_exclusive.contains(&v)
    }
}
