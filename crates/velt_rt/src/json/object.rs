//! The members of a `json.Value` object: insertion order, last value wins for a repeated key,
//! access by position (`at`, `keyAt`: O(1), O(log n) after deletes in the middle) and by key
//! (O(1), hashed past [`INDEX_THRESHOLD`] members), and O(1) amortized `delete` anywhere.
//!
//! Small objects keep their members in a dense vector (a delete moves at most 16 entries).
//! Large ones have an [`Index`] and delete by leaving a hole: a hole at the front or the back
//! is skipped right away (positions stay O(1) for objects emptied from either end). With holes
//! in the middle, positional access finds the `i`-th member in a Fenwick tree of the live slots
//! ([`Positions`], O(log n)): built by the first such access after a compaction, then kept up
//! to date by every delete and insert in O(log n). When the holes outnumber the members the
//! vector is compacted, so every delete costs O(1) amortized (plus O(log n) while the tree
//! exists) and the vector stays at most twice the member count.
//!
//! Members whose keys are array indexes (`"0"` to `"4294967294"`) come first, ascending, as in
//! JavaScript. They live apart, in a deque sorted by index ([`Index::ints`]): found by binary
//! search, appended (or prepended) in O(1), and inserted or deleted in the middle by moving the
//! members on the shorter side. `JSON.parse` reads every member in document order and moves the
//! index members there once the object is complete ([`Object::order_indexes`]).

use super::value::{Text, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};

use positions::Positions;

/// Objects with more keys than this get a hash index for `get` and duplicate-key detection.
pub const INDEX_THRESHOLD: usize = 16;

/// A member: key (WTF-8, `value::Text`) and value.
pub type Entry = (Text, Arc<Value>);

/// The live members of an object in order (see [`Object::iter`]): the array-index members,
/// then the others.
#[derive(Clone)]
pub struct Entries<'a> {
    ints: Option<std::collections::vec_deque::Iter<'a, Entry>>,
    rest: std::iter::Flatten<std::slice::Iter<'a, Option<Entry>>>,
}

impl<'a> Iterator for Entries<'a> {
    type Item = &'a Entry;

    #[inline]
    fn next(&mut self) -> Option<&'a Entry> {
        if let Some(ints) = &mut self.ints {
            match ints.next() {
                Some(e) => return Some(e),
                None => self.ints = None,
            }
        }
        self.rest.next()
    }
}

/// Object members in document order (first-occurrence position, last value wins — like
/// `JSON.parse`).
#[derive(Debug, Default, Clone)]
pub struct Object {
    /// Members by slot; `None` is a deleted member's hole (only with an index). No key here is
    /// an array index, except while `JSON.parse` reads the object (see the module docs).
    slots: Vec<Option<Entry>>,
    index: Option<Box<Index>>,
}

/// Lookup structures of a large object, or of one with array-index members.
#[derive(Debug, Clone)]
struct Index {
    /// The array-index members, ascending.
    ints: VecDeque<Entry>,
    /// Slot of every live key.
    slots: HashMap<Text, usize>,
    /// Slots before `head` are holes; `slots[head]` is live (or `head == slots.len()`).
    head: usize,
    /// Holes at or after `head` (never the last slot).
    holes: usize,
    /// Which slots are live, for positional access while `holes > 0`: built on demand, then
    /// kept up to date until the next compaction.
    positions: OnceLock<Positions>,
}

#[cfg(test)]
thread_local! {
    /// Work done editing objects on this thread: slots moved or visited by linear passes
    /// (compaction, building [`Positions`], moves in small objects) and children copied when an
    /// edit copies a shared node on write. Lets tests check that editing stays linear by
    /// counting instead of timing. The O(log n) steps of a [`Positions`] tree are not counted.
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
            Some(ix) => self.slots.len() - ix.head - ix.holes + ix.ints.len(),
            None => self.slots.len(),
        }
    }

    /// Whether the object has no members.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The members in order.
    pub fn iter(&self) -> Entries<'_> {
        let (head, ints) = match &self.index {
            Some(ix) => (ix.head, (!ix.ints.is_empty()).then(|| ix.ints.iter())),
            None => (0, None),
        };
        Entries {
            ints,
            rest: self.slots[head..].iter().flatten(),
        }
    }

    /// The value of `key`.
    pub fn get(&self, key: &[u8]) -> Option<&Arc<Value>> {
        // An array-index key is never in the slots (outside `JSON.parse`): without index
        // members, `find` misses it.
        if let Some(ix) = self.index.as_ref().filter(|ix| !ix.ints.is_empty()) {
            if let Some(n) = array_index(key) {
                return ix.int_pos(n).ok().map(|i| &ix.ints[i].1);
            }
        }
        let slot = self.find(key)?;
        self.slots[slot].as_ref().map(|(_, v)| v)
    }

    /// The `i`-th member.
    pub fn entry_at(&self, mut i: usize) -> Option<&Entry> {
        if let Some(ix) = &self.index {
            if i < ix.ints.len() {
                return ix.ints.get(i);
            }
            i -= ix.ints.len();
        }
        let slot = match &self.index {
            None => i,
            Some(ix) if ix.holes == 0 => ix.head.checked_add(i)?,
            Some(ix) => ix
                .positions
                .get_or_init(|| Positions::new(&self.slots))
                .select(i)?,
        };
        self.slots.get(slot)?.as_ref()
    }

    /// Slot of `key`.
    #[inline(always)]
    fn find(&self, key: &[u8]) -> Option<usize> {
        match &self.index {
            Some(ix) => ix.slots.get(key).copied(),
            None => self
                .slots
                .iter()
                .position(|e| e.as_ref().is_some_and(|(k, _)| &**k == key)),
        }
    }

    /// Insert, replacing the value of an existing key in place. A new key goes where
    /// JavaScript puts it: an array index (`"0"` to `"4294967294"`) among the other indexes,
    /// ascending, before every other key; any other key last.
    pub fn insert(&mut self, key: Text, value: Arc<Value>) {
        if let Some(n) = array_index(&key) {
            return self.insert_int(n, key, value);
        }
        if let Some(slot) = self.find(&key) {
            if let Some((_, v)) = &mut self.slots[slot] {
                *v = value;
            }
            return;
        }
        self.push_new(key, value);
    }

    /// Insert the array-index member `n`: appended or prepended in O(1), else the members on
    /// the shorter side move.
    fn insert_int(&mut self, n: u32, key: Text, value: Arc<Value>) {
        let slots = &self.slots;
        let ix = self
            .index
            .get_or_insert_with(|| Box::new(Index::new(slots)));
        match ix.int_pos(n) {
            Ok(i) => ix.ints[i].1 = value,
            Err(i) => {
                count_work(i.min(ix.ints.len() - i));
                ix.ints.insert(i, (key, value));
            }
        }
    }

    /// Insert in document order (`JSON.parse` before [`Object::order_indexes`]): a new key
    /// goes last.
    #[inline]
    pub(super) fn insert_last(&mut self, key: Text, value: Arc<Value>) {
        if let Some(slot) = self.find(&key) {
            if let Some((_, v)) = &mut self.slots[slot] {
                *v = value;
            }
            return;
        }
        self.push_new(key, value);
    }

    /// Move the members whose keys are array indexes to [`Index::ints`], ascending, keeping
    /// the order of the others: JavaScript's order, for an object read in document order
    /// (`JSON.parse`) that may have such a key.
    pub(super) fn order_indexes(&mut self) {
        if !self.iter().any(|(k, _)| array_index(k).is_some()) {
            return;
        }
        count_work(self.slots.len());
        let mut ints = self.index.take().map(|ix| ix.ints).unwrap_or_default();
        let mut named = Vec::with_capacity(self.slots.len());
        for (k, v) in std::mem::take(&mut self.slots).into_iter().flatten() {
            match array_index(&k) {
                Some(_) => ints.push_back((k, v)),
                None => named.push(Some((k, v))),
            }
        }
        ints.make_contiguous()
            .sort_unstable_by_key(|(k, _)| array_index(k).unwrap_or(u32::MAX));
        self.slots = named;
        let mut ix = Index::new(&self.slots);
        ix.ints = ints;
        self.index = Some(Box::new(ix));
    }

    /// Append the new member `key`.
    #[inline]
    fn push_new(&mut self, key: Text, value: Arc<Value>) {
        let slot = self.slots.len();
        match &mut self.index {
            Some(ix) => {
                ix.slots.insert(key.clone(), slot);
                if let Some(p) = ix.positions.get_mut() {
                    p.push_live();
                }
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

    /// Remove `key`, keeping the order of the others; whether it was there. O(1) amortized
    /// (an array-index member in the middle: the members on the shorter side move).
    pub fn remove(&mut self, key: &[u8]) -> bool {
        if let Some(n) = array_index(key) {
            let Some(ix) = &mut self.index else {
                return false;
            };
            let Ok(i) = ix.int_pos(n) else {
                return false;
            };
            count_work(i.min(ix.ints.len() - i));
            ix.ints.remove(i);
            if ix.ints.is_empty() && self.slots.len() - ix.head - ix.holes <= INDEX_THRESHOLD {
                self.compact();
            }
            return true;
        }
        let Some(slot) = self.find(key) else {
            return false;
        };
        let Some(ix) = &mut self.index else {
            count_work(self.slots.len() - slot);
            self.slots.remove(slot);
            return true;
        };
        ix.slots.remove(key);
        self.slots[slot] = None;
        if let Some(p) = ix.positions.get_mut() {
            p.clear(slot);
        }
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
            if let Some(p) = ix.positions.get_mut() {
                p.truncate(self.slots.len());
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

    /// Drop the holes (and the index when few members are left and none is an array index).
    fn compact(&mut self) {
        let Some(old) = self.index.take() else {
            return;
        };
        count_work(self.slots.len());
        self.slots.retain(Option::is_some);
        if self.slots.len() > INDEX_THRESHOLD || !old.ints.is_empty() {
            let mut ix = Index::new(&self.slots);
            ix.ints = old.ints;
            self.index = Some(Box::new(ix));
        }
    }

    /// Move out all values (leaves the object empty).
    pub fn take_values(&mut self) -> Vec<Arc<Value>> {
        let ints = self.index.take().map(|ix| ix.ints).unwrap_or_default();
        ints.into_iter()
            .chain(std::mem::take(&mut self.slots).into_iter().flatten())
            .map(|(_, v)| v)
            .collect()
    }
}

/// The array index `key` names (canonical decimal `0` to `2^32 - 2`): such keys come first
/// in a JavaScript object, ascending. One byte test for the common key.
#[inline]
pub fn array_index(key: &[u8]) -> Option<u32> {
    let (&first, rest) = key.split_first()?;
    if !first.is_ascii_digit() {
        return None;
    }
    array_index_digits(first, rest)
}

#[cold]
fn array_index_digits(first: u8, rest: &[u8]) -> Option<u32> {
    if (first == b'0' && !rest.is_empty()) || rest.len() > 9 {
        return None;
    }
    let mut n = u64::from(first - b'0');
    for &b in rest {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n * 10 + u64::from(b - b'0');
    }
    u32::try_from(n).ok().filter(|&n| n != u32::MAX)
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
            ints: VecDeque::new(),
            slots: keys,
            head: 0,
            holes: 0,
            positions: OnceLock::new(),
        }
    }

    /// Where the array-index member `n` is (`Ok`) or goes (`Err`) in `ints`.
    #[inline]
    fn int_pos(&self, n: u32) -> Result<usize, usize> {
        match self.ints.back() {
            None => Err(0),
            Some((k, _)) if array_index(k).is_some_and(|m| m < n) => Err(self.ints.len()),
            _ => self
                .ints
                .binary_search_by_key(&n, |(k, _)| array_index(k).unwrap_or(u32::MAX)),
        }
    }
}

mod positions {
    use super::{count_work, Entry};

    /// A Fenwick tree over an object's slots (1 for a live member, 0 for a hole): finds the
    /// slot of the `i`-th member, marks a slot deleted and appends a member in O(log n).
    #[derive(Debug, Clone)]
    pub struct Positions {
        /// `tree[k - 1]` counts the live slots in `(k - lowbit(k), k]` (1-based `k`).
        tree: Vec<usize>,
    }

    impl Positions {
        /// The tree of `slots` (O(number of slots), counted as work).
        pub fn new(slots: &[Option<Entry>]) -> Positions {
            count_work(slots.len());
            let mut tree: Vec<usize> = slots.iter().map(|e| e.is_some() as usize).collect();
            for k in 1..=tree.len() {
                let parent = k + lowbit(k);
                if parent <= tree.len() {
                    tree[parent - 1] += tree[k - 1];
                }
            }
            Positions { tree }
        }

        /// The slot of the `i`-th live member (0-based), if there are more than `i`.
        pub fn select(&self, i: usize) -> Option<usize> {
            let n = self.tree.len();
            if n == 0 {
                return None;
            }
            // Descend from the largest power of two: `at` is the last slot (1-based) whose
            // prefix holds at most `i` members, so the member sought is the next slot.
            let (mut at, mut left) = (0, i + 1);
            let mut step = 1 << (usize::BITS - 1 - n.leading_zeros());
            while step > 0 {
                if at + step <= n && self.tree[at + step - 1] < left {
                    at += step;
                    left -= self.tree[at - 1];
                }
                step >>= 1;
            }
            (at < n).then_some(at)
        }

        /// Marks live slot `slot` deleted.
        pub fn clear(&mut self, slot: usize) {
            let mut k = slot + 1;
            while k <= self.tree.len() {
                self.tree[k - 1] -= 1;
                k += lowbit(k);
            }
        }

        /// Appends a live slot.
        pub fn push_live(&mut self) {
            // Node `k` covers `(k - lowbit(k), k]`: the new slot plus the nodes below `k` that
            // tile `(k - lowbit(k), k - 1]`.
            let k = self.tree.len() + 1;
            let (mut sum, mut j, low) = (1, k - 1, k - lowbit(k));
            while j > low {
                sum += self.tree[j - 1];
                j -= lowbit(j);
            }
            self.tree.push(sum);
        }

        /// Drops the slots from `len` on (holes at the end): no remaining node covers them.
        pub fn truncate(&mut self, len: usize) {
            self.tree.truncate(len);
        }
    }

    /// The lowest set bit of `k` (the size of the range Fenwick node `k` covers).
    fn lowbit(k: usize) -> usize {
        k.isolate_lowest_one()
    }

    #[cfg(test)]
    mod tests {
        use super::super::Value;
        use super::*;
        use std::sync::Arc;

        fn slots(live: &[bool]) -> Vec<Option<Entry>> {
            live.iter()
                .map(|&l| l.then(|| (Box::from(&b"k"[..]), Arc::new(Value::Null))))
                .collect()
        }

        /// `select` agrees with a scan after deletes, appends and truncations.
        #[test]
        fn matches_a_scan() {
            let mut live: Vec<bool> = (0..100).map(|i| i % 7 != 3).collect();
            let mut p = Positions::new(&slots(&live));
            let mut seed = 0x2545_f491_u64;
            for step in 0..2_000 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                match seed % 5 {
                    0 => {
                        live.push(true);
                        p.push_live();
                    }
                    1 => {
                        while live.last() == Some(&false) {
                            live.pop();
                        }
                        p.truncate(live.len());
                    }
                    _ => {
                        if let Some(s) = (0..live.len()).find(|&s| live[(s + step) % live.len()]) {
                            let s = (s + step) % live.len();
                            live[s] = false;
                            p.clear(s);
                        }
                    }
                }
                let want: Vec<usize> = (0..live.len()).filter(|&s| live[s]).collect();
                for (i, &s) in want.iter().enumerate() {
                    assert_eq!(p.select(i), Some(s), "member {i} after step {step}");
                }
                assert_eq!(p.select(want.len()), None);
            }
        }
    }
}

#[cfg(test)]
mod order_tests {
    use super::{array_index, Object, INDEX_THRESHOLD};
    use crate::json::value::{parse, Value};
    use std::sync::Arc;

    fn keys(o: &Object) -> Vec<String> {
        o.iter()
            .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
            .collect()
    }

    fn put(o: &mut Object, k: &str) {
        o.insert(k.as_bytes().into(), Arc::new(Value::Null));
    }

    #[test]
    fn array_indexes() {
        assert_eq!(array_index(b"0"), Some(0));
        assert_eq!(array_index(b"10"), Some(10));
        assert_eq!(array_index(b"4294967294"), Some(4294967294));
        for k in [
            "",
            "01",
            "4294967295",
            "99999999999",
            "1a",
            "-1",
            "a1",
            "1.5",
        ] {
            assert_eq!(array_index(k.as_bytes()), None, "{k}");
        }
    }

    #[test]
    fn insert_puts_indexes_first() {
        let mut o = Object::default();
        for k in ["b", "10", "a", "01", "4294967295", "4294967294", "2", "10"] {
            put(&mut o, k);
        }
        assert_eq!(
            keys(&o),
            ["2", "10", "4294967294", "b", "a", "01", "4294967295"]
        );
    }

    #[test]
    fn insert_with_an_index_and_holes() {
        let mut o = Object::default();
        let names: Vec<String> = (0..2 * INDEX_THRESHOLD).map(|i| format!("k{i}")).collect();
        for k in &names {
            put(&mut o, k);
        }
        assert!(o.remove(b"k3"));
        assert!(o.remove(b"k0"));
        put(&mut o, "7");
        put(&mut o, "3");
        put(&mut o, "k3");
        let ks = keys(&o);
        assert_eq!(&ks[..3], ["3", "7", "k1"]);
        assert_eq!(ks.last().unwrap(), "k3");
        assert_eq!(ks.len(), 2 * INDEX_THRESHOLD + 1);
        assert!(o.get(b"k20").is_some() && o.get(b"7").is_some());
        assert_eq!(o.entry_at(1).map(|(k, _)| &**k), Some(&b"7"[..]));
    }

    #[test]
    fn parse_orders_like_json_parse() {
        let v = parse(br#"{"b":1,"10":2,"a":3,"2":4,"01":5,"10":6}"#, 64).unwrap();
        let Value::Object(o) = &*v else { panic!() };
        assert_eq!(keys(o), ["2", "10", "b", "a", "01"]);
        let v = parse(br#"{"b":1,"a":{"9":1,"x":2,"1":3}}"#, 64).unwrap();
        let Value::Object(o) = &*v else { panic!() };
        let Value::Object(inner) = &**o.get(b"a").unwrap() else {
            panic!()
        };
        assert_eq!(keys(inner), ["1", "9", "x"]);
        assert_eq!(keys(o), ["b", "a"]);
    }

    #[test]
    fn parse_orders_objects_nested_past_64_levels() {
        let depth = 70;
        let mut doc = String::new();
        for _ in 0..depth {
            doc.push_str(r#"{"x":1,"5":2,"k":"#);
        }
        doc.push_str(r#"{"y":1,"2":2}"#);
        for _ in 0..depth {
            doc.push('}');
        }
        let mut v = parse(doc.as_bytes(), 1000).unwrap();
        for _ in 0..depth {
            let Value::Object(o) = &*v else { panic!() };
            assert_eq!(keys(o), ["5", "x", "k"]);
            v = o.get(b"k").unwrap().clone();
        }
        let Value::Object(o) = &*v else { panic!() };
        assert_eq!(keys(o), ["2", "y"]);
    }
}

#[cfg(test)]
mod index_member_tests {
    use super::{Object, WORK};
    use crate::json::value::Value;
    use std::sync::Arc;

    fn put(o: &mut Object, k: &str) {
        o.insert(k.as_bytes().into(), Arc::new(Value::Null));
    }

    fn work() -> usize {
        WORK.with(|w| w.get())
    }

    /// 20,000 index keys set in ascending or descending order, after a named key or not, take
    /// linear work (each one was a pass over the members, and a rebuilt hash index).
    #[test]
    fn many_index_keys_take_linear_work() {
        for named in [false, true] {
            for ascending in [true, false] {
                let mut o = Object::default();
                if named {
                    put(&mut o, "name");
                }
                let before = work();
                for i in 0..20_000 {
                    let n = if ascending { i } else { 19_999 - i };
                    put(&mut o, &n.to_string());
                }
                assert!(
                    work() - before < 1_000,
                    "named {named}, ascending {ascending}"
                );
                assert_eq!(o.len(), 20_000 + usize::from(named));
                let first: Vec<&[u8]> = o.iter().take(2).map(|(k, _)| &**k).collect();
                assert_eq!(first, [&b"0"[..], &b"1"[..]]);
                let last = o.entry_at(o.len() - 1).map(|(k, _)| &**k);
                let want: &[u8] = if named { b"name" } else { b"19999" };
                assert_eq!(last, Some(want));
                assert!(o.get(b"12345").is_some() && o.get(b"20000").is_none());
            }
        }
    }

    /// Index members and named ones mixed: lookups, positions, deletes and re-inserts.
    #[test]
    fn index_members_with_named_ones() {
        let mut o = Object::default();
        for k in ["x", "5", "y", "1", "3", "z"] {
            put(&mut o, k);
        }
        let keys = |o: &Object| -> Vec<String> {
            o.iter()
                .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
                .collect()
        };
        assert_eq!(keys(&o), ["1", "3", "5", "x", "y", "z"]);
        assert_eq!(o.entry_at(3).map(|(k, _)| &**k), Some(&b"x"[..]));
        assert!(o.remove(b"3") && !o.remove(b"3") && o.remove(b"y"));
        put(&mut o, "2");
        assert_eq!(keys(&o), ["1", "2", "5", "x", "z"]);
        assert!(o.remove(b"1") && o.remove(b"2") && o.remove(b"5"));
        assert_eq!(keys(&o), ["x", "z"]);
        assert_eq!(o.len(), 2);
        assert_eq!(o.take_values().len(), 2);
    }
}
