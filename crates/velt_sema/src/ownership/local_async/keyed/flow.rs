//! Which keys reach which stores, for [`super`]: a summary says only which parameter of a function
//! is stored into from which ([`super::apply`]), and each such pair is a set of keys, a node of
//! this graph. A store by a key seeds a node; a call of a function adds an edge from the callee's
//! set to the caller's; a store into a value of a type is a sink. Keys never change which functions
//! run, so the graph is complete when the worklist empties, and only then do keys flow over it,
//! once: the nodes of a cycle (functions calling each other through a join) become one set, and a
//! set a node passes on unchanged is shared, not copied. A program of N keys and N functions holds
//! few sets of N keys, not N of them.

use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

use super::super::fed::{adt_of, declaring, Field};
use super::super::targets::{held_adts, Literals};
use super::apply::Node;
use super::{Key, Site, Src};

/// The key id of `Key::Any`.
const ANY: u32 = 0;

/// The graph of key sets.
#[derive(Default)]
pub(super) struct Flows {
    ids: FxHashMap<(Node, u32, Src), u32>,
    succ: Vec<Vec<u32>>,
    /// A node a store makes holds that store's key.
    seed: Vec<Option<u32>>,
    edges: FxHashSet<(u32, u32)>,
    /// In the order they were found: the first to feed a field is the one its error names.
    sinks: Vec<(u32, TyId, Site)>,
    sunk: FxHashSet<(u32, TyId)>,
    /// The ids of constant keys, from 1 ([`ANY`] is 0).
    names: FxHashMap<String, u32>,
}

impl Flows {
    fn node(&mut self, seed: Option<u32>) -> u32 {
        self.succ.push(vec![]);
        self.seed.push(seed);
        self.succ.len() as u32 - 1
    }

    /// The set of keys summary `n` stores by into parameter `q` from `src`.
    pub(super) fn set(&mut self, n: Node, q: u32, src: Src) -> u32 {
        if let Some(&id) = self.ids.get(&(n, q, src)) {
            return id;
        }
        let id = self.node(None);
        self.ids.insert((n, q, src), id);
        id
    }

    /// A node holding `key`, for one store.
    pub(super) fn seeded(&mut self, key: &Key) -> u32 {
        let id = match key {
            Key::Any => ANY,
            Key::Name(n) => match self.names.get(n.as_str()) {
                Some(&id) => id,
                None => {
                    let id = self.names.len() as u32 + 1;
                    self.names.insert(n.clone(), id);
                    id
                }
            },
        };
        self.node(Some(id))
    }

    /// The keys of `from` are also keys of `to`.
    pub(super) fn edge(&mut self, from: u32, to: u32) {
        if from != to && self.edges.insert((from, to)) {
            self.succ[from as usize].push(to);
        }
    }

    /// The keys of `from` are stored into a value of type `ty`, at `site`.
    pub(super) fn sink(&mut self, from: u32, ty: TyId, site: &Site) {
        if self.sunk.insert((from, ty)) {
            self.sinks.push((from, ty, site.clone()));
        }
    }

    /// The fields the sinks feed, each with where it was first fed: a store into a known object
    /// before one into any object.
    pub(super) fn fields(self, cx: &mut Ctx, lits: &Literals) -> FxHashMap<Field, Site> {
        let words = (self.names.len() + 1).div_ceil(64);
        let (comp, ncomps) = components(&self.succ);
        let mut work = self.succ.len() as u64;
        let mut has_sink = vec![false; ncomps];
        for &(from, _, _) in &self.sinks {
            has_sink[comp[from as usize] as usize] = true;
        }
        // The components each one passes keys to, sorted.
        let mut out: Vec<(u32, u32)> = vec![];
        for (n, succ) in self.succ.iter().enumerate() {
            out.extend((succ.iter()).filter_map(|&s| {
                let (a, b) = (comp[n], comp[s as usize]);
                (a != b).then_some((a, b))
            }));
        }
        out.sort_unstable();
        out.dedup();
        work += out.len() as u64;
        let mut starts = vec![0usize; ncomps + 1];
        for &(a, _) in &out {
            starts[a as usize + 1] += 1;
        }
        for c in 0..ncomps {
            starts[c + 1] += starts[c];
        }
        // Components are numbered successors first. Keys matter only on their way to a sink.
        let mut useful = has_sink.clone();
        for c in 0..ncomps {
            let succs = &out[starts[c]..starts[c + 1]];
            useful[c] |= succs.iter().any(|&(_, s)| useful[s as usize]);
        }
        let mut incoming: Vec<Option<Set>> = vec![None; ncomps];
        for (n, seed) in self.seed.iter().enumerate() {
            // A seeded node has no edge into it: it is its own component.
            if let Some(k) = *seed {
                incoming[comp[n] as usize] = Some(Set::One(k));
            }
        }
        let mut at: Vec<Option<Set>> = vec![None; ncomps];
        for c in (0..ncomps).rev() {
            let Some(set) = incoming[c].take().filter(|_| useful[c]) else {
                continue;
            };
            if has_sink[c] {
                at[c] = Some(set.clone());
            }
            let mut set = Some(set);
            let mut succs = (out[starts[c]..starts[c + 1]].iter())
                .map(|&(_, s)| s as usize)
                .filter(|&s| useful[s])
                .peekable();
            while let Some(s) = succs.next() {
                // The last one takes it, so that a chain adds to one set in place.
                let keys = match succs.peek() {
                    Some(_) => set.clone(),
                    None => set.take(),
                };
                if let Some(keys) = keys {
                    work += union(&mut incoming[s], keys, words);
                }
            }
        }
        let mut feeding = Feeding {
            cx,
            lits,
            names: &self.names,
            words,
            tables: FxHashMap::default(),
            visited: FxHashSet::default(),
            fed: FxHashMap::default(),
            work: 0,
        };
        for (i, &(from, ty, _)) in self.sinks.iter().enumerate() {
            if let Some(set) = &at[comp[from as usize] as usize] {
                feeding.feed(set, ty, i as u32);
            }
        }
        crate::check_work::add_keyed_work(work + feeding.work + self.sinks.len() as u64);
        let fed = feeding.fed;
        let sinks = self.sinks;
        (fed.into_iter())
            .map(|(f, (i, _))| (f, sinks[i as usize].2.clone()))
            .collect()
    }
}

/// A set of key ids: most sets a store makes hold one.
#[derive(Clone)]
enum Set {
    One(u32),
    Many(Rc<Vec<u64>>),
}

/// Add `keys` to `to`; returns the words combined.
fn union(to: &mut Option<Set>, keys: Set, words: usize) -> u64 {
    let Some(have) = to else {
        *to = Some(keys);
        return 1;
    };
    match (&mut *have, keys) {
        (Set::One(a), Set::One(b)) if *a == b => 0,
        (Set::One(a), Set::One(b)) => {
            let mut bits = vec![0u64; words];
            insert(&mut bits, *a);
            insert(&mut bits, b);
            *have = Set::Many(Rc::new(bits));
            words as u64
        }
        (Set::One(a), Set::Many(mut bits)) => {
            let a = *a;
            insert(Rc::make_mut(&mut bits).as_mut_slice(), a);
            *have = Set::Many(bits);
            1
        }
        (Set::Many(bits), Set::One(b)) => {
            insert(Rc::make_mut(bits).as_mut_slice(), b);
            1
        }
        (Set::Many(a), Set::Many(mut b)) => {
            if Rc::ptr_eq(a, &b) {
                return 0;
            }
            // The one nothing else holds is changed in place.
            if Rc::strong_count(a) > 1 && Rc::strong_count(&b) == 1 {
                std::mem::swap(a, &mut b);
            }
            for (x, y) in Rc::make_mut(a).iter_mut().zip(b.iter()) {
                *x |= *y;
            }
            words as u64
        }
    }
}

fn insert(bits: &mut [u64], k: u32) {
    bits[k as usize / 64] |= 1 << (k % 64);
}

fn holds(bits: &[u64], k: u32) -> bool {
    bits[k as usize / 64] >> (k % 64) & 1 != 0
}

/// The strongly connected components of a graph, found iteratively (a chain of calls may be deep),
/// and how many there are: a component's successors have lower numbers.
fn components(succ: &[Vec<u32>]) -> (Vec<u32>, usize) {
    const NONE: u32 = u32::MAX;
    let n = succ.len();
    let (mut index, mut low) = (vec![NONE; n], vec![0u32; n]);
    let mut comp = vec![NONE; n];
    let (mut next, mut ncomps) = (0u32, 0u32);
    let mut stack: Vec<u32> = vec![];
    let mut calls: Vec<(u32, u32)> = vec![];
    for root in 0..n as u32 {
        if index[root as usize] != NONE {
            continue;
        }
        index[root as usize] = next;
        low[root as usize] = next;
        next += 1;
        stack.push(root);
        calls.push((root, 0));
        while let Some(&mut (v, ref mut i)) = calls.last_mut() {
            let v = v as usize;
            if let Some(&w) = succ[v].get(*i as usize) {
                *i += 1;
                let u = w as usize;
                if index[u] == NONE {
                    index[u] = next;
                    low[u] = next;
                    next += 1;
                    stack.push(w);
                    calls.push((w, 0));
                } else if comp[u] == NONE {
                    low[v] = low[v].min(index[u]);
                }
                continue;
            }
            calls.pop();
            if let Some(&(p, _)) = calls.last() {
                low[p as usize] = low[p as usize].min(low[v]);
            }
            if low[v] == index[v] {
                while let Some(w) = stack.pop() {
                    comp[w as usize] = ncomps;
                    if w as usize == v {
                        break;
                    }
                }
                ncomps += 1;
            }
        }
    }
    (comp, ncomps as usize)
}

/// The fields a store by key into a value of one type may feed.
struct Table {
    known: bool,
    all: Vec<(Option<u32>, Field)>,
    by_key: FxHashMap<u32, Vec<Field>>,
    /// The keys looked at, for a type of more fields than a set has words (empty otherwise).
    seen: Vec<u64>,
    all_done: bool,
}

/// Fields fed so far, with the sink that fed each first and whether its object was known.
type Fed = FxHashMap<Field, (u32, bool)>;

struct Feeding<'a, 'm> {
    cx: &'a mut Ctx<'m>,
    lits: &'a Literals,
    names: &'a FxHashMap<String, u32>,
    words: usize,
    tables: FxHashMap<TyId, Table>,
    visited: FxHashSet<(*const Vec<u64>, TyId)>,
    fed: Fed,
    work: u64,
}

impl Feeding<'_, '_> {
    /// Feed the fields `set`'s keys name of a value of type `ty`, for sink `sink`. A set of many
    /// keys is looked at once per type; a type of few fields is looked up by field, one of many
    /// by key, so neither many keys nor many fields multiply.
    fn feed(&mut self, set: &Set, ty: TyId, sink: u32) {
        let ty = match self.cx.ty.kind(ty) {
            TyKind::Option(inner, _) => *inner,
            _ => ty,
        };
        if let Set::Many(bits) = set {
            if !self.visited.insert((Rc::as_ptr(bits), ty)) {
                return;
            }
        }
        if !self.tables.contains_key(&ty) {
            let table = self.table(ty);
            self.tables.insert(ty, table);
        }
        let Some(t) = self.tables.get_mut(&ty) else {
            return;
        };
        let (fed, work) = (&mut self.fed, &mut self.work);
        let big = !t.seen.is_empty();
        match set {
            Set::One(k) if *k != ANY => {
                if !big || !holds(&t.seen, *k) {
                    if big {
                        insert(&mut t.seen, *k);
                    }
                    by_key(t, *k, sink, fed, work);
                }
            }
            Set::Many(bits) if !holds(bits, ANY) && big => {
                for w in 0..bits.len() {
                    *work += 1;
                    let mut new = bits[w] & !t.seen[w];
                    t.seen[w] |= new;
                    while new != 0 {
                        let k = w as u32 * 64 + new.trailing_zeros();
                        new &= new - 1;
                        by_key(t, k, sink, fed, work);
                    }
                }
            }
            Set::Many(bits) if !holds(bits, ANY) => {
                *work += t.all.len() as u64;
                for (k, f) in &t.all {
                    if k.is_some_and(|k| holds(bits, k)) {
                        record(fed, *f, sink, t.known);
                    }
                }
            }
            _ if t.all_done => {}
            _ => {
                t.all_done = true;
                *work += t.all.len() as u64;
                for (_, f) in &t.all {
                    record(fed, *f, sink, t.known);
                }
            }
        }
    }

    /// What a store into a value of type `ty` may feed: a class and its subclasses, an interface's
    /// implementations, and for an open type what the program converts to one and what that holds.
    fn table(&mut self, ty: TyId) -> Table {
        let cx = &mut *self.cx;
        let mut types = vec![];
        let mut known = true;
        if let Some(d) = adt_of(cx, ty) {
            types.push(d);
        } else if let (TyKind::Dyn(iface, _), None) = (cx.ty.kind(ty), cx.dynamic(ty)) {
            let iface = *iface;
            for i in cx.impls.iter().filter(|i| i.iface == iface) {
                types.extend(adt_of(cx, i.ty));
            }
        } else {
            known = false;
            types = held_adts(cx, self.lits);
        }
        let mut t = Table {
            known,
            all: vec![],
            by_key: FxHashMap::default(),
            seen: vec![],
            all_done: false,
        };
        // A value of class `d` may be of any of its subclasses.
        for c in types.into_iter().flat_map(|d| self.lits.below(d)) {
            let Some(a) = cx.adt(c) else { continue };
            for (k, fd) in a.fields.iter().enumerate() {
                if let Some(f) = declaring(cx, c, k as u32) {
                    let id = self.names.get(fd.name.as_str()).copied();
                    t.all.push((id, f));
                    if let Some(id) = id {
                        t.by_key.entry(id).or_default().push(f);
                    }
                }
            }
        }
        self.work += t.all.len() as u64;
        if t.all.len() > self.words {
            t.seen = vec![0; self.words];
        }
        t
    }
}

/// Key `k` reaches the fields of a type that it names.
fn by_key(t: &Table, k: u32, sink: u32, fed: &mut Fed, work: &mut u64) {
    *work += 1;
    for f in t.by_key.get(&k).into_iter().flatten() {
        *work += 1;
        record(fed, *f, sink, t.known);
    }
}

/// Field `f` is fed by sink `sink`: its error names a store into a known object over one into any
/// object, and the first of each.
fn record(fed: &mut Fed, f: Field, sink: u32, known: bool) {
    match fed.get(&f) {
        Some((_, true)) => {}
        Some((_, false)) if !known => {}
        _ => {
            fed.insert(f, (sink, known));
        }
    }
}
