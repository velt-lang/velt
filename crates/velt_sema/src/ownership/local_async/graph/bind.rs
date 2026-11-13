//! Pattern bindings in the flow graph: a bound local holds a value of unknown origin, and, when
//! the matched value is a local's, what that local holds.

use crate::hir::{Pat, PatKind};

use super::Walk;

impl Walk<'_, '_> {
    pub(super) fn bind(&mut self, p: &Pat) {
        self.bind_from(p, None);
    }

    /// [`Self::bind`], where the matched value is node `from`'s: a part of it is bound, so its
    /// values flow into the bindings too (`match j { Some(v) => … }` binds what `j` holds).
    pub(super) fn bind_from(&mut self, p: &Pat, from: Option<usize>) {
        match &p.kind {
            PatKind::Binding(l, _) => {
                let n = self.local(*l);
                self.g.unknown[n] = true;
                if let Some(m) = from {
                    self.g.srcs[n].push((m, None));
                }
            }
            PatKind::Variant { args, .. } | PatKind::Tuple(args) | PatKind::Or(args) => {
                args.iter().for_each(|a| self.bind_from(a, from))
            }
            PatKind::Adt { fields } => fields.iter().for_each(|(_, a)| self.bind_from(a, from)),
            PatKind::Array { elems, rest } => {
                elems.iter().for_each(|a| self.bind_from(a, from));
                if let Some(l) = rest {
                    let n = self.local(*l);
                    self.g.unknown[n] = true;
                }
            }
            PatKind::Some(a) => self.bind_from(a, from),
            PatKind::Wildcard | PatKind::Lit(_) | PatKind::None | PatKind::InstanceOf(_) => {}
        }
    }
}
