//! The interned type table of the HIR (part of the hir.rs contract).

use super::{TyId, TyKind};

#[derive(Clone, Debug, Default)]
pub struct TyTable {
    kinds: Vec<TyKind>,
    map: std::collections::HashMap<TyKind, TyId>,
}

impl TyTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, kind: TyKind) -> TyId {
        // An option of an option is the inner option, with the inner one's nothing (`T | null |
        // null` is `T | null`; hir_encodings.md "Undefined"); wrapping into it is the identity.
        if let TyKind::Option(inner, _) = kind {
            if matches!(self.kinds[inner.0 as usize], TyKind::Option(..)) {
                return inner;
            }
        }
        if let Some(&id) = self.map.get(&kind) {
            return id;
        }
        let id = TyId(self.kinds.len() as u32);
        self.kinds.push(kind.clone());
        self.map.insert(kind, id);
        id
    }

    pub fn kind(&self, id: TyId) -> &TyKind {
        &self.kinds[id.0 as usize]
    }

    /// Lookup without inserting.
    pub fn get(&self, kind: &TyKind) -> Option<TyId> {
        self.map.get(kind).copied()
    }

    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }
}
