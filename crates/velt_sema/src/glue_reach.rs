//! The methods a value runs with no call that names them and no vtable slot
//! ([`crate::dispatch`] has those): the hooks glue calls for every value of a class, its
//! `[Symbol.dispose]()` drop hook and the formatters' `toString()`, `toJSON()` and `[INSPECT]()`
//! (`hir::AdtDef::dispose`, `crate::hooks`), and, for a value converted to a dynamic type while
//! the program reads members of one, every method of its class, which its `any` table holds by
//! name (`Program::dyn_members`). `struct_tuples` carries what these methods store as it carries
//! a dispatched method's.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::collect::{ASYNC_DISPOSE, DISPOSE};
use crate::ctx::Ctx;
use crate::dispatch::{class_chain, Target};
use crate::hir::{AdtKind, DefId, TyId, TyKind};
use crate::hooks;
use crate::struct_tuples::is_generic;
use crate::types::children;

/// The method names glue calls.
const HOOKS: [&str; 5] = [
    DISPOSE,
    ASYNC_DISPOSE,
    hooks::TO_STRING,
    hooks::TO_JSON,
    hooks::INSPECT,
];

/// What [`Reach::dynamic`] finds.
#[derive(Clone, Default)]
pub(crate) struct Held {
    /// Every method of each struct or class held, with its owner's arguments.
    pub(crate) methods: Vec<Target>,
    /// The interfaces (not dynamic types) whose values are held: converted to a dynamic type,
    /// such a value's object reaches its class's methods by name too.
    pub(crate) ifaces: Vec<DefId>,
    /// The type parameters held (and the types computed from them, like `T["k"]`): what a
    /// caller gives for them is held too. The rest of a generic type is followed here, its
    /// classes' methods with arguments in these parameters.
    pub(crate) params: Vec<TyId>,
}

/// [`Reach::hooks`] and [`Reach::dynamic`] per type, memoized.
#[derive(Default)]
pub(crate) struct Reach {
    hooks: FxHashMap<TyId, Vec<Target>>,
    dynamic: FxHashMap<TyId, Held>,
    /// Types and methods examined: counted as the pass's work.
    pub(crate) work: u64,
}

impl Reach {
    /// The hooks of every struct or class `t` names (also as a type argument, as in
    /// `H<i64>[]`), instantiated with their owners' arguments. A type a field of one holds is
    /// named where the value is made, which reads its hooks there.
    pub(crate) fn hooks(&mut self, cx: &mut Ctx, t: TyId) -> Vec<Target> {
        if let Some(ts) = self.hooks.get(&t) {
            return ts.clone();
        }
        self.work += 1;
        let kind = cx.ty.kind(t).clone();
        let mut out = Targets::default();
        if let TyKind::Adt(d, args) = &kind {
            self.methods(cx, *d, args, false, &mut out);
        }
        // A type argument never holds the type itself, so this ends.
        for c in children(&kind) {
            for x in self.hooks(cx, c) {
                out.push(x);
            }
        }
        self.hooks.insert(t, out.list.clone());
        out.list
    }

    /// What a value of type `t` converted to a dynamic type reaches by name: every method of
    /// each struct or class it holds, in its type arguments, its union members and its fields (and theirs: a
    /// member read through the dynamic value gives a dynamic value of the field), and the
    /// interfaces whose values it holds there.
    pub(crate) fn dynamic(&mut self, cx: &mut Ctx, t: TyId) -> Held {
        if let Some(h) = self.dynamic.get(&t) {
            return h.clone();
        }
        let mut held = Held::default();
        let mut methods = Targets::default();
        let (mut stack, mut seen) = (vec![t], FxHashSet::default());
        while let Some(t) = stack.pop() {
            if !seen.insert(t) {
                continue;
            }
            self.work += 1;
            let kind = cx.ty.kind(t).clone();
            match &kind {
                TyKind::Param(_) => {
                    held.params.push(t);
                    continue;
                }
                TyKind::Op(..) if is_generic(cx, t) => {
                    held.params.push(t);
                    continue;
                }
                TyKind::Adt(d, args) => {
                    self.methods(cx, *d, args, true, &mut methods);
                    let fields: Vec<TyId> = cx
                        .adt(*d)
                        .map(|a| a.fields.iter().map(|f| f.ty).collect())
                        .unwrap_or_default();
                    self.work += fields.len() as u64;
                    stack.extend(fields.into_iter().map(|f| cx.subst(f, args)));
                }
                TyKind::Dyn(i, _) if cx.dynamic(t).is_none() && !held.ifaces.contains(i) => {
                    held.ifaces.push(*i)
                }
                _ => {}
            }
            // A union holds its members' values.
            if let Some(ms) = cx.union_members(t) {
                stack.extend(ms);
            }
            stack.extend(children(&kind));
        }
        held.methods = methods.list;
        self.dynamic.insert(t, held.clone());
        held
    }

    /// The methods of struct or class `d<args>` and its base classes (hooks only, unless
    /// `all`), with their owners' arguments.
    fn methods(&mut self, cx: &mut Ctx, d: DefId, args: &[TyId], all: bool, out: &mut Targets) {
        let methods = cx
            .adt(d)
            .is_some_and(|a| matches!(a.kind, AdtKind::Class | AdtKind::Struct));
        if !methods {
            return;
        }
        for (c, cargs) in class_chain(cx, d, args.to_vec()) {
            let Some(a) = cx.adt(c) else { continue };
            let mut own: Vec<(&String, DefId)> = (a.methods.iter())
                .filter(|(k, m)| !m.is_static && (all || HOOKS.contains(&k.as_str())))
                .map(|(k, m)| (k, m.def))
                .collect();
            own.sort();
            self.work += own.len() as u64;
            for (_, m) in own {
                out.push((m, cargs.clone()));
            }
        }
    }
}

/// Targets in the order found, each once.
#[derive(Default)]
struct Targets {
    list: Vec<Target>,
    seen: FxHashSet<Target>,
}

impl Targets {
    fn push(&mut self, t: Target) {
        if self.seen.insert(t.clone()) {
            self.list.push(t);
        }
    }
}

/// Does the program read members of a dynamic value, so that a value converted to one reaches
/// its class's methods by name (`any::dyn_members`)?
pub(crate) fn members_by_name(cx: &Ctx) -> bool {
    cx.any.is_some_and(|a| a.members) || crate::any::accessor_literals_dynamic(cx)
}
