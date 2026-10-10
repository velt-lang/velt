//! Ephemeron cycles (`raw -> proxy` caches, where an entry's value holds its key strongly) and
//! the bounded trial deletion that frees them.
//!
//! At insert, [`members`] traces the new value's graph (up to [`LIMIT`] objects) and records
//! every object on a path from the value back to the key, with the references the entry holds
//! to it (the map's reference to the value, and the references inside the value's graph). Their
//! sum per object is its hint (`table::Node::hint`).
//!
//! When a release leaves an object's count at or below its hint, [`run`] runs a trial
//! deletion in the style of Bacon and Rajan's synchronous cycle collection, restricted to one
//! subgraph: it collects the objects reachable from the released one through strong references
//! and through map entries (a key leads to its value), plus the keys of the entries the objects
//! belong to, and counts the references among them. An object referenced more often than that is
//! referenced from outside, so it is live, and so is everything it reaches (an entry's value is
//! reachable from its key: the ephemeron rule). Keys left dead have their entries deleted, and
//! releasing those values frees the cycle through the ordinary release paths. A trial that
//! finds the cycle live records, for each live object on a path back to a key, the references
//! it counted (`Node::observed`), so a later release to that level tries again even when the
//! value graph changed after insert.
//!
//! Every mistake this can make is a leak, never a use-after-free: the only action is deleting
//! entries of keys found dead, and deleting an entry only gives up the map's own reference.

use super::table::{AddrMap, MapId, State};
use super::{rc_word, TraceFn, RC_COUNT};
use std::ffi::c_void;

/// Objects a trial or an insert visits before it gives up (and the cycle is kept: a leak, not
/// an error). A proxy cell, its handler and the handler's closures are a handful.
pub const LIMIT: usize = 256;

/// An object on a path from an entry's value back to its key, and the references the entry
/// holds to it.
pub(super) struct Contribution {
    pub(super) obj: usize,
    pub(super) refs: u32,
    pub(super) trace: Option<TraceFn>,
}

/// A bounded subgraph of counted objects and the references among them.
struct Graph {
    index: AddrMap<u32>,
    objs: Vec<usize>,
    traces: Vec<Option<TraceFn>>,
    /// References to each object from inside the graph.
    internal: Vec<u32>,
    edges: Vec<(u32, u32)>,
    work: Vec<u32>,
    /// An object recorded but not traced through (the key, at insert).
    stop: usize,
    current: u32,
    overflow: bool,
}

impl Graph {
    fn new(stop: usize) -> Self {
        Graph {
            index: AddrMap::default(),
            objs: Vec::new(),
            traces: Vec::new(),
            internal: Vec::new(),
            edges: Vec::new(),
            work: Vec::new(),
            stop,
            current: 0,
            overflow: false,
        }
    }

    fn add(&mut self, obj: usize, trace: Option<TraceFn>) -> u32 {
        if let Some(&i) = self.index.get(&obj) {
            let t = &mut self.traces[i as usize];
            if t.is_none() {
                *t = trace;
            }
            return i;
        }
        let i = self.objs.len() as u32;
        self.index.insert(obj, i);
        self.objs.push(obj);
        self.traces.push(trace);
        self.internal.push(0);
        if obj != self.stop {
            self.work.push(i);
        }
        self.overflow |= self.objs.len() > LIMIT;
        i
    }

    fn edge(&mut self, from: u32, to: u32) {
        self.internal[to as usize] += 1;
        self.edges.push((from, to));
    }

    /// Records the strong references object `i` holds.
    ///
    /// # Safety
    /// Object `i` is live and its trace glue (if any) is its type's.
    unsafe fn expand(&mut self, i: u32) {
        if let Some(trace) = self.traces[i as usize] {
            self.current = i;
            (trace.0)(
                self.objs[i as usize] as *mut u8,
                visit,
                self as *mut Graph as *mut c_void,
            );
        }
    }

    /// Objects reachable from `roots` along the edges.
    fn reach(&self, roots: impl Iterator<Item = u32>, reverse: bool) -> Vec<bool> {
        let mut adj = vec![Vec::new(); self.objs.len()];
        for &(a, b) in &self.edges {
            let (from, to) = if reverse { (b, a) } else { (a, b) };
            adj[from as usize].push(to);
        }
        let mut seen = vec![false; self.objs.len()];
        let mut stack: Vec<u32> = roots.collect();
        while let Some(i) = stack.pop() {
            if !std::mem::replace(&mut seen[i as usize], true) {
                stack.extend(&adj[i as usize]);
            }
        }
        seen
    }
}

unsafe extern "C" fn visit(ctx: *mut c_void, child: *mut u8, trace: Option<TraceFn>) {
    let g = &mut *(ctx as *mut Graph);
    if child.is_null() {
        return;
    }
    let j = g.add(child as usize, trace);
    g.edge(g.current, j);
}

/// The objects of `value`'s graph on a path back to `key`, with the entry's references to each
/// (none when the value never refers to the key, or its graph is past [`LIMIT`]).
///
/// # Safety
/// `key` and `value` are live counted objects; `trace` is `value`'s trace glue.
pub(super) unsafe fn members(key: *mut u8, value: *mut u8, trace: TraceFn) -> Vec<Contribution> {
    let mut g = Graph::new(key as usize);
    let v = g.add(value as usize, Some(trace));
    g.internal[v as usize] += 1; // the map's reference
    while let Some(i) = g.work.pop() {
        if g.overflow {
            return Vec::new();
        }
        g.expand(i);
    }
    let Some(&k) = g.index.get(&(key as usize)) else {
        return Vec::new();
    };
    if g.overflow {
        return Vec::new();
    }
    let to_key = g.reach(std::iter::once(k), true);
    (0..g.objs.len())
        .filter(|&i| to_key[i])
        .map(|i| Contribution {
            obj: g.objs[i],
            refs: g.internal[i],
            trace: g.traces[i],
        })
        .collect()
}

/// What a trial deletion found.
#[derive(Default)]
pub(super) struct Trial {
    /// The entries `(map, key)` of every key that nothing outside the subgraph keeps alive.
    pub(super) dead: Vec<(MapId, usize)>,
    /// The live objects on a path back to a key, with the references to each from the subgraph.
    pub(super) observed: Vec<Contribution>,
}

/// A trial deletion from the recorded object `start` (finding nothing if the subgraph is past
/// [`LIMIT`]).
pub(super) fn run(s: &State, start: usize) -> Trial {
    let mut g = Graph::new(0);
    g.add(start, s.nodes.get(&start).and_then(|n| n.trace));
    while let Some(i) = g.work.pop() {
        if g.overflow {
            return Trial::default();
        }
        // SAFETY: every object in the graph is live: it is recorded, or reached through strong
        // references from a recorded object.
        unsafe { g.expand(i) };
        add_entry_edges(s, &mut g, i);
    }
    if g.overflow {
        return Trial::default();
    }
    let outside = (0..g.objs.len() as u32).filter(|&i| {
        // SAFETY: as above.
        let count = unsafe { *rc_word(g.objs[i as usize] as *mut u8) & RC_COUNT };
        count > u64::from(g.internal[i as usize])
    });
    let live = g.reach(outside, false);
    let keys = (0..g.objs.len() as u32).filter(|&i| {
        let obj = g.objs[i as usize];
        s.nodes.get(&obj).is_some_and(|n| !n.keyed_in.is_empty())
    });
    let to_key = g.reach(keys, true);
    let mut trial = Trial::default();
    for (i, &obj) in g.objs.iter().enumerate() {
        if !live[i] {
            if let Some(node) = s.nodes.get(&obj) {
                trial.dead.extend(node.keyed_in.iter().map(|&m| (m, obj)));
            }
        } else if to_key[i] {
            let (refs, trace) = (g.internal[i], g.traces[i]);
            trial.observed.push(Contribution { obj, refs, trace });
        }
    }
    trial
}

/// The map references of object `i`: to its entries' traced values (as a key) and from the keys of the
/// entries it belongs to (whose own edges then count those references as internal).
fn add_entry_edges(s: &State, g: &mut Graph, i: u32) {
    let obj = g.objs[i as usize];
    let Some(node) = s.nodes.get(&obj) else {
        return;
    };
    for &m in &node.keyed_in {
        // Only traced values join the graph: a value left out (untraced, or 0) can only make
        // what it refers to look referenced from outside, which keeps it alive (safe).
        let map = s.map(m);
        let Some(trace) = map.value_trace else {
            continue;
        };
        match map.entries.get(&obj) {
            Some(e) if e.value != 0 => {
                let j = g.add(e.value as usize, Some(trace));
                g.edge(i, j);
            }
            _ => {}
        }
    }
    for &(m, k) in &node.member_of {
        g.add(k, s.map(m).key_trace);
    }
}
