//! A key tested against a fixed list of names in the `any` glue: `Array.prototype`'s names in
//! an array's `AnyHas` and `AnyGet`, `Object.prototype`'s in every object's, the members a
//! read or call through `any` does not reach yet (#924). The answer is the same for every name
//! of the list, so the list need not be code: written out, it was one `str_eq` call and branch
//! per name in every glue of every type that has it (about 31 names in each of an array type's
//! `AnyHas` and `AnyGet`; 48,000 such calls, about 140,000 blocks, in sigx's small-page, #2189).
//!
//! A longer list is a static table of string objects instead, and the test one call of the
//! shared function `Work::KeyIn` (`(key, table, n) -> bool`), which compares the key with each
//! in turn as the written-out chain did. A list of [`KEYS_INLINE`] names or fewer is still
//! compared at the site.

use super::rt::Rt;
use super::{cint, Cx, FnLower, Work};
use crate::vir::{
    self, BinOp, BlockId, Callee, Const, Function, Operand, Place, Rvalue, StaticData, StaticId,
    Terminator, Ty,
};

/// The longest list compared at the site, one `str_eq` per name.
const KEYS_INLINE: usize = 2;

/// Bytes of one string object (`VeltStr`: text pointer, length word, 0).
const STR_SIZE: usize = 24;

impl Cx<'_> {
    /// The read-only table of string objects holding `names`, in order. Memoized.
    fn key_table(&mut self, names: &[&str]) -> StaticId {
        let key: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        if let Some(&id) = self.key_tables.get(&key) {
            return id;
        }
        let mut bytes = vec![0u8; STR_SIZE * names.len()];
        let mut relocs = Vec::with_capacity(names.len());
        for (i, s) in names.iter().enumerate() {
            let mut text = s.as_bytes().to_vec();
            if text.is_empty() {
                text.push(0);
            }
            let t = self.static_bytes(text, 1);
            let at = STR_SIZE * i;
            bytes[at + 8..at + 16].copy_from_slice(&super::strings::str_w1(s).to_le_bytes());
            relocs.push((at as u32, Const::Static(t)));
        }
        let id = StaticId(self.statics.len() as u32);
        self.statics.push(StaticData {
            bytes,
            align: 8,
            relocs,
            mutable: None,
        });
        self.key_tables.insert(key, id);
        id
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// Branch on the string at `key` being one of `names` (module docs): the current block
    /// becomes the matching one; returns the block to continue in for another key.
    pub(super) fn on_keys(&mut self, key: vir::Local, names: &[&str]) -> BlockId {
        let yes = self.new_block();
        if names.len() <= KEYS_INLINE {
            for name in names {
                let m = self.key_is(key, name);
                let next = self.new_block();
                self.branch(m, yes, next);
                self.switch_to(next);
            }
            let no = self.cur;
            self.switch_to(yes);
            return no;
        }
        let table = self.cx.key_table(names);
        let f = self.cx.func(Work::KeyIn);
        let hit = Place::local(self.temp(Ty::Bool));
        let args = vec![
            Operand::Copy(Place::local(key)),
            Operand::Const(Const::Static(table), Ty::Ptr),
            cint(names.len() as i128, Ty::U64),
        ];
        self.call(Callee::Func(f), args, Some(hit.clone()), false);
        let no = self.new_block();
        self.branch(Operand::Copy(hit), yes, no);
        self.switch_to(yes);
        no
    }

    /// `Work::KeyIn`: `(key, table, n) -> bool`, whether the string at `key` equals one of the
    /// `n` string objects at `table`, compared in order.
    pub(super) fn build_key_in(cx: &'c mut Cx<'h>) -> Function {
        let mut lw = FnLower::bare(cx, vec![]);
        let key = lw.new_local(Ty::Ptr, Some("key".into()));
        let table = lw.new_local(Ty::Ptr, Some("table".into()));
        let n = lw.new_local(Ty::U64, Some("n".into()));
        let i = lw.new_local(Ty::U64, Some("i".into()));
        lw.assign(Place::local(i), Rvalue::Use(cint(0, Ty::U64)));
        let (head, body, step, hit, miss) = (
            lw.new_block(),
            lw.new_block(),
            lw.new_block(),
            lw.new_block(),
            lw.new_block(),
        );
        lw.goto(head);
        lw.switch_to(head);
        let more = lw.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(
                BinOp::Lt,
                Operand::Copy(Place::local(i)),
                Operand::Copy(Place::local(n)),
            ),
        );
        lw.branch(more, body, miss);
        lw.switch_to(body);
        let off = lw.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(
                BinOp::Mul,
                Operand::Copy(Place::local(i)),
                cint(STR_SIZE as i128, Ty::U64),
            ),
        );
        let at = lw.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, Operand::Copy(Place::local(table)), off),
        );
        let eq = Place::local(lw.temp(Ty::U8));
        lw.call_rt(
            Rt::StrEq,
            vec![Operand::Copy(Place::local(key)), at],
            Some(eq.clone()),
        );
        let m = lw.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, Operand::Copy(eq), cint(0, Ty::U8)),
        );
        lw.branch(m, hit, step);
        lw.switch_to(step);
        let next = lw.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Add, Operand::Copy(Place::local(i)), cint(1, Ty::U64)),
        );
        lw.assign(Place::local(i), Rvalue::Use(next));
        lw.goto(head);
        lw.switch_to(hit);
        lw.terminate(Terminator::Return(Operand::Const(
            Const::Bool(true),
            Ty::Bool,
        )));
        lw.switch_to(miss);
        lw.terminate(Terminator::Return(Operand::Const(
            Const::Bool(false),
            Ty::Bool,
        )));
        lw.finish("_Gkey_in".into(), vec![Ty::Ptr, Ty::Ptr, Ty::U64], Ty::Bool)
    }
}
