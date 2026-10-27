//! Does a type meet a bound that `extend … implements` blocks may give it (infer/ext_impls.rs)?
//!
//! A type meets `I<A>` when its own impl (or a base class's) is `I<A>`, or, without one, when some
//! block for `I<A>` fits it and the block's bounds hold in turn, or through a structural impl.
//! Bounds can lead back to the lookup that needs them (`extend<T extends Q> T implements P`
//! with `extend<T extends P> T implements Q`), so the answer is the least one: a type meets a
//! bound only through a finite chain of blocks, never through itself. It is computed over all
//! the lookups a question reaches (finitely many: a bound is checked on a type argument of the
//! type looked up) by iterating to a fixed point, without recursion, so no nesting limit is
//! needed, and it is the same whatever else is being looked up: it is remembered
//! (`Ctx::ext_exists_memo`). Lowering computes the same answer (velt_vir lower/ext_exists.rs).
//!
//! A computation can start another only through a structural impl (`synth_impl`, for an
//! intersection or `Pick` bound checked with `satisfies`, or as the last resort below), which is
//! made at most once per type and interface (`synth_ifaces.tried`), for a type the question
//! reaches: so computations nest finitely, without a limit, and each answer is complete.

use rustc_hash::FxHashMap;

use crate::ctx::Ctx;
use crate::defs::Bound;
use crate::hir::{TyId, TyKind};

/// A lookup: the type and the bound.
pub(crate) type ExistsKey = (TyId, Bound);

/// One lookup in a fixed-point computation: its answer when known outright, else the ways the
/// type can meet the bound (each a list of other lookups that must all hold).
struct Node {
    key: ExistsKey,
    fixed: Option<bool>,
    alts: Vec<Vec<usize>>,
    fallback_checked: bool,
}

impl Ctx<'_> {
    /// Does `ty` meet `b`, when `b`'s interface has `extend` impls (module docs)?
    pub(crate) fn obligation_holds(&mut self, ty: TyId, b: &Bound, scope: &[Vec<Bound>]) -> bool {
        if !self.is_exists_node(ty, b) {
            return self.satisfies(ty, b, scope);
        }
        self.ext_exists(ty, b, scope)
    }

    /// Is `(ty, b)` decided here rather than by `satisfies` (a type parameter's bounds are in
    /// scope; an interface without blocks has no chains)?
    fn is_exists_node(&self, ty: TyId, b: &Bound) -> bool {
        !matches!(self.ty.kind(ty), TyKind::Param(_)) && self.ext_by_iface.contains_key(&b.iface)
    }

    fn ext_exists(&mut self, ty: TyId, b: &Bound, scope: &[Vec<Bound>]) -> bool {
        if self.ext_memo_impls != self.impls.len() {
            self.ext_memo.clear();
            self.ext_exists_memo.clear();
            self.ext_memo_impls = self.impls.len();
        }
        let closed = !self.mentions_params(ty);
        let root: ExistsKey = (ty, b.clone());
        if let Some(&v) = closed.then(|| self.ext_exists_memo.get(&root)).flatten() {
            return v;
        }
        let mut nodes: Vec<Node> = vec![];
        let mut index: FxHashMap<ExistsKey, usize> = FxHashMap::default();
        self.exists_node(root.clone(), &mut nodes, &mut index);
        let mut k = 0;
        while k < nodes.len() {
            if nodes[k].fixed.is_none() {
                let key = nodes[k].key.clone();
                let alts = self.exists_alts(&key, scope, &mut nodes, &mut index);
                nodes[k].alts = alts;
            }
            k += 1;
        }
        let values = loop {
            let values = least_fixed_point(&nodes);
            // A type that no block gives the bound may still meet it through a structural
            // impl (`find_impl`'s last resort).
            let mut more = false;
            for (n, v) in nodes.iter_mut().zip(&values) {
                if *v || n.fixed.is_some() || n.fallback_checked {
                    continue;
                }
                n.fallback_checked = true;
                let (t, b) = n.key.clone();
                let found = match self.field_only.contains_key(&b.iface) {
                    true => self.field_only_impl(t, b.iface),
                    false => self.synth_impl(t, b.iface),
                };
                if found.is_some_and(|(_, args, _)| args == b.args) {
                    n.fixed = Some(true);
                    more = true;
                }
            }
            if !more {
                break values;
            }
        };
        if closed && self.ext_memo_impls == self.impls.len() {
            for (n, v) in nodes.iter().zip(&values) {
                self.ext_exists_memo.insert(n.key.clone(), *v);
            }
        }
        values[0]
    }

    /// The node of `key`, added (with its answer when known outright) if new.
    fn exists_node(
        &mut self,
        key: ExistsKey,
        nodes: &mut Vec<Node>,
        index: &mut FxHashMap<ExistsKey, usize>,
    ) -> usize {
        if let Some(&k) = index.get(&key) {
            return k;
        }
        let (t, b) = (key.0, &key.1);
        let fixed = if let Some(&v) = self.ext_exists_memo.get(&key) {
            Some(v)
        } else if self.ty.is_bottom(t) {
            Some(true)
        } else {
            self.own_bound(t, b)
        };
        index.insert(key.clone(), nodes.len());
        nodes.push(Node {
            key,
            fixed,
            alts: vec![],
            fallback_checked: false,
        });
        nodes.len() - 1
    }

    /// Whether `t` meets `b` through its own impl or its base classes' (or as an interface
    /// value of an interface extending it), when it has one of `b`'s interface at all.
    fn own_bound(&mut self, t: TyId, b: &Bound) -> Option<bool> {
        if let TyKind::Dyn(d, args) = self.ty.kind(t).clone() {
            if self.extends_iface(&Bound { iface: d, args }, b) && !self.has_generic_method(b) {
                return Some(true);
            }
        }
        self.own_impl(t, b.iface).map(|(_, args, _)| args == b.args)
    }

    /// The ways `key`'s type meets its bound through blocks: per block for the bound's
    /// interface and arguments that fits the type (or, without one, a base class), the
    /// lookups its own bounds need. A bound decided outright that fails drops the way. A block
    /// without bounds that fits decides: the most specific one gives the interface arguments
    /// (a block with bounds that would win over it has the same, `Ctx::ext_shadowed`), and
    /// blocks for a base class don't apply.
    fn exists_alts(
        &mut self,
        key: &ExistsKey,
        scope: &[Vec<Bound>],
        nodes: &mut Vec<Node>,
        index: &mut FxHashMap<ExistsKey, usize>,
    ) -> Vec<Vec<usize>> {
        let (ty, b) = (key.0, &key.1);
        let cands = self.ext_by_iface.get(&b.iface).cloned().unwrap_or_default();
        let mut alts = vec![];
        let mut cur = Some(ty);
        for _ in 0..64 {
            let Some(t) = cur else { break };
            let mut fits = vec![];
            let mut plain: Option<(u32, Vec<TyId>)> = None;
            for &i in &cands {
                let Some((_, iargs, _)) = self.try_impl(i, t) else {
                    continue;
                };
                if !self.ext_bounded(i)
                    && (plain.as_ref()).is_none_or(|p| self.ext_more_specific(i, p.0))
                {
                    plain = Some((i, iargs.clone()));
                }
                fits.push((i, iargs));
            }
            if let Some((_, iargs)) = plain {
                if iargs == b.args {
                    alts.push(vec![]);
                }
                break;
            }
            for (i, iargs) in fits {
                if iargs != b.args {
                    continue;
                }
                let e = self.ext_impls[&i];
                let Some(obligations) = self.ext_obligations(i, e, t) else {
                    continue;
                };
                let mut needs = vec![];
                let mut ok = true;
                for (a, ob) in obligations {
                    if self.is_exists_node(a, &ob) {
                        needs.push(self.exists_node((a, ob), nodes, index));
                    } else if !self.satisfies(a, &ob, scope) {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    alts.push(needs);
                }
            }
            cur = self.base_of(t);
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
