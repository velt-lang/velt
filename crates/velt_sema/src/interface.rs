//! Interface summaries (docs/internals/design/dev-warm-front-end.md step 3, "B. Interface
//! summaries"): what other code observes of each function of a checked program, as a hash per
//! function, and the program-wide facts a body can change, as a hash per kind of fact. Two
//! versions of a program whose summaries differ only in the functions a save edited (and their
//! closures) have the same interfaces: nothing that depends on those functions needs to be
//! checked or lowered again (early cutoff).
//!
//! A function's interface is its signature as sema finished it: type parameters, parameter types
//! and pass modes (ownership), which parameters it modifies (write effects), the result type
//! (inferred or written), the error set, async and generator, `this`, its captures' modes, and
//! what the program-wide tables say of it (a fresh closure, a lock-free callback, a CPU target,
//! its value's `name` and `length`). Program-wide facts are the
//! declarations other than functions (fields, variants, interface slots, externs, globals, with
//! the flags sema infers on them from every body, such as a field being assigned somewhere), the
//! interface implementations, the counted object types, the module state, the entry, the std
//! definitions the program reaches, the generated instances and the program's flags.
//!
//! Everything is hashed by structure and by name, never by `TyId`, `DefId` or `Span`: those
//! differ between two checks of programs that differ in one body (types are interned and
//! definitions added in checking order, and an edit moves the code after it). A closure is named
//! after the function it is in and its number there (`f::{closure#0}`), so its name survives an
//! edit of another function. Not covered, by design: initializer expressions of fields and
//! globals (declarations: change detection classifies their edits as other changes), and facts
//! sema keeps to itself without putting them in the HIR (which functions return fresh values,
//! `fresh_returns`), which the per-function passes read from the kept checking context.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::hir::{self, Def, DefId, FnDef, TyId, TyKind};

/// The interfaces of a checked program's functions and its program-wide facts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Per function, by its qualified name (`main`, `User.greet`, `std/fs::readFile`,
    /// `main::{closure#0}`).
    fns: FxHashMap<String, u64>,
    /// Per kind of fact, in a fixed order.
    facts: Vec<(&'static str, u64)>,
    /// The std functions lazy std left unchecked (stubs): a body edit that makes the program
    /// reach one is a change of the `reached std` fact, not of the function's interface.
    unchecked: FxHashSet<String>,
}

/// One difference between two summaries.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    /// A function whose interface changed, or that one version has and the other doesn't.
    Fn(String),
    /// A kind of program-wide fact (`declarations`, `reached std`, ...).
    Fact(&'static str),
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Change::Fn(name) => write!(f, "{name}"),
            Change::Fact(what) => write!(f, "the program's {what}"),
        }
    }
}

impl Summary {
    /// The summary of a checked program.
    pub fn of(p: &hir::Program) -> Summary {
        let mut h = Hashing::new(p);
        let mut fns = FxHashMap::default();
        let mut unchecked = FxHashSet::default();
        for (i, d) in p.defs.iter().enumerate() {
            let Def::Fn(f) = d else { continue };
            let id = DefId(i as u32);
            match p.unchecked.contains(&id) {
                true => unchecked.insert(f.name.clone()),
                false => fns.insert(f.name.clone(), h.function(id, f)).is_some(),
            };
        }
        let facts = vec![
            ("declarations", h.declarations()),
            ("implementations", h.implementations()),
            ("counted types", h.counted()),
            ("module state", h.module_state()),
            ("entry", h.entry()),
            ("reached std", h.reached()),
            ("generated instances", h.generated()),
            ("flags", h.flags()),
        ];
        Summary {
            fns,
            facts,
            unchecked,
        }
    }

    /// How `newer` differs from `self`: facts in their fixed order, then functions by name. A
    /// std function one version checked and the other left unchecked is part of the `reached
    /// std` fact.
    pub fn changes(&self, newer: &Summary) -> Vec<Change> {
        let mut out: Vec<Change> = (self.facts.iter().zip(&newer.facts))
            .filter(|(a, b)| a != b)
            .map(|(a, _)| Change::Fact(a.0))
            .collect();
        // A closure of an unchecked function has no definition in that version.
        let reached = |name: &String| {
            let outer = name
                .find("::{closure#")
                .map_or(name.as_str(), |at| &name[..at]);
            self.unchecked.contains(outer) || newer.unchecked.contains(outer)
        };
        let mut fns: Vec<Change> = (self.fns.iter())
            .filter(|(name, hash)| newer.fns.get(*name) != Some(hash))
            .map(|(name, _)| name)
            .chain((newer.fns.keys()).filter(|name| !self.fns.contains_key(*name)))
            .filter(|name| !reached(name))
            .map(|name| Change::Fn(name.clone()))
            .collect();
        fns.sort();
        out.extend(fns);
        out
    }

    /// The number of functions summarized.
    pub fn functions(&self) -> usize {
        self.fns.len()
    }
}

/// Is `name` the qualified name of a closure in the function `outer` (at any depth)?
pub fn is_closure_of(name: &str, outer: &str) -> bool {
    name.strip_prefix(outer)
        .is_some_and(|rest| rest.starts_with("::{closure#"))
}

/// Hashes of a program's types (memoized) and definitions.
struct Hashing<'p> {
    p: &'p hir::Program,
    tys: FxHashMap<TyId, u64>,
}

fn finish(x: impl Hash) -> u64 {
    let mut s = DefaultHasher::new();
    x.hash(&mut s);
    s.finish()
}

/// Hashes of (name, hash) pairs, independent of their order.
fn unordered(mut items: Vec<(String, u64)>) -> u64 {
    items.sort();
    finish(items)
}

impl<'p> Hashing<'p> {
    fn new(p: &'p hir::Program) -> Self {
        Self {
            p,
            tys: FxHashMap::default(),
        }
    }

    /// A definition's name: what stands for it in a hash.
    fn name(&self, d: DefId) -> &'p str {
        match self.p.defs.get(d.0 as usize) {
            Some(Def::Fn(f)) => &f.name,
            Some(Def::ExternFn(f)) => &f.name,
            Some(Def::Adt(a)) => &a.name,
            Some(Def::Enum(e)) => &e.name,
            Some(Def::Global(g)) => &g.name,
            Some(Def::Interface(i)) => &i.name,
            None => "",
        }
    }

    fn names(&self, ds: impl IntoIterator<Item = DefId>) -> Vec<&'p str> {
        ds.into_iter().map(|d| self.name(d)).collect()
    }

    fn opt_name(&self, d: Option<DefId>) -> Option<&'p str> {
        d.map(|d| self.name(d))
    }

    fn ty(&mut self, t: TyId) -> u64 {
        if let Some(&h) = self.tys.get(&t) {
            return h;
        }
        let mut s = DefaultHasher::new();
        let kind = self.p.types.kind(t);
        std::mem::discriminant(kind).hash(&mut s);
        match kind {
            TyKind::Int(i) => i.hash(&mut s),
            TyKind::Float(f) => f.hash(&mut s),
            TyKind::Bool
            | TyKind::Str
            | TyKind::Unit
            | TyKind::Never
            | TyKind::Error
            | TyKind::Symbol => {}
            TyKind::Adt(d, args) | TyKind::Dyn(d, args) => {
                self.name(*d).hash(&mut s);
                self.tys_into(args, &mut s);
            }
            TyKind::Array(x) | TyKind::Shared(x) => self.ty(*x).hash(&mut s),
            TyKind::Map(a, b) | TyKind::Result(a, b) | TyKind::Promise(a, b) => {
                (self.ty(*a), self.ty(*b)).hash(&mut s)
            }
            TyKind::Tuple(xs) => self.tys_into(xs, &mut s),
            TyKind::Option(x, n) => (self.ty(*x), n).hash(&mut s),
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            } => {
                self.tys_into(params, &mut s);
                (self.ty(*ret), self.ty(*throws), rest).hash(&mut s);
            }
            TyKind::Closure(d) => self.name(*d).hash(&mut s),
            TyKind::Param(n) => n.hash(&mut s),
            TyKind::Literal(v) => v.hash(&mut s),
            TyKind::Simd(v) => v.hash(&mut s),
            TyKind::Op(op, args) => {
                op.hash(&mut s);
                self.tys_into(args, &mut s);
            }
        }
        let h = s.finish();
        self.tys.insert(t, h);
        h
    }

    fn tys_into(&mut self, ts: &[TyId], s: &mut DefaultHasher) {
        ts.len().hash(s);
        for &t in ts {
            self.ty(t).hash(s);
        }
    }

    fn tys(&mut self, ts: &[TyId]) -> Vec<u64> {
        ts.iter().map(|&t| self.ty(t)).collect()
    }

    fn opt_ty(&mut self, t: Option<TyId>) -> Option<u64> {
        t.map(|t| self.ty(t))
    }

    /// What other code observes of function `d`.
    fn function(&mut self, d: DefId, f: &FnDef) -> u64 {
        let p = self.p;
        let mut s = DefaultHasher::new();
        f.generics.hash(&mut s);
        for (i, param) in f.params.iter().enumerate() {
            let local = f.body.locals.get(i);
            let effects = local.map(|l| (l.mutable, l.boxed));
            (self.ty(param.ty), param.mode, effects).hash(&mut s);
        }
        (
            self.ty(f.ret),
            self.opt_ty(f.throws),
            self.opt_ty(f.self_ty),
        )
            .hash(&mut s);
        (
            f.is_async,
            f.is_generator,
            f.shares_captures,
            f.changes_held,
        )
            .hash(&mut s);
        for c in &f.captures {
            (c.mode, c.share).hash(&mut s);
        }
        let fresh = p.fresh_closures.get(&d).map(|c| match *c {
            hir::FreshClosure::New(t) => (0u8, self.ty(t), 0),
            hir::FreshClosure::Several(t) => (1, self.ty(t), 0),
            hir::FreshClosure::Converts { from, to } => (2, self.ty(from), self.ty(to)),
        });
        fresh.hash(&mut s);
        p.lock_free_callbacks.contains(&d).hash(&mut s);
        p.cpu_targets.get(&d).map(|t| format!("{t:?}")).hash(&mut s);
        let shape = p.fn_shapes.get(&f.span);
        let shape = shape.map(|sh| (&sh.name, sh.length, self.opt_name(sh.wraps)));
        shape.hash(&mut s);
        s.finish()
    }

    fn fields(&mut self, fields: &[hir::FieldDef]) -> Vec<u64> {
        (fields.iter())
            .map(|fd| {
                let flags = (fd.optional, fd.private, fd.presence, fd.hidden);
                finish((&fd.name, self.ty(fd.ty), fd.default.is_some(), flags))
            })
            .collect()
    }

    /// Every definition other than a function, by name.
    fn declarations(&mut self) -> u64 {
        let p = self.p;
        let mut items = Vec::new();
        for d in &p.defs {
            let (name, hash) = match d {
                Def::Fn(_) => continue,
                Def::ExternFn(f) => {
                    let sig = (self.tys(&f.params), self.ty(f.ret), f.is_async);
                    let how = (&f.system, f.host.is_some(), f.c.is_some());
                    (&f.name, finish((0u8, &f.symbol, sig, how)))
                }
                Def::Adt(a) => {
                    let kind = format!("{:?}", a.kind);
                    let flags = (a.is_copy, a.declared_struct, a.c_struct);
                    let more = (a.private_fields, a.opaque, a.assigned);
                    let hooks = [a.ctor, a.dispose, a.to_string, a.to_json, a.inspect];
                    let hooks: Vec<_> = hooks.iter().map(|h| self.opt_name(*h)).collect();
                    let shape = (kind, a.generics, self.fields(&a.fields), flags, more);
                    let base = self.opt_ty(a.base);
                    let vtable = self.names(a.vtable.iter().copied());
                    (
                        &a.name,
                        finish((1u8, shape, base, hooks, a.inherited_inits.len(), vtable)),
                    )
                }
                Def::Enum(e) => {
                    let variants: Vec<_> = (e.variants.iter())
                        .map(|v| {
                            let payload = self.tys(&v.payload);
                            finish((&v.name, payload, v.discriminant, &v.str_value))
                        })
                        .collect();
                    let flags = (e.generics, e.is_copy, e.is_union);
                    (&e.name, finish((2u8, variants, flags)))
                }
                Def::Interface(i) => {
                    let methods: Vec<_> = (i.methods.iter())
                        .map(|m| {
                            let default = self.opt_name(m.default);
                            finish((&m.name, default, m.promise, self.opt_ty(m.throws)))
                        })
                        .collect();
                    let fields = self.fields(&i.fields);
                    (&i.name, finish((3u8, i.generics, fields, methods)))
                }
                Def::Global(g) => {
                    let c = g.c_const.is_some();
                    (&g.name, finish((4u8, self.ty(g.ty), g.slot, c)))
                }
            };
            items.push((name.clone(), hash));
        }
        unordered(items)
    }

    fn implementations(&mut self) -> u64 {
        let p = self.p;
        let items = (p.impls.iter())
            .map(|i| {
                let head = (self.ty(i.ty), i.generics, self.name(i.iface));
                let args = self.tys(&i.iface_args);
                let methods = self.names(i.methods.iter().copied());
                (head.2.to_string(), finish((head, args, methods)))
            })
            .collect();
        unordered(items)
    }

    fn counted(&mut self) -> u64 {
        let mut hs = self.tys(&self.p.counted_objects);
        hs.sort_unstable();
        finish(hs)
    }

    fn module_state(&mut self) -> u64 {
        let p = self.p;
        let state = p.module_state.as_ref().map(|m| {
            let slots = self.names(m.slots.iter().copied());
            (self.ty(m.ty), slots, m.deferred)
        });
        finish(state)
    }

    fn entry(&mut self) -> u64 {
        finish(self.opt_name(self.p.entry))
    }

    /// The std definitions lazy std left unchecked: the complement of what the program reaches.
    fn reached(&mut self) -> u64 {
        let mut names = self.names(self.p.unchecked.iter().copied());
        names.sort_unstable();
        finish(names)
    }

    fn generated(&mut self) -> u64 {
        let p = self.p;
        let items = (p.generated.iter())
            .map(|((f, args), g)| {
                let key = (self.name(*f), self.tys(args));
                (self.name(*g).to_string(), finish(key))
            })
            .collect();
        unordered(items)
    }

    fn flags(&mut self) -> u64 {
        let p = self.p;
        let mut calls = p.any_call_names.clone();
        calls.sort_unstable();
        let mut dyn_methods: Vec<_> = p.generic_dyn_methods.iter().collect();
        dyn_methods.sort_unstable();
        let mut upcasts: Vec<_> = (p.upcasts.iter())
            .map(|(i, ts)| (self.name(*i), self.names(ts.iter().copied())))
            .collect();
        upcasts.sort_unstable();
        let mut arg_free = self.names(p.arg_free_generic_fns.iter().copied());
        arg_free.sort_unstable();
        let any = (p.any_impl.is_some(), p.dyn_members.is_some());
        let bits = (p.null_proto_records, p.record_fills, p.array_methods_open);
        let more = (p.generator_throw, p.nothing_slots);
        let hooks = format!("{:?}", p.hooks_demanded);
        finish((
            any,
            bits,
            more,
            calls,
            dyn_methods,
            upcasts,
            arg_free,
            hooks,
        ))
    }
}
