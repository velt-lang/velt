//! The members of a `json.Value` object: insertion order, last value wins for a repeated key,
//! O(1) access by position (`at`, `keyAt`) and by key (hashed past [`INDEX_THRESHOLD`] members),
//! and O(1) amortized `delete` anywhere.
//!
//! Small objects keep their members in a dense vector (a delete moves at most 16 entries).
//! Large ones have an [`Index`] and delete by leaving a hole: a hole at the front or the back
//! is skipped right away (positions stay O(1) for objects emptied from either end), a hole in
//! the middle makes positional access go through a table of the live positions, built once
//! after the last change. When the holes outnumber the members the vector is compacted, so
//! every delete costs O(1) amortized and the vector stays at most twice the member count.

use super::value::Value;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

/// Objects with more keys than this get a hash index for `get` and duplicate-key detection.
pub const INDEX_THRESHOLD: usize = 16;

/// A member: key and value.
pub type Entry = (Box<str>, Arc<Value>);

/// The live members of an object in order (see [`Object::iter`]).
pub type Entries<'a> = std::iter::Flatten<std::slice::Iter<'a, Option<Entry>>>;

/// Object members in document order (first-occurrence position, last value wins — like
/// `JSON.parse`).
#[derive(Debug, Default, Clone)]
pub struct Object {
    /// Members by slot; `None` is a deleted member's hole (only with an index).
    slots: Vec<Option<Entry>>,
    index: Option<Box<Index>>,
}

/// Lookup structures of a large object.
#[derive(Debug, Clone)]
struct Index {
    /// Slot of every live key.
    slots: HashMap<Box<str>, usize>,
    /// Slots before `head` are holes; `slots[head]` is live (or `head == slots.len()`).
    head: usize,
    /// Holes at or after `head` (never the last slot).
    holes: usize,
    /// Slot of each live member in order, built on demand while `holes > 0`.
    order: OnceLock<Box<[usize]>>,
}

#[cfg(test)]
thread_local! {
    /// Work done editing objects on this thread: slots moved or visited by deletes (compaction,
    /// order tables, moves in small objects) and children copied when an edit copies a shared
    /// node on write. Lets tests check that editing stays linear by counting instead of timing.
    pub(crate) static WORK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count `n` steps of edit work (tests only).
#[inline(always)]
pub(super) fn count_work(n: usize) {
    #[cfg(test)]
    WORK.with(|w| w.set(w.get() + n));
    #[cfg(not(test))]
    let _ = n;
}

impl Object {
    /// Number of members.
    pub fn len(&self) -> usize {
        match &self.index {
            Some(ix) => self.slots.len() - ix.head - ix.holes,
            None => self.slots.len(),
        }
    }

    /// Whether the object has no members.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The members in order.
    pub fn iter(&self) -> Entries<'_> {
        let head = self.index.as_ref().map_or(0, |ix| ix.head);
        self.slots[head..].iter().flatten()
    }

    /// The value of `key`.
    pub fn get(&self, key: &str) -> Option<&Arc<Value>> {
        let slot = self.find(key)?;
        self.slots[slot].as_ref().map(|(_, v)| v)
    }

    /// The `i`-th member.
    pub fn entry_at(&self, i: usize) -> Option<&Entry> {
        let slot = match &self.index {
            None => i,
            Some(ix) if ix.holes == 0 => ix.head.checked_add(i)?,
            Some(ix) => *ix.order.get_or_init(|| live_slots(&self.slots)).get(i)?,
        };
        self.slots.get(slot)?.as_ref()
    }

    /// Slot of `key`.
    fn find(&self, key: &str) -> Option<usize> {
        match &self.index {
            Some(ix) => ix.slots.get(key).copied(),
            None => self
                .slots
                .iter()
                .position(|e| e.as_ref().is_some_and(|(k, _)| &**k == key)),
        }
    }

    /// Insert, replacing the value of an existing key in place.
    pub fn insert(&mut self, key: Box<str>, value: Arc<Value>) {
        if let Some(slot) = self.find(&key) {
            if let Some((_, v)) = &mut self.slots[slot] {
                *v = value;
            }
            return;
        }
        let slot = self.slots.len();
        match &mut self.index {
            Some(ix) => {
                ix.slots.insert(key.clone(), slot);
                ix.order.take();
            }
            None if slot == INDEX_THRESHOLD => {
                let mut ix = Index::new(&self.slots);
                ix.slots.insert(key.clone(), slot);
                self.index = Some(Box::new(ix));
            }
            None => {}
        }
        self.slots.push(Some((key, value)));
    }

    /// Remove `key`, keeping the order of the others; whether it was there. O(1) amortized.
    pub fn remove(&mut self, key: &str) -> bool {
        let Some(slot) = self.find(key) else {
            return false;
        };
        let Some(ix) = &mut self.index else {
            count_work(self.slots.len() - slot);
            self.slots.remove(slot);
            return true;
        };
        ix.slots.remove(key);
        ix.order.take();
        self.slots[slot] = None;
        if slot == ix.head {
            ix.head += 1;
            while ix.head < self.slots.len() && self.slots[ix.head].is_none() {
                ix.head += 1;
                ix.holes -= 1;
            }
        } else if slot + 1 == self.slots.len() {
            self.slots.pop();
            while self.slots.last().is_some_and(Option::is_none) {
                self.slots.pop();
                ix.holes -= 1;
            }
        } else {
            ix.holes += 1;
        }
        let live = self.slots.len() - ix.head - ix.holes;
        if live <= INDEX_THRESHOLD || ix.head + ix.holes > live {
            self.compact();
        }
        true
    }

    /// Drop the holes (and the index when few members are left).
    fn compact(&mut self) {
        if self.index.take().is_none() {
            return;
        }
        count_work(self.slots.len());
        self.slots.retain(Option::is_some);
        if self.slots.len() > INDEX_THRESHOLD {
            self.index = Some(Box::new(Index::new(&self.slots)));
        }
    }

    /// Move out all values (leaves the object empty).
    pub fn take_values(&mut self) -> Vec<Arc<Value>> {
        self.index = None;
        std::mem::take(&mut self.slots)
            .into_iter()
            .flatten()
            .map(|(_, v)| v)
            .collect()
    }
}

impl Index {
    /// The index of a dense (hole-free) slot vector.
    fn new(slots: &[Option<Entry>]) -> Index {
        let keys = slots
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().map(|(k, _)| (k.clone(), i)))
            .collect();
        Index {
            slots: keys,
            head: 0,
            holes: 0,
            order: OnceLock::new(),
        }
    }
}

/// Slots of the live members, in order.
fn live_slots(slots: &[Option<Entry>]) -> Box<[usize]> {
    count_work(slots.len());
    slots
        .iter()
        .enumerate()
        .filter_map(|(i, e)| e.as_ref().map(|_| i))
        .collect()
}
