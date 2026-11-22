//! The X25519 ladder as one field operation (`velt_field_x25519_ladder`, velt_vir `field.rs`):
//! RFC 7748's Montgomery ladder on Curve25519, with elements of four 64-bit limbs (radix 2^64)
//! instead of std's five 51-bit ones (std/crypto/field25519.velt), as ring's assembly has them.
//!
//! On x86-64 with BMI2 and ADX each ladder step is one block of assembly on a scratch area: the
//! products are those of the P-256 operations (`field.rs` `adx_product`, `adx_square_product`),
//! reduced with 2^256 = 38 mod p (four `mulx` by 38 on the two carry chains, then the carry
//! limb times 38 folded in, then once more for a last carry). Sums and differences fold their
//! carry or borrow the same way. Every value stays below 2^256, congruent mod p but not reduced;
//! the helper reduces X2 and Z2 fully at the end. The loop over the scalar's 255 bits and the
//! conditional swaps are plain IR with masks (no branch or index depends on a bit). Elsewhere the
//! helper returns false and std runs its own ladder.

// The generators name limb `k` of each operand and register `k` of the product by index.
#![allow(clippy::needless_range_loop)]

use crate::field::{adx_product, adx_square_product, asm_text, Ops, CPU_BYTE, HAS_ADX, T};

/// The scratch area's slots (32 bytes each).
const X1: usize = 0;
const X2: usize = 1;
const Z2: usize = 2;
const X3: usize = 3;
const Z3: usize = 4;
const A: usize = 5;
const AA: usize = 6;
const B: usize = 7;
const BB: usize = 8;
const E: usize = 9;
const C: usize = 10;
const D: usize = 11;
const DA: usize = 12;
const CB: usize = 13;
const TMP: usize = 14;
const SLOTS: usize = 15;

/// RFC 7748's a24 for the `AA + a24 E` form.
const A24: u64 = 121_665;

fn ops(d: usize, a: usize, b: usize) -> Ops {
    Ops {
        a: 32 * a,
        b: 32 * b,
        dst: Some(32 * d),
    }
}

/// `T[0..8]` (a full product) reduced below 2^256 and stored as `o` says: `t_lo + 38 t_hi`, the
/// carry limb times 38 added, and 38 more for a carry out of that.
fn reduce38(out: &mut Vec<String>, o: Ops) {
    // RDI is zero for the chains' last carries; the XOR clears CF and OF for them.
    out.push("xorl %edi, %edi".into());
    out.push("movl $$38, %edx".into());
    for k in 0..3 {
        out.push(format!("mulxq {}, %rax, %rcx", T[4 + k]));
        out.push(format!("adcxq %rax, {}", T[k]));
        out.push(format!("adoxq %rcx, {}", T[k + 1]));
    }
    out.push(format!("mulxq {}, %rax, {}", T[7], T[4]));
    out.push(format!("adcxq %rax, {}", T[3]));
    out.push(format!("adoxq %rdi, {}", T[4]));
    out.push(format!("adcxq %rdi, {}", T[4]));
    fold_carry(out, o);
}

/// `T[0..4] + 2^256 T[4]` (`T[4]` small) below 2^256, stored: `38 T[4]` added, then 38 for a
/// carry (after which the value is small, so no carry is left).
fn fold_carry(out: &mut Vec<String>, o: Ops) {
    out.push(format!("imulq $$38, {0}, {0}", T[4]));
    out.push(format!("addq {}, {}", T[4], T[0]));
    for k in 1..4 {
        out.push(format!("adcq $$0, {}", T[k]));
    }
    out.push("sbbq %rax, %rax".into());
    out.push("andl $$38, %eax".into());
    out.push(format!("addq %rax, {}", T[0]));
    o.store(out, [T[0], T[1], T[2], T[3]]);
}

fn mul(out: &mut Vec<String>, d: usize, a: usize, b: usize) {
    let o = ops(d, a, b);
    adx_product(out, o);
    reduce38(out, o);
}

fn sqr(out: &mut Vec<String>, d: usize, a: usize) {
    let o = ops(d, a, a);
    adx_square_product(out, o);
    reduce38(out, o);
}

/// `a + b` or `a - b` below 2^256: the carry (borrow) is 2^256, so 38 is added (subtracted),
/// and once more for a second one.
fn add_sub(out: &mut Vec<String>, add: bool, d: usize, a: usize, b: usize) {
    let o = ops(d, a, b);
    for k in 0..4 {
        out.push(format!("movq {}, {}", o.a(k), T[k]));
    }
    let (first, rest, fold) = match add {
        true => ("addq", "adcq", "addq"),
        false => ("subq", "sbbq", "subq"),
    };
    for k in 0..4 {
        let ins = if k == 0 { first } else { rest };
        out.push(format!("{ins} {}, {}", o.b(k), T[k]));
    }
    out.push("sbbq %rax, %rax".into());
    out.push("andl $$38, %eax".into());
    out.push(format!("{fold} %rax, {}", T[0]));
    for k in 1..4 {
        out.push(format!("{rest} $$0, {}", T[k]));
    }
    out.push("sbbq %rax, %rax".into());
    out.push("andl $$38, %eax".into());
    out.push(format!("{fold} %rax, {}", T[0]));
    o.store(out, [T[0], T[1], T[2], T[3]]);
}

/// `a24 a` below 2^256.
fn mul_a24(out: &mut Vec<String>, d: usize, a: usize) {
    let o = ops(d, a, a);
    out.push(format!("movl $${A24}, %edx"));
    out.push(format!("mulxq {}, {}, {}", o.a(0), T[0], T[1]));
    for k in 1..4 {
        out.push(format!("mulxq {}, %rax, {}", o.a(k), T[k + 1]));
        let ins = if k == 1 { "addq" } else { "adcq" };
        out.push(format!("{ins} %rax, {}", T[k]));
    }
    out.push(format!("adcq $$0, {}", T[4]));
    fold_carry(out, o);
}

/// One ladder step after the swap (RFC 7748 §5), all in the scratch area at RSI.
fn step() -> Vec<String> {
    let mut out = vec![];
    add_sub(&mut out, true, A, X2, Z2);
    sqr(&mut out, AA, A);
    add_sub(&mut out, false, B, X2, Z2);
    sqr(&mut out, BB, B);
    add_sub(&mut out, false, E, AA, BB);
    add_sub(&mut out, true, C, X3, Z3);
    add_sub(&mut out, false, D, X3, Z3);
    mul(&mut out, DA, D, A);
    mul(&mut out, CB, C, B);
    add_sub(&mut out, true, TMP, DA, CB);
    sqr(&mut out, X3, TMP);
    add_sub(&mut out, false, TMP, DA, CB);
    sqr(&mut out, TMP, TMP);
    mul(&mut out, Z3, TMP, X1);
    mul(&mut out, X2, AA, BB);
    mul_a24(&mut out, TMP, E);
    add_sub(&mut out, true, TMP, TMP, AA);
    mul(&mut out, Z2, E, TMP);
    out
}

/// The helper (module docs): `(r, d, u, k) -> i1`.
pub(crate) fn ladder_helper(name: &str, x86: bool) -> Vec<String> {
    if !x86 {
        return vec![format!(
            "define internal i1 {name}(ptr %r, i64 %d, i64 %u, i64 %k) alwaysinline nounwind {{\n  ret i1 false\n}}"
        )];
    }
    let mut ir = format!(
        "define internal i1 {name}(ptr %r, i64 %d, i64 %u, i64 %k) noinline nounwind {{\n\
         entry:\n  %fast = call i1 @velt.field.has_adx()\n  br i1 %fast, label %go, label %no\n\
         no:\n  ret i1 false\n\
         go:\n  %data = load ptr, ptr %r, align 8\n  \
         %od = shl i64 %d, 5\n  %pd = getelementptr i8, ptr %data, i64 %od\n  \
         %ou = shl i64 %u, 5\n  %pu = getelementptr i8, ptr %data, i64 %ou\n  \
         %ok = shl i64 %k, 5\n  %pk = getelementptr i8, ptr %data, i64 %ok\n  \
         %s = alloca [{} x i64], align 16\n",
        4 * SLOTS
    );
    let slot = |ir: &mut String, name: &str, s: usize, k: usize| {
        ir.push_str(&format!(
            "  %{name} = getelementptr i8, ptr %s, i64 {}\n",
            32 * s + 8 * k
        ));
    };
    // X1 = X3 = u; X2 = 1, Z2 = 0, Z3 = 1.
    for k in 0..4 {
        ir.push_str(&format!(
            "  %up{k} = getelementptr i8, ptr %pu, i64 {o}\n  %u{k} = load i64, ptr %up{k}, align 8\n  \
             %kp{k} = getelementptr i8, ptr %pk, i64 {o}\n  %k{k} = load i64, ptr %kp{k}, align 8\n",
            o = 8 * k
        ));
        for (s, v) in [
            (X1, format!("%u{k}")),
            (X3, format!("%u{k}")),
            (X2, if k == 0 { "1".into() } else { "0".into() }),
            (Z2, "0".into()),
            (Z3, if k == 0 { "1".into() } else { "0".into() }),
        ] {
            slot(&mut ir, &format!("i{s}_{k}"), s, k);
            ir.push_str(&format!("  store i64 {v}, ptr %i{s}_{k}, align 8\n"));
        }
    }
    ir.push_str("  br label %loop\n");
    // The loop: bit `pos` of the scalar (pos from 254 down to 0; its limb by a public index).
    ir.push_str(
        "loop:\n  %pos = phi i64 [ 254, %go ], [ %next, %loop ]\n  \
         %prev = phi i64 [ 0, %go ], [ %bit, %loop ]\n  \
         %limb = lshr i64 %pos, 6\n  \
         %is0 = icmp eq i64 %limb, 0\n  %is1 = icmp eq i64 %limb, 1\n  %is2 = icmp eq i64 %limb, 2\n  \
         %w23 = select i1 %is2, i64 %k2, i64 %k3\n  %w123 = select i1 %is1, i64 %k1, i64 %w23\n  \
         %w = select i1 %is0, i64 %k0, i64 %w123\n  \
         %sh = and i64 %pos, 63\n  %ws = lshr i64 %w, %sh\n  %bit = and i64 %ws, 1\n  \
         %sw = xor i64 %prev, %bit\n  %mask = sub i64 0, %sw\n",
    );
    cswap(&mut ir, "l", "%mask");
    ir.push_str(&format!(
        "  %rs = call {{ ptr, ptr }} asm sideeffect \"{}\", \"={{rsi}},={{rdi}},0,1,~{{rax}},~{{rdx}},~{{rcx}},~{{r8}},~{{r9}},~{{r10}},~{{r11}},~{{r12}},~{{r13}},~{{r14}},~{{r15}},~{{memory}},~{{dirflag}},~{{fpsr}},~{{flags}}\"(ptr %s, ptr %s)\n",
        asm_text(&step())
    ));
    ir.push_str(
        "  %next = sub i64 %pos, 1\n  %more = icmp ne i64 %pos, 0\n  \
         br i1 %more, label %loop, label %done\n\
         done:\n  %fmask = sub i64 0, %bit\n",
    );
    cswap(&mut ir, "f", "%fmask");
    freeze(&mut ir, "x", X2, "%pd", 0);
    freeze(&mut ir, "z", Z2, "%pd", 32);
    ir.push_str("  ret i1 true\n}");
    vec![HAS_ADX.to_string(), CPU_BYTE.to_string(), ir]
}

/// Swaps (X2, Z2) with (X3, Z3) in the scratch area where `mask` is all ones, without a branch.
fn cswap(ir: &mut String, tag: &str, mask: &str) {
    for (p, q) in [(X2, X3), (Z2, Z3)] {
        for k in 0..4 {
            let n = format!("{tag}{p}_{k}");
            ir.push_str(&format!(
                "  %pp{n} = getelementptr i8, ptr %s, i64 {pp}\n  %pq{n} = getelementptr i8, ptr %s, i64 {pq}\n  \
                 %vp{n} = load i64, ptr %pp{n}, align 8\n  %vq{n} = load i64, ptr %pq{n}, align 8\n  \
                 %x{n} = xor i64 %vp{n}, %vq{n}\n  %t{n} = and i64 %x{n}, {mask}\n  \
                 %np{n} = xor i64 %vp{n}, %t{n}\n  %nq{n} = xor i64 %vq{n}, %t{n}\n  \
                 store i64 %np{n}, ptr %pp{n}, align 8\n  store i64 %nq{n}, ptr %pq{n}, align 8\n",
                pp = 32 * p + 8 * k,
                pq = 32 * q + 8 * k
            ));
        }
    }
}

/// Slot `s` (below 2^256) reduced mod 2^255 - 19 and stored at `base + off`: bit 255 folded in as
/// 19 twice, then p subtracted when the value plus 19 reaches 2^255, all as one i256.
fn freeze(ir: &mut String, tag: &str, s: usize, base: &str, off: usize) {
    let low = "57896044618658097711785492504343953926634992332820282019728792003956564819967";
    let mut v = format!("%{tag}v0");
    ir.push_str(&format!("  {v} = zext i64 0 to i256\n"));
    for k in 0..4 {
        ir.push_str(&format!(
            "  %{tag}p{k} = getelementptr i8, ptr %s, i64 {o}\n  %{tag}l{k} = load i64, ptr %{tag}p{k}, align 8\n  \
             %{tag}z{k} = zext i64 %{tag}l{k} to i256\n  %{tag}h{k} = shl i256 %{tag}z{k}, {sh}\n  \
             %{tag}v{n} = or i256 {v}, %{tag}h{k}\n",
            o = 32 * s + 8 * k,
            sh = 64 * k,
            n = k + 1
        ));
        v = format!("%{tag}v{}", k + 1);
    }
    for round in 0..2 {
        ir.push_str(&format!(
            "  %{tag}top{round} = lshr i256 {v}, 255\n  %{tag}m{round} = mul i256 %{tag}top{round}, 19\n  \
             %{tag}lo{round} = and i256 {v}, {low}\n  %{tag}f{round} = add i256 %{tag}lo{round}, %{tag}m{round}\n"
        ));
        v = format!("%{tag}f{round}");
    }
    ir.push_str(&format!(
        "  %{tag}plus = add i256 {v}, 19\n  %{tag}over = lshr i256 %{tag}plus, 255\n  \
         %{tag}ge = icmp ne i256 %{tag}over, 0\n  %{tag}minus = and i256 %{tag}plus, {low}\n  \
         %{tag}r = select i1 %{tag}ge, i256 %{tag}minus, i256 {v}\n"
    ));
    for k in 0..4 {
        ir.push_str(&format!(
            "  %{tag}rs{k} = lshr i256 %{tag}r, {sh}\n  %{tag}rw{k} = trunc i256 %{tag}rs{k} to i64\n  \
             %{tag}rp{k} = getelementptr i8, ptr {base}, i64 {o}\n  store i64 %{tag}rw{k}, ptr %{tag}rp{k}, align 8\n",
            sh = 64 * k,
            o = off + 8 * k
        ));
    }
}
