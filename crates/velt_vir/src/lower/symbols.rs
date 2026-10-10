//! `Intrinsic::SymbolStatic`: the symbols the compiler makes (module constants
//! `const KEY = Symbol("k")`, the well-known symbols) are read-only records (rt_abi.md
//! "Symbols"), one per id, so every evaluation of the constant is the same symbol.

use velt_sema::hir::{self, ExprKind, Lit};

use super::{ice, Cx, FnLower};
use crate::vir::{self, Const, Operand, StaticData, StaticId, Ty};

/// The record's `key` of the compiler's symbol `id`: 2 and up (the runtime uses 0 and 1), so no
/// two records have the same bytes and none can be merged with another.
fn record_key(id: u128) -> u64 {
    (id as u64).wrapping_add(2)
}

impl Cx<'_> {
    /// The record of the compiler's symbol `id` (`desc`: its description, if it has one).
    fn symbol_record(&mut self, id: u128, desc: Option<&str>) -> StaticId {
        if let Some(&s) = self.symbol_records.get(&id) {
            return s;
        }
        let mut bytes = vec![0u8; 16];
        bytes[8..16].copy_from_slice(&record_key(id).to_le_bytes());
        let relocs = match desc {
            Some(d) => vec![(0, Const::Static(self.static_str_object(d)))],
            None => vec![],
        };
        let s = StaticId(self.statics.len() as u32);
        self.statics.push(StaticData {
            bytes,
            align: 8,
            relocs,
        });
        self.symbol_records.insert(id, s);
        s
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
}
