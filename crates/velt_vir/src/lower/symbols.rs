//! `Intrinsic::SymbolStatic`: the symbols the compiler makes (module constants
//! `const KEY = Symbol("k")` and `const R = Symbol.for("r")`, the well-known symbols) are
//! read-only records (rt_abi.md "Symbols"), one per id, so every evaluation of the constant is
//! the same symbol, without a call. `Intrinsic::SymbolRegistry`: the table of the registry's
//! records among them (`Symbol.for`), which `velt_rt_symbol_for` consults, so the runtime's
//! registry gives the same records for their keys.

use velt_sema::hir::{self, ExprKind, Lit};

use super::{ice, Cx, FnLower};
use crate::vir::{self, Const, Operand, StaticData, StaticId, Ty};

/// The first id of the registry's symbols (velt_sema's `symbols::REGISTERED_IDS`).
const REGISTERED_IDS: u128 = 1 << 40;

/// The runtime's `key` of a record `Symbol.for` made (velt_rt's `KEY_REGISTERED`).
const KEY_REGISTERED: u64 = 1;

/// The bit of `key` set when the symbol has a description (velt_rt's `DESCRIBED`).
const DESCRIBED: u64 = 1 << 63;

/// The record's `key` of the compiler's symbol `id`: 2 and up (the runtime uses 0 and 1), so no
/// two records have the same bytes and none can be merged with another; a registry record's is
/// [`KEY_REGISTERED`] (its description, a distinct key, tells it apart). [`DESCRIBED`] is added
/// when it has a description.
fn record_key(id: u128, described: bool) -> u64 {
    let key = match id >= REGISTERED_IDS {
        true => KEY_REGISTERED,
        false => (id as u64).wrapping_add(2),
    };
    match described {
        true => key | DESCRIBED,
        false => key,
    }
}

/// The records of the compiler's symbols and the registry table.
#[derive(Default)]
pub(super) struct SymbolRecords {
    by_id: rustc_hash::FxHashMap<u128, StaticId>,
    /// The registry's records, in the order they were made.
    registered: Vec<StaticId>,
    /// The table `SymbolRegistry` gives, filled when lowering is done (`fill_registry`).
    table: Option<StaticId>,
}

impl Cx<'_> {
    /// The record of the compiler's symbol `id` (`desc`: its description, if it has one).
    fn symbol_record(&mut self, id: u128, desc: Option<&str>) -> StaticId {
        if let Some(&s) = self.symbol_records.by_id.get(&id) {
            return s;
        }
        // `{ desc: VeltStr, key: u64 }`: the description in the static form (its bytes are
        // another static), or all zero (the empty string) without one.
        let mut bytes = vec![0u8; 32];
        bytes[24..32].copy_from_slice(&record_key(id, desc.is_some()).to_le_bytes());
        let relocs = match desc {
            Some(d) => {
                bytes[8..16].copy_from_slice(&super::strings::str_w1(d).to_le_bytes());
                let mut text = d.as_bytes().to_vec();
                if text.is_empty() {
                    text.push(0);
                }
                vec![(0, Const::Static(self.static_bytes(text, 1)))]
            }
            None => vec![],
        };
        let s = StaticId(self.statics.len() as u32);
        self.statics.push(StaticData {
            bytes,
            align: 8,
            relocs,
            mutable: None,
        });
        self.symbol_records.by_id.insert(id, s);
        if id >= REGISTERED_IDS {
            self.symbol_records.registered.push(s);
        }
        s
    }

    /// The registry table (filled by [`Self::fill_registry`]).
    fn registry_table(&mut self) -> StaticId {
        if let Some(t) = self.symbol_records.table {
            return t;
        }
        let t = StaticId(self.statics.len() as u32);
        self.statics.push(StaticData {
            bytes: vec![0u8; 8],
            align: 8,
            relocs: vec![],
            mutable: None,
        });
        self.symbol_records.table = Some(t);
        t
    }

    /// Writes the registry table, once every function is lowered: the address of each registry
    /// record, then a null slot.
    pub(super) fn fill_registry(&mut self) {
        let Some(t) = self.symbol_records.table else {
            return;
        };
        let recs = &self.symbol_records.registered;
        let table = &mut self.statics[t.0 as usize];
        table.bytes = vec![0u8; 8 * (recs.len() + 1)];
        table.relocs = recs
            .iter()
            .enumerate()
            .map(|(i, r)| (8 * i as u32, Const::Static(*r)))
            .collect();
    }
}

impl FnLower<'_, '_> {
    /// `SymbolStatic(id, desc, described)`: the address of the record (all three are literals).
    pub(super) fn symbol_static(&mut self, args: &[hir::Expr]) -> Operand {
        let lit = |e: &hir::Expr| match &e.kind {
            ExprKind::Lit(l) => l.clone(),
            _ => ice("`SymbolStatic` takes literals"),
        };
        let (Lit::Int(id), Lit::Str(desc), Lit::Bool(described)) =
            (lit(&args[0]), lit(&args[1]), lit(&args[2]))
        else {
            ice("`SymbolStatic(id, desc, described)`")
        };
        let rec = self
            .cx
            .symbol_record(id, described.then_some(desc.as_str()));
        Operand::Const(vir::Const::Static(rec), Ty::Ptr)
    }

    /// `SymbolRegistry()`: the address of the registry table.
    pub(super) fn symbol_registry(&mut self) -> Operand {
        let t = self.cx.registry_table();
        Operand::Const(vir::Const::Static(t), Ty::Ptr)
    }
}
