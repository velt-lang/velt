//! Does a concrete type meet a bound of an `extend … implements` block? The same answer as
//! sema's (velt_sema infer/ext_exists.rs): through the type's own impl of the interface (or a
//! base class's) when it has one, else through a block for the bound's interface and arguments
//! that fits it and whose own bounds hold, the least such answer (never through itself),
//! computed by iterating over every lookup the question reaches to a fixed point, without
//! recursion or a nesting limit, and remembered (`Cx::exists_memo`).

use rustc_hash::FxHashMap;
use velt_sema::hir::{DefId, TyId, TyKind};

/// A bound lookup: the interface, its arguments and the type.
pub(super) type ExistsKey = (DefId, Vec<TyId>, TyId);

struct Node {
    key: ExistsKey,
    fixed: Option<bool>,
    /// Ways the type meets the bound: each a list of lookups that must all hold.
    alts: Vec<Vec<usize>>,
}

impl super::Cx<'_> {
    /// Does `ty` implement `iface<args>` (an `extend` block's bound)?
    pub(super) fn bound_holds(&mut self, iface: DefId, args: Vec<TyId>, ty: TyId) -> bool {
        if self.dyn_meets(iface, &args, ty) {
            return true;
        }
        if !self.has_ext_impls(iface) {
            return self.impl_shape_any(iface, &args, ty);
        }
        let root: ExistsKey = (iface, args, ty);
        if let Some(&v) = self.exists_memo.get(&root) {
            return v;
        }
        let mut nodes: Vec<Node> = vec![];
        let mut index: FxHashMap<ExistsKey, usize> = FxHashMap::default();
        self.exists_node(root, &mut nodes, &mut index);
        let mut k = 0;
        while k < nodes.len() {
            if nodes[k].fixed.is_none() {
                let key = nodes[k].key.clone();
                nodes[k].alts = self.exists_alts(&key, &mut nodes, &mut index);
            }
            k += 1;
        }
        let values = least_fixed_point(&nodes);
        for (n, v) in nodes.iter().zip(&values) {
            self.exists_memo.insert(n.key.clone(), *v);
        }
        values[0]
    }

    /// Is `ty` an interface value whose interface is or extends `iface<args>`
    /// (`Program::dyn_bounds`, sema's `own_bound`)?
    fn dyn_meets(&mut self, iface: DefId, args: &[TyId], ty: TyId) -> bool {
        let TyKind::Dyn(d, dargs) = self.kind(ty) else {
            return false;
        };
        let Some(meets) = self.hir.dyn_bounds.get(&d).cloned() else {
            return false;
        };
        meets.iter().any(|(i, a)| {
            *i == iface
                && a.len() == args.len()
                && a.iter()
                    .zip(args)
                    .all(|(&x, &y)| self.subst(x, &dargs) == y)
        })
    }

    fn has_ext_impls(&mut self, iface: DefId) -> bool {
        let impls = self.impls_of(iface);
        impls
            .iter()
            .any(|i| self.hir.extension_impls.contains_key(i))
    }

    /// An impl of `iface<args>` for `ty` or a base class, for an interface without blocks.
    fn impl_shape_any(&mut self, iface: DefId, args: &[TyId], ty: TyId) -> bool {
        let impls = self.impls_of(iface);
        for cand in self.self_and_bases(ty) {
            if impls
                .iter()
                .any(|&i| self.impl_shape(i, Some(args), cand).is_some())
            {
                return true;
            }
        }
        false
    }

    fn exists_node(
        &mut self,
        key: ExistsKey,
        nodes: &mut Vec<Node>,
        index: &mut FxHashMap<ExistsKey, usize>,
    ) -> usize {
        if let Some(&k) = index.get(&key) {
            return k;
        }
        let fixed = match self.exists_memo.get(&key).copied() {
            Some(v) => Some(v),
            None if self.dyn_meets(key.0, &key.1, key.2) => Some(true),
            // The type's own impl decides, with its arguments.
            None => (self.own_impl(key.0, key.2))
                .map(|(i, cand)| self.impl_shape(i, Some(&key.1), cand).is_some()),
        };
        index.insert(key.clone(), nodes.len());
        nodes.push(Node {
            key,
            fixed,
            alts: vec![],
        });
        nodes.len() - 1
    }

    /// Per block for the bound's interface and arguments that fits the type (or a base
    /// class), the lookups its own bounds need; a bound on an interface without blocks is
    /// decided outright, and one that fails drops the way. A block without bounds that fits
    /// decides, as in sema: the most specific one gives the arguments, and blocks for a base
    /// class don't apply.
    fn exists_alts(
        &mut self,
        key: &ExistsKey,
        nodes: &mut Vec<Node>,
        index: &mut FxHashMap<ExistsKey, usize>,
    ) -> Vec<Vec<usize>> {
        let (iface, args, ty) = (key.0, key.1.clone(), key.2);
        let impls = self.impls_of(iface);
        let mut alts = vec![];
        for cand in self.self_and_bases(ty) {
            let mut plain: Option<u32> = None;
            for &i in impls.iter() {
                let unbounded = match self.hir.extension_impls.get(&i) {
                    Some(bs) => bs.is_empty(),
                    None => continue,
                };
                if unbounded
                    && self.impl_shape(i, None, cand).is_some()
                    && plain.is_none_or(|p| self.ext_more_specific(i, p))
                {
                    plain = Some(i);
                }
            }
            if let Some(p) = plain {
                if self.impl_shape(p, Some(&args), cand).is_some() {
                    alts.push(vec![]);
                }
                break;
            }
            for &i in impls.iter() {
                if !self.hir.extension_impls.contains_key(&i) {
                    continue;
                }
                let Some(binds) = self.impl_shape(i, Some(&args), cand) else {
                    continue;
                };
                let mut needs = vec![];
                let mut ok = true;
                for (bi, bargs, t) in self.ext_obligations(i, &binds) {
                    if self.has_ext_impls(bi) {
                        needs.push(self.exists_node((bi, bargs, t), nodes, index));
                    } else if !self.dyn_meets(bi, &bargs, t) && !self.impl_shape_any(bi, &bargs, t)
                    {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    alts.push(needs);
                }
            }
        }
        alts
    }
}

/// The least answers: a node holds when it is known to, or when all the nodes of one of its
/// ways hold.
fn least_fixed_point(nodes: &[Node]) -> Vec<bool> {
    let mut v: Vec<bool> = nodes.iter().map(|n| n.fixed == Some(true)).collect();
    loop {
        let mut changed = false;
        for (k, n) in nodes.iter().enumerate() {
            if v[k] || n.fixed.is_some() {
                continue;
            }
            if n.alts.iter().any(|alt| alt.iter().all(|&d| v[d])) {
                v[k] = true;
                changed = true;
            }
        }
        if !changed {
            return v;
        }
    }
}
