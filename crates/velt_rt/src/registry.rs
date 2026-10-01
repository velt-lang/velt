//! Handle tables for the runtime objects `std` exposes through Copy handle structs: TCP
//! listeners and streams, UDP sockets, child processes, file readers and writers, WebSockets
//! (rt_abi_async.md §3.2).
//!
//! Velt code copies those structs freely, so after `close()` through one copy another copy (in
//! another task, say) may still hold the handle. Such a handle is therefore not the object's
//! address but a key into a table, `(generation << 32) | (slot + 1)`: `close()` removes the
//! entry (operations in flight keep their own `Arc`, so the object lives until they finish),
//! and every later use of any copy finds no entry and fails with `EBADF` ("handle is closed")
//! instead of touching freed memory. A slot is reused with the next generation, so a stale
//! handle never reaches a newer object.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::result::{code, IoResult, VeltErr};
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;

/// A key into a [`Registry<T>`] (0 = none); a `u64` at the C ABI.
#[repr(transparent)]
pub struct Key<T>(u64, PhantomData<fn() -> T>);

impl<T> Clone for Key<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Key<T> {}

impl<T> std::fmt::Debug for Key<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Key({:#x})", self.0)
    }
}

impl<T> Key<T> {
    /// The null handle (never open).
    pub const NULL: Self = Key(0, PhantomData);

    /// The raw `u64`, as Velt code sees it.
    pub fn bits(self) -> u64 {
        self.0
    }

    fn slot(self) -> Option<(usize, u32)> {
        let index = (self.0 & 0xffff_ffff) as usize;
        (index != 0).then(|| (index - 1, (self.0 >> 32) as u32))
    }
}

struct Slot<T> {
    generation: u32,
    obj: Option<Arc<T>>,
}

struct Table<T> {
    slots: Vec<Slot<T>>,
    free: Vec<usize>,
}

/// The open objects of one kind, keyed by handle.
pub struct Registry<T> {
    table: Mutex<Table<T>>,
}

impl<T> Default for Registry<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Registry<T> {
    /// An empty table (usable in a `static`).
    pub const fn new() -> Self {
        Registry {
            table: Mutex::new(Table {
                slots: Vec::new(),
                free: Vec::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Table<T>> {
        // A poisoned lock only means a panic elsewhere (which ends the process anyway).
        self.table.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Register a new object; the table holds one reference until [`remove`](Self::remove).
    pub fn insert(&self, obj: T) -> Key<T> {
        let obj = Arc::new(obj);
        let mut t = self.lock();
        let index = match t.free.pop() {
            Some(i) => {
                t.slots[i].obj = Some(obj);
                i
            }
            None => {
                t.slots.push(Slot {
                    generation: 1,
                    obj: Some(obj),
                });
                t.slots.len() - 1
            }
        };
        let generation = t.slots[index].generation;
        Key(
            ((generation as u64) << 32) | (index as u64 + 1),
            PhantomData,
        )
    }

    /// The object, if the handle is still open.
    pub fn get(&self, key: Key<T>) -> Option<Arc<T>> {
        let (index, generation) = key.slot()?;
        let t = self.lock();
        let slot = t.slots.get(index)?;
        (slot.generation == generation)
            .then(|| slot.obj.clone())
            .flatten()
    }

    /// Start an async operation on the object: `start(obj)` builds its future, or, when the
    /// handle is closed, a future that fails at once with `EBADF` (result type `IoResult<R>`).
    pub fn op<R: Send + 'static>(
        &self,
        key: Key<T>,
        start: impl FnOnce(Arc<T>) -> *mut VeltFut,
    ) -> *mut VeltFut {
        match self.get(key) {
            Some(obj) => start(obj),
            None => closed_leaf::<R>(),
        }
    }

    /// Close the handle: every copy of it is dead afterwards. Returns the table's reference
    /// (`None` if the handle was already closed or never valid).
    pub fn remove(&self, key: Key<T>) -> Option<Arc<T>> {
        let (index, generation) = key.slot()?;
        let mut t = self.lock();
        let slot = t.slots.get_mut(index)?;
        if slot.generation != generation {
            return None;
        }
        let obj = slot.obj.take()?;
        // Generation 0 is never handed out, so a key is never 0.
        slot.generation = slot.generation.checked_add(1).unwrap_or(1);
        t.free.push(index);
        Some(obj)
    }
}

/// The error of an operation on a closed handle (`IoError` code `EBADF`).
pub fn closed_error() -> VeltErr {
    VeltErr::new(code::BAD_HANDLE, "handle is closed")
}

/// A failed `IoResult<T>` for a closed handle.
pub fn closed<T>() -> IoResult<T> {
    IoResult::err(closed_error())
}

/// A future that completes at once with [`closed`].
pub fn closed_leaf<T: Send + 'static>() -> *mut VeltFut {
    new_leaf(async { closed::<T>() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_handles_stay_dead_after_slot_reuse() {
        let r: Registry<String> = Registry::new();
        let a = r.insert("a".into());
        let copy = a;
        assert_eq!(r.get(copy).as_deref().map(String::as_str), Some("a"));
        let held = r.get(a).unwrap();
        assert!(r.remove(a).is_some());
        assert!(r.get(copy).is_none());
        assert!(r.remove(copy).is_none());
        // An operation in flight keeps the object alive.
        assert_eq!(*held, "a");
        let b = r.insert("b".into());
        assert_ne!(a.bits(), b.bits());
        assert!(r.get(a).is_none());
        assert_eq!(r.get(b).as_deref().map(String::as_str), Some("b"));
        assert!(r.get(Key(0, PhantomData)).is_none());
        assert!(r.get(Key(12345, PhantomData)).is_none());
    }
}
