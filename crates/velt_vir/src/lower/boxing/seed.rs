//! The counted types carried from one lowering of a program to the next (`velt dev`,
//! docs/internals/design/dev-warm-front-end.md step 2).
//!
//! `lower_program` lowers the whole program again while a pass finds more types that must be
//! counted: about half of a dev reload's lowering is the pass that only finds them. The edit a
//! reload lowers rarely changes them, so the dev host hands the previous version's counted
//! types back as a seed, and the first pass already uses them: when it finds nothing more, it is
//! the only pass.
//!
//! The seed names types by their stable spelling (`Cx::type_key`, the one symbols use) and
//! definitions by their qualified names: the type table of the next version is another one. A
//! name the next program does not have is dropped. Counted sets only grow within a lowering
//! (`Cx::close_boxing`), so a seed never takes a type out; one the program no longer needs stays
//! counted until the host restarts. For the sets carried that is a representation the edited
//! program cannot observe: more counting, an `any` table slot it does not use, an identity for
//! closures it no longer compares. Not for what decides whether an `RwLock`'s `read` is exclusive:
//! a read that stayed exclusive after the edit removed the reason would not let two readers in at
//! once, so readers waiting for each other could deadlock where the cold build lets them both
//! read. So `lock_exclusive` is never seeded, nor what `lock_reach` makes it of: the weak sets
//! (`weak_roots`, `weak`, and `weak_object` with its `dyn_types`) and the proxy sets
//! (`proxy_roots`, `proxy_types`, `proxy_traced`, `proxy_class`). Each lowering finds them again;
//! they are rare, so they rarely cost a second pass.
//!
//! Those sets are built by walking counted types (`close_weak` follows a weak key's fields into
//! counted classes), seeded ones included, so a class counted only because the seed names it can
//! still become weak-capable and make a read of it exclusive. So a seeded lowering that ends with
//! a lock value and any weak or proxy fact is done again without the seed
//! (`Boxing::locks_may_follow_seed`, `lower_program_seeded`): such programs are rare, and they
//! only lose the passes the seed would have saved.
//!
//! Type keys name a class or interface by its qualified name, and every object literal that
//! implements interface `I` by one hidden struct name (`hidden_struct_def`), so two of them share a
//! key and the seed may count another one than the old version did: every hidden struct may be
//! counted in a cold build too, so that costs at most an extra pass.

use std::collections::BTreeSet;

use rustc_hash::FxHashMap;
use velt_sema::hir::{DefId, TyId};

use super::Boxing;
use crate::lower::Cx;

/// The counted types of a lowered program, in a form that outlives its type table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CountedSeed {
    /// [`Boxing`]'s sets of types, in the order of [`Cx::seed`]'s destructuring.
    types: Vec<BTreeSet<String>>,
    /// [`Boxing`]'s sets of definitions, likewise.
    defs: Vec<BTreeSet<String>>,
    /// [`Boxing`]'s flags, likewise.
    flags: Vec<bool>,
    any_call_keys: BTreeSet<String>,
}

impl CountedSeed {
    /// Whether one of its sets names `name` (a type key, or `adt Node` for a definition).
    #[cfg(test)]
    pub(crate) fn names(&self, name: &str) -> bool {
        (self.types.iter().chain(&self.defs)).any(|s| s.contains(name))
    }

    /// Names nothing (the first version's, or a program that counts nothing): nothing to add.
    pub(in crate::lower) fn is_empty(&self) -> bool {
        self.types.iter().all(BTreeSet::is_empty)
            && self.defs.iter().all(BTreeSet::is_empty)
            && !self.flags.contains(&true)
            && self.any_call_keys.is_empty()
    }
}

/// `opts` without a seed: the cold lowering a seeded one falls back to.
pub(in crate::lower) fn without_seed<'a>(
    opts: &crate::LowerOptions<'a>,
) -> crate::LowerOptions<'a> {
    crate::LowerOptions {
        counted_seed: None,
        ..*opts
    }
}

impl Boxing {
    /// Whether an `RwLock` `read` may have been made exclusive by the seed: the program has a
    /// lock value and a weak or proxy fact, which `close_weak` and `close_proxies` build by
    /// walking counted types, seeded ones included, and which `lock_reach` turns into an
    /// exclusive read. A lowering that started from a seed and ends so is done again without it
    /// (`lower_program_seeded`): rare, and it costs that program the passes the seed saved.
    pub(in crate::lower) fn locks_may_follow_seed(&self) -> bool {
        !self.lock_values.is_empty()
            && (self.weak_object
                || !self.weak_roots.is_empty()
                || !self.weak.is_empty()
                || !self.proxy_roots.is_empty()
                || !self.proxy_types.is_empty()
                || self.proxy_class.is_some())
    }
}

impl Cx<'_> {
    /// `b` as a seed for the next version of the program.
    pub(in crate::lower) fn seed(&self, b: &Boxing) -> CountedSeed {
        // Every field, so that a new one cannot be forgotten here.
        let Boxing {
            roots,
            boxes,
            interior,
            identity_dyns,
            any_identity,
            fn_identity,
            fn_wrappers,
            generic_any,
            any_writes,
            any_members,
            fn_props,
            any_has,
            shared_fns,
            any_delete,
            any_keys,
            any_reflect,
            any_number,
            any_call_keys,
            recursive,
            transferred,
            // Not seeded: whether a class or type is weak-capable or may hold a proxy decides
            // whether an `RwLock` read of it is exclusive (locks.rs `lock_reach`), which is
            // behavior (module docs). Each lowering finds them again.
            weak_roots: _,
            weak: _,
            weak_object: _,
            dyn_types: _,
            dyn_to_any,
            proxy_roots: _,
            proxy_types: _,
            proxy_class: _,
            proxy_traced: _,
            array_views,
            lock_values,
            lock_roots,
            lock_boxes,
            // Not seeded: an exclusive `read` is behavior, not representation (module docs).
            lock_exclusive: _,
            read_indirect,
        } = b;
        let types = |s: &mut dyn Iterator<Item = &TyId>| s.map(|t| self.type_key(*t)).collect();
        let defs = |s: &mut dyn Iterator<Item = &DefId>| {
            s.filter_map(|d| self.seed_def_name(*d)).collect()
        };
        CountedSeed {
            types: vec![
                types(&mut boxes.iter()),
                types(&mut interior.iter()),
                types(&mut identity_dyns.iter()),
                types(&mut transferred.iter()),
                types(&mut array_views.iter()),
                types(&mut lock_values.iter()),
                types(&mut lock_boxes.iter()),
            ],
            defs: vec![
                defs(&mut roots.iter()),
                defs(&mut recursive.iter()),
                defs(&mut dyn_to_any.iter()),
                defs(&mut lock_roots.iter()),
            ],
            flags: vec![
                *any_identity,
                *fn_identity,
                *generic_any,
                *any_writes,
                *fn_props,
                *any_has,
                *shared_fns,
                *any_delete,
                *any_keys,
                *any_reflect,
                *any_number,
                *read_indirect,
                *any_members,
                *fn_wrappers,
            ],
            any_call_keys: any_call_keys.clone(),
        }
    }

    /// `b` with what `seed` names that this program has added: the counted types the first
    /// pass starts from.
    pub(in crate::lower) fn with_seed(&self, mut b: Boxing, seed: &CountedSeed) -> Boxing {
        if seed.is_empty() {
            return b;
        }
        let tys: FxHashMap<String, TyId> = (0..self.types.len())
            .map(|i| {
                let t = TyId(i as u32);
                (self.type_key(t), t)
            })
            .collect();
        let defs: FxHashMap<String, DefId> = (0..self.hir.defs.len())
            .filter_map(|i| {
                let d = DefId(i as u32);
                self.seed_def_name(d).map(|n| (n, d))
            })
            .collect();
        let t = |i: usize| {
            (seed.types.get(i).into_iter().flatten()).filter_map(|n| tys.get(n).copied())
        };
        b.boxes.extend(t(0));
        b.interior.extend(t(1));
        b.identity_dyns.extend(t(2));
        b.transferred.extend(t(3));
        b.array_views.extend(t(4));
        b.lock_values.extend(t(5));
        b.lock_boxes.extend(t(6));
        let d = |i: usize| {
            (seed.defs.get(i).into_iter().flatten()).filter_map(|n| defs.get(n).copied())
        };
        b.roots.extend(d(0));
        b.recursive.extend(d(1));
        b.dyn_to_any.extend(d(2));
        b.lock_roots.extend(d(3));
        let flags: [&mut bool; 14] = [
            &mut b.any_identity,
            &mut b.fn_identity,
            &mut b.generic_any,
            &mut b.any_writes,
            &mut b.fn_props,
            &mut b.any_has,
            &mut b.shared_fns,
            &mut b.any_delete,
            &mut b.any_keys,
            &mut b.any_reflect,
            &mut b.any_number,
            &mut b.read_indirect,
            &mut b.any_members,
            &mut b.fn_wrappers,
        ];
        for (flag, &seeded) in flags.into_iter().zip(&seed.flags) {
            *flag |= seeded;
        }
        b.any_call_keys.extend(seed.any_call_keys.iter().cloned());
        b
    }

    /// A definition's name in a seed: its qualified name, when it has one that names it alone
    /// (not an anonymous object type, which is structural and `recursive_objects` finds again).
    fn seed_def_name(&self, d: DefId) -> Option<String> {
        use velt_sema::hir::{AdtKind, Def};
        match self.hir.defs.get(d.0 as usize)? {
            Def::Adt(a) if a.kind == AdtKind::Anon => None,
            Def::Adt(a) => Some(format!("adt {}", a.name)),
            Def::Enum(e) => Some(format!("enum {}", e.name)),
            Def::Interface(i) => Some(format!("iface {}", i.name)),
            Def::Fn(f) => Some(format!("fn {}", f.name)),
            _ => None,
        }
    }
}
