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
//! counted until the host restarts, which changes the representation, not what the program does.

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
    proxy_class: Option<String>,
    any_call_keys: BTreeSet<String>,
}

impl CountedSeed {
    /// Names nothing (the first version's, or a program that counts nothing): nothing to add.
    fn is_empty(&self) -> bool {
        self.types.iter().all(BTreeSet::is_empty)
            && self.defs.iter().all(BTreeSet::is_empty)
            && !self.flags.contains(&true)
            && self.proxy_class.is_none()
            && self.any_call_keys.is_empty()
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
            generic_any,
            any_writes,
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
            weak_roots,
            weak,
            weak_object,
            dyn_types,
            dyn_to_any,
            proxy_roots,
            proxy_types,
            proxy_class,
            proxy_traced,
            array_views,
            lock_values,
            lock_roots,
            lock_boxes,
            lock_exclusive,
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
                types(&mut weak.iter()),
                types(&mut dyn_types.iter()),
                types(&mut proxy_types.iter()),
                types(&mut proxy_traced.iter()),
                types(&mut array_views.iter()),
                types(&mut lock_values.iter()),
                types(&mut lock_boxes.iter()),
                types(&mut lock_exclusive.iter()),
            ],
            defs: vec![
                defs(&mut roots.iter()),
                defs(&mut recursive.iter()),
                defs(&mut weak_roots.iter()),
                defs(&mut dyn_to_any.iter()),
                defs(&mut proxy_roots.iter()),
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
                *weak_object,
                *read_indirect,
            ],
            proxy_class: proxy_class.and_then(|d| self.seed_def_name(d)),
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
        b.weak.extend(t(4));
        b.dyn_types.extend(t(5));
        b.proxy_types.extend(t(6));
        b.proxy_traced.extend(t(7));
        b.array_views.extend(t(8));
        b.lock_values.extend(t(9));
        b.lock_boxes.extend(t(10));
        b.lock_exclusive.extend(t(11));
        let d = |i: usize| {
            (seed.defs.get(i).into_iter().flatten()).filter_map(|n| defs.get(n).copied())
        };
        b.roots.extend(d(0));
        b.recursive.extend(d(1));
        b.weak_roots.extend(d(2));
        b.dyn_to_any.extend(d(3));
        b.proxy_roots.extend(d(4));
        b.lock_roots.extend(d(5));
        let flags: [&mut bool; 13] = [
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
            &mut b.weak_object,
            &mut b.read_indirect,
        ];
        for (flag, &seeded) in flags.into_iter().zip(&seed.flags) {
            *flag |= seeded;
        }
        if b.proxy_class.is_none() {
            b.proxy_class = (seed.proxy_class.as_ref()).and_then(|n| defs.get(n).copied());
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
