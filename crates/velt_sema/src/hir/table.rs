//! The interned type table of the HIR (part of the hir.rs contract).

use super::{Nothing, TyId, TyKind};

#[derive(Clone, Debug, Default)]
pub struct TyTable {
    kinds: Vec<TyKind>,
    map: rustc_hash::FxHashMap<TyKind, TyId>,
}

impl TyTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, kind: TyKind) -> TyId {
        // An option of an option is one option (hir_encodings.md "Undefined"). `(T | null) |
        // undefined` (also under `T | null | undefined`) is `T | null | undefined`, whose value
        // is the outer option of a `T | null`; otherwise it is the inner option, whose nothing
        // is kept (`T | null | null` is `T | null`): wrapping into it is the identity.
        if let TyKind::Option(inner, outer) = kind {
            if let TyKind::Option(p, n) = self.kinds[inner.0 as usize] {
                if n == Nothing::Null && outer.has_undefined() {
                    return self.intern(TyKind::Option(p, Nothing::Both));
                }
                return inner;
            }
            // `T | null | undefined` holds a `T | null` (its payload, `TyTable::payload`).
            if outer == Nothing::Both {
                self.intern(TyKind::Option(inner, Nothing::Null));
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

    /// The type a `WrapSome` / `UnwrapSome` / `PatKind::Some` of option type `t` holds: `T` of
    /// `T | null` and `T | undefined`, `T | null` of `T | null | undefined` (whose own nothing
    /// is `undefined`; hir_encodings.md "Undefined"). `None` if `t` is not an option.
    pub fn payload(&self, t: TyId) -> Option<TyId> {
        match *self.kind(t) {
            TyKind::Option(p, Nothing::Both) => Some(
                self.get(&TyKind::Option(p, Nothing::Null))
                    .expect("ICE: `T | null | undefined` interned without its `T | null`"),
            ),
            TyKind::Option(p, _) => Some(p),
            _ => None,
        }
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
