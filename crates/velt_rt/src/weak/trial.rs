//! Ephemeron cycles (`raw -> proxy` caches, where an entry's value holds its key strongly) and
//! the bounded trial deletion that frees them.
//!
//! On the first decrement of a key after an insert (`table::State::examine`), [`members`]
//! traces the entry's value graph (up to [`LIMIT`] objects) and records every object on a path
//! from the value back to the key, with the references the entry holds to it (the map's
//! reference to the value, and the references inside the value's graph). Their sum per object
//! is its hint (`table::Node::hint`).
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
use std::cell::Cell;
use std::ffi::c_void;

/// Objects a trial or an examination visits before it gives up (and the cycle is kept: a leak, not
/// an error). A proxy cell, its handler and the handler's closures are a handful.
pub const LIMIT: usize = 256;

/// An object on a path from an entry's value back to its key, and the references the entry
/// holds to it.
pub(super) struct Contribution {
    pub(super) obj: usize,
    pub(super) refs: u32,
    pub(super) trace: Option<TraceFn>,
}

/// Graphs up to this many objects find an object by a scan of `objs` rather than through
/// `index` (a value graph is a handful of objects: a proxy cell, its handler and closures).
const SMALL: usize = 16;

/// A bounded subgraph of counted objects and the references among them. One per thread is
/// kept between uses ([`with_graph`]), so an examination or a trial allocates nothing once the
/// buffers have grown to the graphs a program makes.
#[derive(Default)]
struct Graph {
    /// The objects' indexes, once there are more than [`SMALL`].
    index: AddrMap<u32>,
    objs: Vec<usize>,
    traces: Vec<Option<TraceFn>>,
    /// References to each object from inside the graph.
    internal: Vec<u32>,
    edges: Vec<(u32, u32)>,
    work: Vec<u32>,
    /// An object recorded but not traced through (the key, in [`members`]).
    stop: usize,
    current: u32,
    overflow: bool,
    /// [`Graph::reach`]'s adjacency (`first[i]..first[i + 1]` index `adj`) and stack.
    first: Vec<u32>,
    adj: Vec<u32>,
    stack: Vec<u32>,
    /// [`Graph::reach`]'s results, one flag per object (taken out while filled).
    live: Vec<bool>,
    to_key: Vec<bool>,
}

thread_local! {
    /// This thread's graph between uses (`None` while one is in use).
    static SCRATCH: Cell<Option<Box<Graph>>> = const { Cell::new(None) };
}

/// Runs `f` on this thread's graph, emptied, with `stop` as its stop object.
fn with_graph<R>(stop: usize, f: impl FnOnce(&mut Graph) -> R) -> R {
    let mut g = SCRATCH.with(Cell::take).unwrap_or_default();
    g.reset(stop);
    let r = f(&mut g);
    g.trim();
    SCRATCH.with(|c| c.set(Some(g)));
    r
}

impl Graph {
    fn reset(&mut self, stop: usize) {
        if self.objs.len() > SMALL {
            self.index.clear();
        }
        self.objs.clear();
        self.traces.clear();
        self.internal.clear();
        self.edges.clear();
        self.work.clear();
        self.stop = stop;
        self.current = 0;
        self.overflow = false;
    }

    /// Gives back the memory of an unusually large graph (an object with many references), so
    /// the graph kept between uses stays small.
    fn trim(&mut self) {
        const KEEP: usize = 4 * LIMIT;
        if self.edges.capacity() > KEEP {
            self.edges = Vec::new();
            self.adj = Vec::new();
        }
    }

    fn find(&self, obj: usize) -> Option<u32> {
        if self.objs.len() <= SMALL {
            self.objs.iter().position(|&o| o == obj).map(|i| i as u32)
        } else {
            self.index.get(&obj).copied()
        }
    }

    fn add(&mut self, obj: usize, trace: Option<TraceFn>) -> u32 {
        if let Some(i) = self.find(obj) {
            let t = &mut self.traces[i as usize];
            if t.is_none() {
                *t = trace;
            }
            return i;
        }
        let i = self.objs.len() as u32;
        self.objs.push(obj);
        if self.objs.len() == SMALL + 1 {
            let objs = self.objs.iter().enumerate();
            self.index.extend(objs.map(|(j, &o)| (o, j as u32)));
        } else if self.objs.len() > SMALL {
            self.index.insert(obj, i);
        }
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

    /// Marks in `seen` (one flag per object) the objects reachable from `roots` along the
    /// edges (against them when `reverse`).
    fn reach(&mut self, roots: impl Iterator<Item = u32>, reverse: bool, seen: &mut Vec<bool>) {
        self.reach_in(self.objs.len(), roots, reverse, seen);
    }

    /// [`Graph::reach`] over the first `n` objects (`objs` may be taken out meanwhile).
    fn reach_in(
        &mut self,
        n: usize,
        roots: impl Iterator<Item = u32>,
        reverse: bool,
        seen: &mut Vec<bool>,
    ) {
        let ends = |&(a, b): &(u32, u32)| if reverse { (b, a) } else { (a, b) };
        self.first.clear();
        self.first.resize(n + 1, 0);
        for e in &self.edges {
            self.first[ends(e).0 as usize + 1] += 1;
        }
        for i in 0..n {
            self.first[i + 1] += self.first[i];
        }
        self.adj.clear();
        self.adj.resize(self.edges.len(), 0);
        // `first[i]` is used as the next free slot of `i`'s list, then restored.
        for e in &self.edges {
            let (from, to) = ends(e);
            self.adj[self.first[from as usize] as usize] = to;
            self.first[from as usize] += 1;
        }
        for i in (1..=n).rev() {
            self.first[i] = self.first[i - 1];
        }
        self.first[0] = 0;
        seen.clear();
        seen.resize(n, false);
        self.stack.clear();
        self.stack.extend(roots);
        while let Some(i) = self.stack.pop() {
            if !std::mem::replace(&mut seen[i as usize], true) {
                let (a, b) = (self.first[i as usize], self.first[i as usize + 1]);
                self.stack.extend(&self.adj[a as usize..b as usize]);
            }
        }
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
    with_graph(key as usize, |g| {
        let v = g.add(value as usize, Some(trace));
        g.internal[v as usize] += 1; // the map's reference
        while let Some(i) = g.work.pop() {
            if g.overflow {
                return Vec::new();
            }
            g.expand(i);
        }
        let Some(k) = g.find(key as usize) else {
            return Vec::new();
        };
        if g.overflow {
            return Vec::new();
        }
        let mut to_key = std::mem::take(&mut g.to_key);
        g.reach(std::iter::once(k), true, &mut to_key);
        let members = (0..g.objs.len())
            .filter(|&i| to_key[i])
            .map(|i| Contribution {
                obj: g.objs[i],
                refs: g.internal[i],
                trace: g.traces[i],
            })
            .collect();
        g.to_key = to_key;
        members
    })
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
    with_graph(0, |g| run_in(s, g, start))
}

fn run_in(s: &State, g: &mut Graph, start: usize) -> Trial {
    g.add(start, s.nodes.get(&start).and_then(|n| n.trace));
    while let Some(i) = g.work.pop() {
        if g.overflow {
            return Trial::default();
        }
        // SAFETY: every object in the graph is live: it is recorded, or reached through strong
        // references from a recorded object.
        unsafe { g.expand(i) };
        add_entry_edges(s, g, i);
    }
    if g.overflow {
        return Trial::default();
    }
    let (mut live, mut to_key) = (std::mem::take(&mut g.live), std::mem::take(&mut g.to_key));
    // The roots are found from `objs` and `internal`, which `reach` reads but leaves alone.
    let (objs, internal) = (std::mem::take(&mut g.objs), std::mem::take(&mut g.internal));
    let outside = (0..objs.len() as u32).filter(|&i| {
        // SAFETY: as above.
        let count = unsafe { *rc_word(objs[i as usize] as *mut u8) & RC_COUNT };
        count > u64::from(internal[i as usize])
    });
    g.reach_in(objs.len(), outside, false, &mut live);
    let keys = (0..objs.len() as u32).filter(|&i| {
        s.nodes
            .get(&objs[i as usize])
            .is_some_and(|n| !n.keyed_in.is_empty())
    });
    g.reach_in(objs.len(), keys, true, &mut to_key);
    (g.objs, g.internal) = (objs, internal);
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
    (g.live, g.to_key) = (live, to_key);
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
        // A key's own trace glue (the keys of a dynamic key type differ, and so do a proxiable
        // key type's typed proxies, which are `Proxy` objects), else the map's.
        let trace = s.nodes.get(&k).and_then(|n| n.trace);
        g.add(k, trace.or(s.map(m).key_trace));
    }
}
