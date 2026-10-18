//! `load_partial` and `store_partial`: the `count` elements left at the end of an array (fewer
//! than a vector's), in a few scalar loads or stores that stay inside them. With `c` bytes
//! left: 9 to 15 are two overlapping 8-byte accesses, 4 to 8 two overlapping 4-byte ones, 1 to
//! 3 three single bytes (the first, the middle and the last). Each load is shifted into place
//! in a register, so nothing goes through memory twice.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, types, InstBuilder};
use cranelift_module::Module;
use velt_vir::simd::Lanes;

use super::place::mem_flags;
use super::Translator;

impl<M: Module> Translator<'_, '_, M> {
    /// The vector of the `count` elements from `at` of `array`, zeros above them.
    pub(super) fn simd_load_partial(
        &mut self,
        lanes: Lanes,
        array: ir::Value,
        at: ir::Value,
        count: ir::Value,
    ) -> ir::Value {
        let p = self.simd_element(lanes, array, at);
        let c = self
            .builder
            .ins()
            .imul_imm(count, i64::from(lanes.bits() / 8));
        let [big, notbig, four, small, bytes, join] = self.blocks();
        let low = self.builder.append_block_param(join, types::I64);
        let high = self.builder.append_block_param(join, types::I64);
        let zero = self.builder.ins().iconst(types::I64, 0);
        let is_big = self
            .builder
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThan, c, 8);
        self.builder.ins().brif(is_big, big, &[], notbig, &[]);

        self.builder.switch_to_block(big);
        let lo = self.builder.ins().load(types::I64, mem_flags(), p, 0);
        let end = self.builder.ins().iadd(p, c);
        let hi = self.builder.ins().load(types::I64, mem_flags(), end, -8);
        // The bytes past the first eight are the top `c - 8` of `hi`.
        let missing = self.builder.ins().irsub_imm(c, 16);
        let shift = self.builder.ins().ishl_imm(missing, 3);
        let hi = self.builder.ins().ushr(hi, shift);
        self.builder.ins().jump(join, &[lo.into(), hi.into()]);

        self.builder.switch_to_block(notbig);
        let is_four = self
            .builder
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, c, 4);
        self.builder.ins().brif(is_four, four, &[], small, &[]);

        self.builder.switch_to_block(four);
        let a = self.builder.ins().uload32(mem_flags(), p, 0);
        let end = self.builder.ins().iadd(p, c);
        let z = self.builder.ins().uload32(mem_flags(), end, -4);
        // Bytes 4 to `c - 1` are the top `c - 4` bytes of `z`.
        let missing = self.builder.ins().irsub_imm(c, 8);
        let shift = self.builder.ins().ishl_imm(missing, 3);
        let top = self.builder.ins().ushr(z, shift);
        let placed = self.builder.ins().ishl_imm(top, 32);
        let lo = self.builder.ins().bor(a, placed);
        self.builder.ins().jump(join, &[lo.into(), zero.into()]);

        self.builder.switch_to_block(small);
        self.builder
            .ins()
            .brif(c, bytes, &[], join, &[zero.into(), zero.into()]);

        self.builder.switch_to_block(bytes);
        let half = self.builder.ins().ushr_imm(c, 1);
        let last = self.builder.ins().iadd_imm(c, -1);
        let mut word = self.builder.ins().uload8(types::I64, mem_flags(), p, 0);
        for at in [half, last] {
            let ptr = self.builder.ins().iadd(p, at);
            let byte = self.builder.ins().uload8(types::I64, mem_flags(), ptr, 0);
            let bits = self.builder.ins().ishl_imm(at, 3);
            let moved = self.builder.ins().ishl(byte, bits);
            word = self.builder.ins().bor(word, moved);
        }
        self.builder.ins().jump(join, &[word.into(), zero.into()]);

        self.builder.switch_to_block(join);
        let v = self.builder.ins().scalar_to_vector(types::I64X2, low);
        let v = self.builder.ins().insertlane(v, high, 1);
        self.lanes_of(v, types::I8X16)
    }

    /// Writes the low `count` lanes of `v` to the elements from `at` of `array`.
    pub(super) fn simd_store_partial(
        &mut self,
        lanes: Lanes,
        v: ir::Value,
        array: ir::Value,
        at: ir::Value,
        count: ir::Value,
    ) {
        let words = self.lanes_of(v, types::I64X2);
        let lo = self.builder.ins().extractlane(words, 0);
        let hi = self.builder.ins().extractlane(words, 1);
        let p = self.simd_element(lanes, array, at);
        let c = self
            .builder
            .ins()
            .imul_imm(count, i64::from(lanes.bits() / 8));
        let [big, notbig, four, small, bytes, done] = self.blocks();
        let is_big = self
            .builder
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThan, c, 8);
        self.builder.ins().brif(is_big, big, &[], notbig, &[]);

        self.builder.switch_to_block(big);
        self.builder.ins().store(mem_flags(), lo, p, 0);
        // Bytes `c - 8` to `c - 1` of the vector: `k = c - 8` is 1 to 7.
        let k = self.builder.ins().iadd_imm(c, -8);
        let kbits = self.builder.ins().ishl_imm(k, 3);
        let down = self.builder.ins().ushr(lo, kbits);
        let upbits = self.builder.ins().irsub_imm(kbits, 64);
        let up = self.builder.ins().ishl(hi, upbits);
        let tail = self.builder.ins().bor(down, up);
        let q = self.builder.ins().iadd(p, k);
        self.builder.ins().store(mem_flags(), tail, q, 0);
        self.builder.ins().jump(done, &[]);

        self.builder.switch_to_block(notbig);
        let is_four = self
            .builder
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, c, 4);
        self.builder.ins().brif(is_four, four, &[], small, &[]);

        self.builder.switch_to_block(four);
        self.builder.ins().istore32(mem_flags(), lo, p, 0);
        let k = self.builder.ins().iadd_imm(c, -4);
        let kbits = self.builder.ins().ishl_imm(k, 3);
        let down = self.builder.ins().ushr(lo, kbits);
        let q = self.builder.ins().iadd(p, k);
        self.builder.ins().istore32(mem_flags(), down, q, 0);
        self.builder.ins().jump(done, &[]);

        self.builder.switch_to_block(small);
        self.builder.ins().brif(c, bytes, &[], done, &[]);

        self.builder.switch_to_block(bytes);
        self.builder.ins().istore8(mem_flags(), lo, p, 0);
        let half = self.builder.ins().ushr_imm(c, 1);
        let last = self.builder.ins().iadd_imm(c, -1);
        for at in [half, last] {
            let bits = self.builder.ins().ishl_imm(at, 3);
            let moved = self.builder.ins().ushr(lo, bits);
            let ptr = self.builder.ins().iadd(p, at);
            self.builder.ins().istore8(mem_flags(), moved, ptr, 0);
        }
        self.builder.ins().jump(done, &[]);

        self.builder.switch_to_block(done);
    }

    fn blocks<const N: usize>(&mut self) -> [ir::Block; N] {
        std::array::from_fn(|_| self.builder.create_block())
    }
}
