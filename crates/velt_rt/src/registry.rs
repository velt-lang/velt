//! Handle tables for the runtime objects `std` exposes: TCP listeners and streams, UDP sockets,
//! child processes, file readers and writers, WebSockets, HTTP servers, requests and responses
//! (rt_abi_async.md §3.2).
//!
//! Velt code copies those structs freely, so after `close()` through one copy another copy (in
//! another task, say) may still hold the handle. Such a handle is therefore not the object's
//! address but a key into a table, `(generation << 32) | (shard << 24) | (slot + 1)`: `close()`
//! removes the entry (operations in flight keep their own `Arc`, so the object lives until they
//! finish), and every later use of any copy finds no entry and fails with `EBADF` ("handle is
//! closed") instead of touching freed memory. A slot is reused with the next generation, so a stale
//! handle never reaches a newer object.
//!
//! The table is split into [`SHARDS`] shards, each behind its own lock. A thread inserts into
//! "its" shard (assigned round-robin on first use), and the shard is part of the key, so the
//! objects one worker creates and uses (an HTTP request and its response, on the worker that runs
//! the handler) never wait for another worker's lock.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};
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

    /// The key whose raw value is `bits` (what generated code passes back to the runtime).
    pub fn from_bits(bits: u64) -> Self {
        Key(bits, PhantomData)
    }

    /// `(shard, slot, generation)`, or `None` for the null key.
    fn slot(self) -> Option<(usize, usize, u32)> {
        let low = (self.0 & 0xffff_ffff) as usize;
        let index = low & SLOT_MASK;
        (index != 0).then(|| (low >> SHARD_SHIFT, index - 1, (self.0 >> 32) as u32))
    }
}

/// Number of shards (a power of two; the shard index takes the key's bits 24..32).
pub const SHARDS: usize = 16;
const SHARD_SHIFT: usize = 24;
const SLOT_MASK: usize = (1 << SHARD_SHIFT) - 1;

/// The shard this thread inserts into.
fn home_shard() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        static HOME: usize = NEXT.fetch_add(1, Ordering::Relaxed) % SHARDS;
    }
    HOME.with(|h| *h)
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
    shards: [Shard<T>; SHARDS],
}

/// One shard's lock and table, on cache lines of its own (no false sharing between workers).
#[repr(align(128))]
struct Shard<T>(Mutex<Table<T>>);

impl<T> Default for Registry<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Registry<T> {
    /// An empty table (usable in a `static`).
    pub const fn new() -> Self {
        Registry {
            shards: [const {
                Shard(Mutex::new(Table {
                    slots: Vec::new(),
                    free: Vec::new(),
                }))
            }; SHARDS],
        }
    }

    fn lock(&self, shard: usize) -> Option<MutexGuard<'_, Table<T>>> {
        // A poisoned lock only means a panic elsewhere (which ends the process anyway).
        let table = self.shards.get(shard)?;
        Some(table.0.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Register a new object; the table holds one reference until [`remove`](Self::remove).
    pub fn insert(&self, obj: T) -> Key<T> {
        let obj = Arc::new(obj);
        let shard = home_shard();
        let mut t = self.lock(shard).expect("ICE: home shard exists");
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
        assert!(
            index < SLOT_MASK,
            "ICE: more than 16M open handles of one kind in one shard"
        );
        let generation = t.slots[index].generation;
        let low = (shard << SHARD_SHIFT) | (index + 1);
        Key(((generation as u64) << 32) | low as u64, PhantomData)
    }

    /// The object, if the handle is still open.
    pub fn get(&self, key: Key<T>) -> Option<Arc<T>> {
        let (shard, index, generation) = key.slot()?;
        let t = self.lock(shard)?;
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

    /// Every open object (a snapshot; taken one shard lock at a time).
    pub fn all(&self) -> Vec<Arc<T>> {
        let mut out = Vec::new();
        for shard in 0..SHARDS {
            if let Some(t) = self.lock(shard) {
                out.extend(t.slots.iter().filter_map(|s| s.obj.clone()));
            }
        }
        out
    }

    /// Close the handle: every copy of it is dead afterwards. Returns the table's reference
    /// (`None` if the handle was already closed or never valid).
    pub fn remove(&self, key: Key<T>) -> Option<Arc<T>> {
        let (shard, index, generation) = key.slot()?;
        let mut t = self.lock(shard)?;
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
        // A key naming a shard that does not exist.
        assert!(r
            .get(Key((1 << 32) | (0xff << 24) | 1, PhantomData))
            .is_none());
    }

    #[test]
    fn threads_insert_into_their_own_shards() {
        let r: Registry<usize> = Registry::new();
        let keys: Vec<Key<usize>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..SHARDS)
                .map(|i| {
                    let r = &r;
                    s.spawn(move || r.insert(i))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(r.get(*k).as_deref(), Some(&i));
        }
        let shards: std::collections::HashSet<usize> =
            keys.iter().map(|k| k.slot().unwrap().0).collect();
        assert!(
            shards.len() > 1,
            "inserts from different threads use different shards"
        );
        for k in keys {
            assert!(r.remove(k).is_some());
            assert!(r.get(k).is_none());
        }
    }
}
