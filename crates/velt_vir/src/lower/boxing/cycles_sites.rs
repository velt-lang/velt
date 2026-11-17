//! Where the values of the cycle-capable types are made (docs/internals/design/cycles.md
//! section 3.4, step 2): `VELT_DEBUG_COUNTED=1` prints a line per cycle-capable type with the
//! `new` expressions, literals and closures that make its values, so a benchmark row's cost
//! can be traced to the code that pays it. Recorded only under that variable.

use velt_sema::hir::{self, DefId, TyId, TyKind};

use super::cycles::CycleCapable;
use crate::lower::{Cx, FnLower};

impl FnLower<'_, '_> {
    /// Record `e`'s location when it makes a value: `new`, an object, array or tuple literal, a
    /// closure.
    pub(in crate::lower) fn note_alloc_site(&mut self, e: &hir::Expr) {
        if !self.cx.cycle_facts {
            return;
        }
        use hir::ExprKind as K;
        if matches!(
            e.kind,
            K::New { .. } | K::AdtLit { .. } | K::ArrayLit(_) | K::Tuple(_) | K::Closure(_)
        ) {
            let t = self.sub(e.ty);
            self.cx.facts.alloc_sites.insert((t, e.span));
        }
    }
}

impl Cx<'_> {
    /// `name: path:line:col, …` for each entry of `c` some recorded site makes, sorted.
    pub(in crate::lower) fn cycle_site_lines(&mut self, c: &CycleCapable) -> Vec<String> {
        let mut sites: Vec<(String, String)> = vec![];
        let recorded: Vec<_> = self.facts.alloc_sites.iter().copied().collect();
        for (t, span) in recorded {
            let names = self.cycle_entry_names(c, t);
            if names.is_empty() {
                continue;
            }
            let at = (self.locs.as_ref())
                .and_then(|m| m.loc(span).map(|l| m.describe(l)))
                .unwrap_or_else(|| "?".to_string());
            sites.extend(names.into_iter().map(|n| (n, at.clone())));
        }
        sites.sort();
        sites.dedup();
        let mut out: Vec<String> = vec![];
        for (name, at) in sites {
            match out.last_mut() {
                Some(line) if line.starts_with(&format!("{name}: ")) => {
                    line.push_str(", ");
                    line.push_str(&at);
                }
                _ => out.push(format!("{name}: {at}")),
            }
        }
        out
    }

    /// The display names of the entries of `c` a value of `t` is counted under: its class,
    /// its type or its closures' key, and the interfaces and dynamic values it is held as.
    fn cycle_entry_names(&mut self, c: &CycleCapable, t: TyId) -> Vec<String> {
        let mut out = vec![];
        match self.kind(t) {
            TyKind::Adt(d, _) if self.is_class(t) => {
                let root = self.class_root(d);
                if c.roots.contains(&root) {
                    out.push(self.class_name(root));
                }
            }
            // A generic closure's own type: the environments without a key.
            TyKind::Closure(_) if c.closures.contains(&None) => {
                out.push(self.closure_set_name(&None));
            }
            TyKind::FnPtr { .. } => {
                let key = self.cycle_key(t);
                if key.is_some() && c.closures.contains(&key) {
                    out.push(self.closure_set_name(&key));
                }
            }
            _ if c.tys.contains(&t) => out.push(self.type_name(t)),
            _ => {}
        }
        // The value may be held as an interface value (a table of a cycle-capable interface was
        // built for its type) or as a dynamic value (an `any` table was).
        let mut ifaces: Vec<DefId> = (self.facts.iface_tables.iter())
            .filter(|&&(d, _, i)| i == t && c.ifaces.contains(&d))
            .map(|&(d, _, _)| d)
            .collect();
        ifaces.sort_by_key(|d| d.0);
        out.extend(ifaces.into_iter().map(|d| self.iface_entry_name(d)));
        if c.dynamics && self.facts.dyn_types.contains(&t) {
            out.push("dynamic values".into());
        }
        out
    }
}
