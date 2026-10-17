//! The side table: weakly held objects, the weak maps and the `WeakRef` slots.
//!
//! One table for the process, behind a lock. A weakly held object is used by one task at a time
//! (weak-capable types are never atomic, and sema keeps weak handles out of transfers), but a
//! task moves between worker threads at an `await`, and an object reached through a `shared`
//! `Mutex` is released by whichever task holds the lock: so map ids, `WeakRef` slots and object
//! records must mean the same on every thread. The lock is taken only on cold paths (weak map
//! and `WeakRef` operations, and releases of objects with [`RC_WEAK`] set); programs without
//! weak collections never touch it.
//!
//! Lock discipline: `with` never runs generated code that could re-enter it. Value retains and
//! releases run after the lock is released (`release_values`, `velt_rt_weakmap_get`); trace glue
//! runs under the lock (in `trial`) but only reads fields and calls `visit`, which records into
//! a local graph. A re-entry would deadlock, so `with` turns it into an ICE instead.

use super::trial::{self, Contribution};
use super::{no_record, rc_word, ReleaseFn, RetainFn, TraceFn, RC_COUNT, RC_WEAK};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::{Mutex, PoisonError};

/// A weak map's handle.
pub type MapId = u32;
/// A `WeakRef`'s handle.
pub type RefId = u32;

/// Hashes object addresses: one multiply (Fibonacci hashing), enough for pointers.
#[derive(Default)]
pub(super) struct AddrHasher(u64);

impl Hasher for AddrHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(8) ^ u64::from(b)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }
    fn write_usize(&mut self, n: usize) {
        self.0 = (n as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .rotate_left(29);
    }
}

pub(super) type AddrMap<V> = HashMap<usize, V, BuildHasherDefault<AddrHasher>>;

/// A weakly held object's record.
#[derive(Default)]
pub(super) struct Node {
    /// The object's trace glue, when known (keys get their map's, cycle members their parent's).
    pub(super) trace: Option<TraceFn>,
    /// The maps with an entry for this object as key.
    pub(super) keyed_in: Vec<MapId>,
    /// The entries `(map, key)` whose value graph refers to this object (on a path to the key).
    pub(super) member_of: Vec<(MapId, usize)>,
    /// The `WeakRef`s to this object.
    pub(super) refs: Vec<RefId>,
    /// References to this object from its entries (the map's reference to a value, and the
    /// references inside entry values): a count at or below it may mean nothing outside refers
    /// to the object, so a release to it runs a trial deletion.
    pub(super) hint: u32,
    /// References to this object from its cycle that the last trial through it counted: it
    /// covers references added after insert (a value that later gains a path back to its key),
    /// and puts objects on such new paths in the table.
    pub(super) observed: u32,
}

impl Node {
    fn is_empty(&self) -> bool {
        self.keyed_in.is_empty() && self.member_of.is_empty() && self.refs.is_empty()
    }
}

/// An entry's value and the references it was found to hold back to its key's cycle.
pub(super) struct Entry {
    pub(super) value: u64,
    pub(super) members: Vec<Contribution>,
}

/// A weak map.
pub(super) struct MapData {
    pub(super) key_trace: Option<TraceFn>,
    pub(super) value_retain: Option<RetainFn>,
    pub(super) value_release: Option<ReleaseFn>,
    pub(super) value_trace: Option<TraceFn>,
    pub(super) entries: AddrMap<Entry>,
}

/// The process's weak state.
#[derive(Default)]
pub(super) struct State {
    pub(super) nodes: AddrMap<Node>,
    pub(super) maps: Vec<Option<MapData>>,
    free_maps: Vec<MapId>,
    refs: Vec<Option<usize>>,
    /// A word kept with each `WeakRef` slot for the compiler (the descriptor of an `object`
    /// target, so `deref()` can rebuild the dynamic value); 0 when not given.
    ref_words: Vec<u64>,
    free_refs: Vec<RefId>,
}

static STATE: Mutex<State> = Mutex::new(State {
    nodes: HashMap::with_hasher(BuildHasherDefault::new()),
    maps: Vec::new(),
    free_maps: Vec::new(),
    refs: Vec::new(),
    ref_words: Vec::new(),
    free_refs: Vec::new(),
});

// A cascade (the releases `release_values` runs) is synchronous code: drop glue never awaits,
// so it starts and ends on one thread, and these are per thread. Two threads may run cascades
// at once over different objects; each defers only its own trials.
thread_local! {
    /// Value releases under way: trials wait until the outermost finishes (`PENDING`).
    static CASCADE: Cell<u32> = const { Cell::new(0) };
    /// Objects that reached their hint during a cascade, tried once it ends.
    static PENDING: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    /// This thread holds the table's lock (a re-entry would deadlock).
    static LOCKED: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` with the table locked. `f` must not call back into `with` (module docs).
pub(super) fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    struct Unlocked;
    impl Drop for Unlocked {
        fn drop(&mut self) {
            LOCKED.with(|l| l.set(false));
        }
    }
    assert!(
        !LOCKED.with(|l| l.replace(true)),
        "ICE: the weak table was re-entered while locked"
    );
    let _unlocked = Unlocked;
    // A panic under the lock is an ICE that unwinds out of the program (or a test); the table
    // is not left half-updated by any path that panics, so a poisoned lock is still usable.
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    f(&mut state)
}

/// Empties the table (tests: each starts from a clean one).
#[cfg(test)]
pub(super) fn reset() {
    with(|s| *s = State::default());
}

impl State {
    pub(super) fn map(&self, m: MapId) -> &MapData {
        self.maps[m as usize]
            .as_ref()
            .expect("ICE: weak map used after drop")
    }

    fn map_mut(&mut self, m: MapId) -> &mut MapData {
        self.maps[m as usize]
            .as_mut()
            .expect("ICE: weak map used after drop")
    }

    pub(super) fn new_map(
        &mut self,
        key_trace: Option<TraceFn>,
        value_retain: Option<RetainFn>,
        value_release: Option<ReleaseFn>,
        value_trace: Option<TraceFn>,
    ) -> MapId {
        let data = MapData {
            key_trace,
            value_retain,
            value_release,
            value_trace,
            entries: AddrMap::default(),
        };
        match self.free_maps.pop() {
            Some(m) => {
                self.maps[m as usize] = Some(data);
                m
            }
            None => {
                if self.maps.is_empty() {
                    // Id 0 is never a map: generated code keeps it for "not created yet".
                    self.maps.push(None);
                }
                self.maps.push(Some(data));
                (self.maps.len() - 1) as MapId
            }
        }
    }

    pub(super) fn map_len(&self, m: MapId) -> usize {
        self.map(m).entries.len()
    }

    pub(super) fn tracked(&self) -> usize {
        self.nodes.len()
    }

    /// The record of `obj`, created (and the object marked) if it has none.
    fn node(&mut self, obj: usize, trace: Option<TraceFn>) -> &mut Node {
        let node = self.nodes.entry(obj).or_insert_with(|| {
            // SAFETY: callers pass live counted objects.
            unsafe { *rc_word(obj as *mut u8) |= RC_WEAK };
            Node::default()
        });
        if node.trace.is_none() {
            node.trace = trace;
        }
        node
    }

    /// Drops `obj`'s record once nothing refers to it weakly, clearing its mark.
    fn settle(&mut self, obj: usize) {
        if self.nodes.get(&obj).is_some_and(Node::is_empty) {
            self.nodes.remove(&obj);
            // SAFETY: a recorded object is live (`forget` removes it before it is freed).
            unsafe { *rc_word(obj as *mut u8) &= !RC_WEAK };
        }
    }

    pub(super) fn get(&self, m: MapId, key: *mut u8) -> Option<u64> {
        self.map(m).entries.get(&(key as usize)).map(|e| e.value)
    }

    /// Inserts or replaces an entry; returns the values to release.
    pub(super) fn set(&mut self, m: MapId, key: *mut u8, value: u64) -> Vec<(ReleaseFn, u64)> {
        let key_trace = self.map(m).key_trace;
        self.set_traced(m, key, value, key_trace)
    }

    /// [`State::set`] with the key's own trace glue (`key_trace`) instead of the map's: the
    /// keys of a map whose key type is dynamic (`WeakMap<object, V>`) have different types.
    pub(super) fn set_traced(
        &mut self,
        m: MapId,
        key: *mut u8,
        value: u64,
        key_trace: Option<TraceFn>,
    ) -> Vec<(ReleaseFn, u64)> {
        let old = self.delete(m, key).unwrap_or_default();
        let k = key as usize;
        let map = self.map(m);
        let members = match map.value_trace {
            // SAFETY: a nonzero value of a map with value glue is a live counted object, owned
            // by the map from here on.
            Some(trace) if value != 0 => unsafe { trial::members(key, value as *mut u8, trace) },
            _ => Vec::new(),
        };
        self.node(k, key_trace).keyed_in.push(m);
        for c in &members {
            let node = self.node(c.obj, c.trace);
            node.member_of.push((m, k));
            node.hint += c.refs;
        }
        self.map_mut(m).entries.insert(k, Entry { value, members });
        old
    }

    /// Removes `key`'s entry from `m`; returns its value to release (`None`: there was no entry).
    pub(super) fn delete(&mut self, m: MapId, key: *mut u8) -> Option<Vec<(ReleaseFn, u64)>> {
        let k = key as usize;
        let entry = self.map_mut(m).entries.remove(&k)?;
        if let Some(node) = self.nodes.get_mut(&k) {
            remove_one(&mut node.keyed_in, &m);
        }
        self.unlink(m, k, &entry);
        self.settle(k);
        let release = self.map(m).value_release.filter(|_| entry.value != 0);
        Some(release.map(|r| (r, entry.value)).into_iter().collect())
    }

    /// Takes `entry`'s contributions off its members' records.
    fn unlink(&mut self, m: MapId, k: usize, entry: &Entry) {
        for c in &entry.members {
            if let Some(node) = self.nodes.get_mut(&c.obj) {
                remove_one(&mut node.member_of, &(m, k));
                node.hint -= c.refs;
                self.settle(c.obj);
            }
        }
    }

    /// Drops map `m`; returns its values to release.
    pub(super) fn drop_map(&mut self, m: MapId) -> Vec<(ReleaseFn, u64)> {
        let keys: Vec<usize> = self.map(m).entries.keys().copied().collect();
        let old = keys
            .into_iter()
            .flat_map(|k| self.delete(m, k as *mut u8).unwrap_or_default())
            .collect();
        self.maps[m as usize] = None;
        self.free_maps.push(m);
        old
    }

    /// `obj` is about to be freed: it leaves every map, entry record and `WeakRef`. Returns the
    /// values of its entries to release (`None`: there is no record of `obj`).
    pub(super) fn forget(&mut self, obj: *mut u8) -> Option<Vec<(ReleaseFn, u64)>> {
        let o = obj as usize;
        let node = self.nodes.remove(&o)?;
        for (m, k) in &node.member_of {
            if let Some(e) = self.map_mut(*m).entries.get_mut(k) {
                e.members.retain(|c| c.obj != o);
            }
        }
        for r in &node.refs {
            self.refs[*r as usize] = None;
        }
        PENDING.with(|p| p.borrow_mut().retain(|&x| x != o));
        let mut old = Vec::new();
        for m in node.keyed_in {
            // The record is gone, so `delete` leaves the object's count word alone.
            old.extend(self.delete(m, obj).unwrap_or_default());
        }
        Some(old)
    }

    pub(super) fn new_ref(&mut self, obj: *mut u8, word: u64) -> RefId {
        let r = match self.free_refs.pop() {
            Some(r) => {
                self.refs[r as usize] = Some(obj as usize);
                self.ref_words[r as usize] = word;
                r
            }
            None => {
                if self.refs.is_empty() {
                    // Id 0 is never a `WeakRef` slot (as for maps).
                    self.refs.push(None);
                    self.ref_words.push(0);
                }
                self.refs.push(Some(obj as usize));
                self.ref_words.push(word);
                (self.refs.len() - 1) as RefId
            }
        };
        self.node(obj as usize, None).refs.push(r);
        r
    }

    pub(super) fn deref(&self, r: RefId) -> *mut u8 {
        match self.refs[r as usize] {
            Some(obj) => {
                // SAFETY: a set slot names a live object (`forget` clears it first).
                unsafe { *rc_word(obj as *mut u8) += 1 };
                obj as *mut u8
            }
            None => std::ptr::null_mut(),
        }
    }

    /// The word given when `r` was made.
    pub(super) fn ref_word(&self, r: RefId) -> u64 {
        self.ref_words.get(r as usize).copied().unwrap_or(0)
    }

    pub(super) fn drop_ref(&mut self, r: RefId) {
        if let Some(obj) = self.refs[r as usize].take() {
            if let Some(node) = self.nodes.get_mut(&obj) {
                remove_one(&mut node.refs, &r);
            }
            self.settle(obj);
        }
        self.free_refs.push(r);
    }
}

fn remove_one<T: PartialEq>(v: &mut Vec<T>, x: &T) {
    if let Some(i) = v.iter().position(|y| y == x) {
        v.swap_remove(i);
    }
}

/// Releases map values with no borrow of the table held. Trials that these releases cause wait
/// until the outermost release ends, so a trial never sees a cycle half released.
pub(super) fn release_values(values: Vec<(ReleaseFn, u64)>) {
    if values.is_empty() {
        return;
    }
    CASCADE.with(|c| c.set(c.get() + 1));
    for (release, v) in values {
        // SAFETY: the map owned one reference to `v`, which it gives up here.
        unsafe { release(v) };
    }
    let depth = CASCADE.with(|c| {
        c.set(c.get() - 1);
        c.get()
    });
    if depth == 0 {
        while let Some(obj) = PENDING.with(|p| p.borrow_mut().pop()) {
            try_free(obj);
        }
    }
}

/// Decrements the count of `obj` (weakly held, count above 1); if that leaves it at or below its
/// hint, tries whether its cycle is garbage (now, or once the cascade under way ends). No record of
/// `obj` is an ICE, reported before the count changes.
///
/// # Safety
/// `obj` is a live counted object with [`RC_WEAK`] set, and the caller owns one reference.
pub(super) unsafe fn decrement(obj: *mut u8) {
    let o = obj as usize;
    let cascade = CASCADE.with(Cell::get) > 0;
    // One acquisition: the record check, the decrement and (outside a cascade) the trial.
    let values = with(|s| {
        let n = s.nodes.get(&o)?;
        let hint = n.hint.max(n.observed);
        // SAFETY: as for the function; the count changes under the lock, like `deref`'s increment.
        let left = unsafe {
            let rc = rc_word(obj);
            *rc -= 1;
            *rc & RC_COUNT
        };
        if left > u64::from(hint) {
            return Some(Vec::new());
        }
        if cascade {
            PENDING.with(|p| {
                let mut p = p.borrow_mut();
                if !p.contains(&o) {
                    p.push(o);
                }
            });
            return Some(Vec::new());
        }
        Some(trial_locked(s, o))
    });
    let Some(values) = values else { no_record(obj) };
    release_values(values);
}

fn at_hint(s: &State, o: usize) -> bool {
    match s.nodes.get(&o) {
        // SAFETY: a recorded object is live.
        Some(n) => {
            (unsafe { *rc_word(o as *mut u8) & RC_COUNT }) <= u64::from(n.hint.max(n.observed))
        }
        None => false,
    }
}

/// The trial from `o` with the table locked: the values of the dead keys' entries to release.
fn trial_locked(s: &mut State, o: usize) -> Vec<(ReleaseFn, u64)> {
    let trial = trial::run(s, o);
    for c in trial.observed {
        s.node(c.obj, c.trace).observed = c.refs;
    }
    let dead = trial.dead.into_iter();
    dead.flat_map(|(m, k)| s.delete(m, k as *mut u8).unwrap_or_default())
        .collect()
}

/// Runs a trial deletion from `o` (a live recorded object) and deletes the entries of every key
/// it finds dead, which frees the cycle.
fn try_free(o: usize) {
    let values = with(|s| {
        if at_hint(s, o) {
            trial_locked(s, o)
        } else {
            Vec::new()
        }
    });
    release_values(values);
}
