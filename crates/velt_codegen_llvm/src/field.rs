//! The field operations (`velt_field_*`, velt_vir `field.rs`): inline helpers that compute a
//! P-256 Montgomery product or square on registers of the field's array.
//!
//! On x86-64 the product is hand-scheduled assembly, because LLVM keeps the carries of a
//! multi-word product in flags poorly. A CPU with BMI2 and ADX (every x86-64 CPU since about
//! 2014) runs the form with `mulx`, which leaves the flags alone, and two carry chains at once
//! (`adcx` on CF for the low halves, `adox` on OF for the high ones): about 130 instructions. Any
//! other x86-64 CPU runs the form with `mul`, about 180. The choice reads the CPU's feature word
//! (rt_abi.md "CPU check": BMI2 and ADX, so `VELT_CPU_DISABLE` turns the form off), or is made
//! at compile time where `--cpu` guarantees both ([`has_adx`]). Every other target computes the
//! same product in plain IR with 128-bit multiplies.
//!
//! Both assembly forms are one interleaved Montgomery multiplication: for each limb `b_i` of
//! the second operand, the accumulator `x` (five limbs and a carry limb) gains `a * b_i`, and
//! then one round of reduction adds `m p` for `m = x0`, which clears limb 0, and drops that limb.
//! P-256's prime makes the round free of multiplies: `p = 2^256 - 2^224 + 2^192 + 2^96 - 1`, so
//! `x + m p` shifted down a limb is `x1 + (m << 32)`, `x2 + (m >> 32)`, `x3 + m - (m << 32)`,
//! `x4 + m - (m >> 32)` with their carries and borrows. The accumulator stays below `2p`; a last
//! subtraction of `p`, kept or not without a branch, reduces it fully. The registers of the
//! accumulator rotate from one limb to the next instead of moving.

// The generators name limb `k` of each operand and register `k` of the product by index.
#![allow(clippy::needless_range_loop)]

use velt_vir::field::FieldOp;
use velt_vir::vir::Ty;

/// The helper computing field operation `symbol` on target `x86` (x86-64 or not), `adx` where
/// the build guarantees BMI2 and ADX (`--cpu`), `barrier` where the target has inline assembly
/// for the portable form's masks ([`choose`]): its name, what the module must define for it,
/// and the extern's signature.
pub(crate) fn helper(
    symbol: &str,
    x86: bool,
    adx: bool,
    barrier: bool,
) -> Option<(String, Vec<String>, Vec<Ty>, Ty)> {
    let op = FieldOp::of_symbol(symbol)?;
    let (params, ret) = op.signature();
    let name = format!("@velt.field.{}", op.symbol().strip_prefix("velt_field_")?);
    if op == FieldOp::P256Double {
        return Some((name.clone(), double_helper(&name, x86, adx), params, ret));
    }
    if op == FieldOp::X25519Ladder {
        return Some((
            name.clone(),
            crate::field25519::ladder_helper(&name, x86, adx),
            params,
            ret,
        ));
    }
    let b_param = match op {
        FieldOp::P256Sqr => "",
        _ => ", i64 %b",
    };
    let b_index = match op {
        FieldOp::P256Sqr => "%a",
        _ => "%b",
    };
    let mut out = format!(
        "define internal void {name}(ptr %r, i64 %d, i64 %a{b_param}) alwaysinline nounwind {{\n\
         entry:\n  \
         %data = load ptr, ptr %r, align 8\n  \
         %od = shl i64 %d, 5\n  %pd = getelementptr i8, ptr %data, i64 %od\n  \
         %oa = shl i64 %a, 5\n  %pa = getelementptr i8, ptr %data, i64 %oa\n  \
         %ob = shl i64 {b_index}, 5\n  %pb = getelementptr i8, ptr %data, i64 %ob\n"
    );
    let mut defs = vec![];
    if matches!(op, FieldOp::P256Add | FieldOp::P256Sub) {
        if x86 {
            let text = format!(
                "\"{}\"",
                asm_text(&add_sub(op == FieldOp::P256Add, Ops::STANDALONE))
            );
            out.push_str(&format!(
                "  %slot = alloca ptr, align 8\n  store ptr %pd, ptr %slot, align 8\n  \
                 %rs = call {{ ptr, ptr }} asm sideeffect {text}, \"{ADX_CONSTRAINTS}\"(ptr %pa, ptr %pb, ptr elementtype(ptr) %slot)\n  ret void\n}}"
            ));
        } else {
            out.push_str(&portable_add_sub(op == FieldOp::P256Add, barrier));
            out.push_str("  ret void\n}");
        }
        defs.push(out);
        return Some((name, defs, params, ret));
    }
    if x86 {
        let adx_lines = match op {
            FieldOp::P256Sqr => adx_sqr(Ops::STANDALONE),
            _ => adx_mul(Ops::STANDALONE),
        };
        let fast = format!("\"{}\"", asm_text(&adx_lines));
        let plain = format!("\"{}\"", asm_text(&mul_rows()));
        out.push_str(&format!(
            "  %slot = alloca ptr, align 8\n  store ptr %pd, ptr %slot, align 8\n  \
             %fast = call i1 @velt.field.has_adx()\n  \
             br i1 %fast, label %adx, label %plain\n\
             adx:\n  %ra = call {{ ptr, ptr }} asm sideeffect {fast}, \"{ADX_CONSTRAINTS}\"(ptr %pa, ptr %pb, ptr elementtype(ptr) %slot)\n  ret void\n\
             plain:\n  %rp = call {{ ptr, ptr }} asm sideeffect {plain}, \"{CONSTRAINTS}\"(ptr %pa, ptr %pb, ptr %pd)\n  ret void\n}}"
        ));
        defs.extend(has_adx(adx));
    } else {
        out.push_str(&portable(barrier));
        out.push_str("  ret void\n}");
    }
    defs.push(out);
    Some((name, defs, params, ret))
}

/// The operands: `a`'s limbs at RSI, `b`'s at RDI, the result at R15. The last step reuses RSI
/// and RDI, so they are outputs tied to those inputs; everything else the assembly touches is
/// clobbered. Neither RBX (which LLVM may keep as a base pointer) nor RBP.
const CONSTRAINTS: &str = "={rsi},={rdi},0,1,{r15},~{rax},~{rdx},~{rcx},~{r8},~{r9},~{r10},~{r11},~{r12},~{r13},~{r14},~{memory},~{dirflag},~{fpsr},~{flags}";

/// The operands of the BMI2/ADX forms: `a` at RSI, `b` at RDI (outputs tied to them, reused as
/// scratch), and the result's address in memory (`$4`): the full product takes the other eleven
/// registers.
const ADX_CONSTRAINTS: &str = "={rsi},={rdi},0,1,*m,~{rax},~{rdx},~{rcx},~{r8},~{r9},~{r10},~{r11},~{r12},~{r13},~{r14},~{r15},~{memory},~{dirflag},~{fpsr},~{flags}";

/// The full product's limbs `t0..t7`.
pub(crate) const T: [&str; 8] = ["%r8", "%r9", "%r10", "%r11", "%r12", "%r13", "%r14", "%r15"];

/// Where a BMI2/ADX sequence finds its operands and puts its result: the standalone operations
/// read `a` at RSI and `b` at RDI and store at the address in `$4`; the fused point doubling
/// ([`adx_double`]) keeps every value in a scratch area at RSI, at constant offsets.
#[derive(Clone, Copy)]
pub(crate) struct Ops {
    pub(crate) a: usize,
    pub(crate) b: usize,
    /// `None`: at `$4`; `Some(off)`: at `off(%rsi)`.
    pub(crate) dst: Option<usize>,
}

impl Ops {
    /// The standalone operation's operands.
    const STANDALONE: Ops = Ops {
        a: 0,
        b: 0,
        dst: None,
    };

    fn fused(&self) -> bool {
        self.dst.is_some()
    }
    /// Limb `k` of `a`.
    pub(crate) fn a(&self, k: usize) -> String {
        format!("{}(%rsi)", self.a + 8 * k)
    }
    /// Limb `k` of `b`.
    pub(crate) fn b(&self, k: usize) -> String {
        match self.fused() {
            true => format!("{}(%rsi)", self.b + 8 * k),
            false => format!("{}(%rdi)", 8 * k),
        }
    }
    /// Stores `limbs` as the result.
    pub(crate) fn store(&self, out: &mut Vec<String>, limbs: [&str; 4]) {
        let at = match self.dst {
            Some(off) => off,
            None => {
                out.push("movq $4, %rsi".into());
                0
            }
        };
        for (k, r) in limbs.iter().enumerate() {
            out.push(format!("movq {r}, {}(%rsi)", at + 8 * k));
        }
    }
}

/// The scratch area of the fused doubling: the offset of p's top limb, kept there by the helper
/// for `mulx` (a register would be one too many).
const P3_SLOT: usize = 32 * SLOTS;

/// The BMI2/ADX product: `t = a * b` in full (each row's low halves on CF, high halves on OF;
/// no row waits on a reduction), then [`reduce_low`] and [`add_high`].
fn adx_mul(o: Ops) -> Vec<String> {
    let mut out = vec![];
    adx_product(&mut out, o);
    reduce_low(&mut out, o);
    add_high(&mut out, o);
    out
}

/// The full product `a * b` into `T[0..8]`.
pub(crate) fn adx_product(out: &mut Vec<String>, o: Ops) {
    out.push(format!("movq {}, %rdx", o.b(0)));
    out.push(format!("mulxq {}, {}, {}", o.a(0), T[0], T[1]));
    out.push(format!("mulxq {}, %rax, {}", o.a(1), T[2]));
    out.push(format!("addq %rax, {}", T[1]));
    out.push(format!("mulxq {}, %rax, {}", o.a(2), T[3]));
    out.push(format!("adcq %rax, {}", T[2]));
    out.push(format!("mulxq {}, %rax, {}", o.a(3), T[4]));
    out.push(format!("adcq %rax, {}", T[3]));
    out.push(format!("adcq $$0, {}", T[4]));
    for i in 1..4 {
        out.push(format!("movq {}, %rdx", o.b(i)));
        // Zero the row's new top limb, and both flags.
        out.push(format!("xorl {}, {}", low32(T[i + 4]), low32(T[i + 4])));
        for j in 0..4 {
            out.push(format!("mulxq {}, %rax, %rcx", o.a(j)));
            out.push(format!("adcxq %rax, {}", T[i + j]));
            out.push(format!("adoxq %rcx, {}", T[i + j + 1]));
        }
        // OF ended in the top limb, which cannot overflow; CF goes there too.
        out.push(format!("adcq $$0, {}", T[i + 4]));
    }
}

/// The BMI2/ADX square: the six products of distinct limbs once, doubled, plus the four squares.
fn adx_sqr(o: Ops) -> Vec<String> {
    let mut out = vec![];
    adx_square_product(&mut out, o);
    reduce_low(&mut out, o);
    add_high(&mut out, o);
    out
}

/// The full square `a * a` into `T[0..8]`.
pub(crate) fn adx_square_product(out: &mut Vec<String>, o: Ops) {
    // a0 (a1, a2, a3) at limbs 1..4.
    out.push(format!("movq {}, %rdx", o.a(0)));
    out.push(format!("mulxq {}, {}, {}", o.a(1), T[1], T[2]));
    out.push(format!("mulxq {}, %rax, {}", o.a(2), T[3]));
    out.push(format!("addq %rax, {}", T[2]));
    out.push(format!("mulxq {}, %rax, {}", o.a(3), T[4]));
    out.push(format!("adcq %rax, {}", T[3]));
    out.push(format!("adcq $$0, {}", T[4]));
    // a1 (a2, a3) at limbs 3..5.
    out.push(format!("movq {}, %rdx", o.a(1)));
    out.push(format!("xorl {}, {}", low32(T[5]), low32(T[5])));
    out.push(format!("mulxq {}, %rax, %rcx", o.a(2)));
    out.push(format!("adcxq %rax, {}", T[3]));
    out.push(format!("adoxq %rcx, {}", T[4]));
    out.push(format!("mulxq {}, %rax, %rcx", o.a(3)));
    out.push(format!("adcxq %rax, {}", T[4]));
    out.push(format!("adoxq %rcx, {}", T[5]));
    out.push(format!("adcq $$0, {}", T[5]));
    // a2 a3 at limbs 5..6.
    out.push(format!("movq {}, %rdx", o.a(2)));
    out.push(format!("mulxq {}, %rax, {}", o.a(3), T[6]));
    out.push(format!("addq %rax, {}", T[5]));
    out.push(format!("adcq $$0, {}", T[6]));
    // Twice that (limb 7 the carry), then the squares on one chain.
    out.push(format!("xorl {}, {}", low32(T[7]), low32(T[7])));
    out.push(format!("addq {0}, {0}", T[1]));
    for k in 2..7 {
        out.push(format!("adcq {0}, {0}", T[k]));
    }
    out.push(format!("adcq $$0, {}", T[7]));
    out.push(format!("movq {}, %rdx", o.a(0)));
    out.push(format!("mulxq %rdx, {}, %rax", T[0]));
    out.push(format!("movq {}, %rdx", o.a(1)));
    out.push("mulxq %rdx, %rcx, %rdx".into());
    out.push(format!("addq %rax, {}", T[1]));
    out.push(format!("adcq %rcx, {}", T[2]));
    out.push(format!("adcq %rdx, {}", T[3]));
    out.push(format!("movq {}, %rdx", o.a(2)));
    out.push("mulxq %rdx, %rcx, %rdx".into());
    out.push(format!("adcq %rcx, {}", T[4]));
    out.push(format!("adcq %rdx, {}", T[5]));
    out.push(format!("movq {}, %rdx", o.a(3)));
    out.push("mulxq %rdx, %rcx, %rdx".into());
    out.push(format!("adcq %rcx, {}", T[6]));
    out.push(format!("adcq %rdx, {}", T[7]));
}

/// Four rounds on the low half `t0..t3` alone (module docs): round `k` adds `m p / 2^64` for
/// `m` its lowest limb, so the low half stays four limbs; each round's next `m` is ready after
/// one addition. Afterwards limb `k` of the low half is in `T[k]` again.
fn reduce_low(out: &mut Vec<String>, o: Ops) {
    // BMI2 (which the ADX forms have too): RDI the shift count, so a round is three-operand
    // shifts and one `mulx` for `m p3`, with p3 in RSI (the operand pointers are done) or, in the
    // fused doubling whose base RSI stays, in its slot.
    out.push("movl $$32, %edi".into());
    let p3 = match o.fused() {
        true => format!("{P3_SLOT}(%rsi)"),
        false => {
            out.push("movabsq $$0xffffffff00000001, %rsi".into());
            "%rsi".to_string()
        }
    };
    for r in 0..4 {
        let u = |k: usize| T[(r + k) % 4];
        let (u0, u1, u2, u3) = (u(0), u(1), u(2), u(3));
        out.push(format!("movq {u0}, %rdx"));
        out.push(format!("shlxq %rdi, {u0}, %rax"));
        out.push(format!("shrxq %rdi, {u0}, %rcx"));
        out.push(format!("addq %rax, {u1}"));
        out.push(format!("adcq %rcx, {u2}"));
        out.push(format!("mulxq {p3}, %rax, %rcx"));
        out.push(format!("adcq %rax, {u3}"));
        // The round's top limb: the high half of m p3, plus the carry.
        out.push("adcq $$0, %rcx".into());
        out.push(format!("movq %rcx, {u0}"));
    }
}

/// The reduced low half `t0..t3` plus the high half `t4..t7`, a carry in RAX: below `2p`; `p`
/// subtracted when that does not borrow; stored as `o` says.
fn add_high(out: &mut Vec<String>, o: Ops) {
    out.push("movl $$0, %eax".into());
    out.push(format!("addq {}, {}", T[4], T[0]));
    out.push(format!("adcq {}, {}", T[5], T[1]));
    out.push(format!("adcq {}, {}", T[6], T[2]));
    out.push(format!("adcq {}, {}", T[7], T[3]));
    out.push("adcq $$0, %rax".into());
    minus_p(out, o);
}

/// `T[0..4]` with a carry limb in RAX, below `2p`: less `p` unless that borrows, stored.
fn minus_p(out: &mut Vec<String>, o: Ops) {
    for k in 0..4 {
        out.push(format!("movq {}, {}", T[k], T[4 + k]));
    }
    out.push("movl $$0xffffffff, %ecx".into());
    out.push("movabsq $$0xffffffff00000001, %rdx".into());
    out.push(format!("subq $$-1, {}", T[4]));
    out.push(format!("sbbq %rcx, {}", T[5]));
    out.push(format!("sbbq $$0, {}", T[6]));
    out.push(format!("sbbq %rdx, {}", T[7]));
    out.push("sbbq $$0, %rax".into());
    for k in 0..4 {
        out.push(format!("cmovcq {}, {}", T[k], T[4 + k]));
    }
    o.store(out, [T[4], T[5], T[6], T[7]]);
}

/// `a + b` or `a - b` mod p, constant time: the sum less `p` unless that borrows (and the sum
/// did not carry), or the difference plus `p` masked by its borrow. Stored as `o` says.
fn add_sub(add: bool, o: Ops) -> Vec<String> {
    let mut out = vec![];
    for k in 0..4 {
        out.push(format!("movq {}, {}", o.a(k), T[k]));
    }
    out.push("xorl %eax, %eax".into());
    let (first, rest) = if add {
        ("addq", "adcq")
    } else {
        ("subq", "sbbq")
    };
    for k in 0..4 {
        let ins = if k == 0 { first } else { rest };
        out.push(format!("{ins} {}, {}", o.b(k), T[k]));
    }
    if add {
        out.push("adcq $$0, %rax".into());
        minus_p(&mut out, o);
    } else {
        // RAX = all ones on a borrow: p masked by it is RAX, RCX, 0, RDX.
        out.push("sbbq %rax, %rax".into());
        out.push("movl $$0xffffffff, %ecx".into());
        out.push("andq %rax, %rcx".into());
        out.push("movabsq $$0xffffffff00000001, %rdx".into());
        out.push("andq %rax, %rdx".into());
        out.push(format!("addq %rax, {}", T[0]));
        out.push(format!("adcq %rcx, {}", T[1]));
        out.push(format!("adcq $$0, {}", T[2]));
        out.push(format!("adcq %rdx, {}", T[3]));
        o.store(&mut out, [T[0], T[1], T[2], T[3]]);
    }
    out
}

/// The fused doubling's helper: on x86-64 with BMI2 and ADX, the point copied into a scratch
/// area, [`adx_double`], the result copied out, and true; false elsewhere. Out of line: it is
/// two thousand instructions, and its callers double in a loop.
fn double_helper(name: &str, x86: bool, adx: bool) -> Vec<String> {
    if !x86 {
        return vec![format!(
            "define internal i1 {name}(ptr %r, i64 %d, i64 %p) alwaysinline nounwind {{\n  ret i1 false\n}}"
        )];
    }
    let mut ir = format!(
        "define internal i1 {name}(ptr %r, i64 %d, i64 %p) noinline nounwind {{\n\
         entry:\n  %fast = call i1 @velt.field.has_adx()\n  br i1 %fast, label %go, label %no\n\
         no:\n  ret i1 false\n\
         go:\n  %data = load ptr, ptr %r, align 8\n  \
         %op = shl i64 %p, 5\n  %pp = getelementptr i8, ptr %data, i64 %op\n  \
         %od = shl i64 %d, 5\n  %pd = getelementptr i8, ptr %data, i64 %od\n  \
         %s = alloca [{} x i64], align 16\n",
        4 * SLOTS + 1
    );
    // X, Y, Z into slots 0..2 (12 limbs), p's top limb after the slots.
    for k in 0..12 {
        ir.push_str(&format!(
            "  %ip{k} = getelementptr i8, ptr %pp, i64 {o}\n  %iv{k} = load i64, ptr %ip{k}, align 8\n  %is{k} = getelementptr i8, ptr %s, i64 {o}\n  store i64 %iv{k}, ptr %is{k}, align 8\n",
            o = 8 * k
        ));
    }
    ir.push_str(&format!(
        "  %p3 = getelementptr i8, ptr %s, i64 {P3_SLOT}\n  store i64 {}, ptr %p3, align 8\n",
        velt_vir::field::P256[3] as i64
    ));
    ir.push_str(&format!(
        "  %rs = call {{ ptr, ptr }} asm sideeffect \"{}\", \"={{rsi}},={{rdi}},0,1,~{{rax}},~{{rdx}},~{{rcx}},~{{r8}},~{{r9}},~{{r10}},~{{r11}},~{{r12}},~{{r13}},~{{r14}},~{{r15}},~{{memory}},~{{dirflag}},~{{fpsr}},~{{flags}}\"(ptr %s, ptr %s)\n",
        asm_text(&adx_double())
    ));
    // X3, Y3, Z3 from slots 9, 10, 8.
    for (c, slot) in [(0usize, 9usize), (1, 10), (2, 8)] {
        for k in 0..4 {
            ir.push_str(&format!(
                "  %os{c}{k} = getelementptr i8, ptr %s, i64 {}\n  %ov{c}{k} = load i64, ptr %os{c}{k}, align 8\n  %op{c}{k} = getelementptr i8, ptr %pd, i64 {}\n  store i64 %ov{c}{k}, ptr %op{c}{k}, align 8\n",
                32 * slot + 8 * k,
                32 * c + 8 * k
            ));
        }
    }
    ir.push_str("  ret i1 true\n}");
    let mut defs = has_adx(adx);
    defs.push(ir);
    defs
}

/// `2^k a` (k >= 1), or `3 a` (`triple`), at `d`: the value stays in `T[0..4]` from one doubling
/// to the next, each a carry chain and a subtraction of `p` kept or not, with one load and one
/// store in all (offsets in the scratch area at RSI).
fn small_multiple(out: &mut Vec<String>, d: usize, a: usize, k: usize, triple: bool) {
    let o = Ops {
        a,
        b: a,
        dst: Some(d),
    };
    for j in 0..4 {
        out.push(format!("movq {}, {}", o.a(j), T[j]));
    }
    let steps = if triple { 2 } else { k };
    for step in 0..steps {
        out.push("xorl %eax, %eax".into());
        if triple && step == 1 {
            // 2a + a.
            out.push(format!("addq {}, {}", o.a(0), T[0]));
            for j in 1..4 {
                out.push(format!("adcq {}, {}", o.a(j), T[j]));
            }
        } else {
            out.push(format!("addq {0}, {0}", T[0]));
            for j in 1..4 {
                out.push(format!("adcq {0}, {0}", T[j]));
            }
        }
        out.push("adcq $$0, %rax".into());
        // Less p unless that borrows, back into T[0..4] (the last step stores instead).
        for j in 0..4 {
            out.push(format!("movq {}, {}", T[j], T[4 + j]));
        }
        out.push("movl $$0xffffffff, %ecx".into());
        out.push("movabsq $$0xffffffff00000001, %rdx".into());
        out.push(format!("subq $$-1, {}", T[4]));
        out.push(format!("sbbq %rcx, {}", T[5]));
        out.push(format!("sbbq $$0, {}", T[6]));
        out.push(format!("sbbq %rdx, {}", T[7]));
        out.push("sbbq $$0, %rax".into());
        for j in 0..4 {
            out.push(format!("cmovncq {}, {}", T[4 + j], T[j]));
        }
    }
    o.store(out, [T[0], T[1], T[2], T[3]]);
}

/// The scratch slots of the fused doubling (32 bytes each), after the input point's three.
const SLOTS: usize = 11;

/// The fused point doubling (dbl-2001-b, a = -3), all in the scratch area at RSI: slots 0..2 the
/// input X, Y, Z (the helper copies them in), the result X3, Y3, Z3 in slots 9, 10, 8 (copied
/// out); every step one of the sequences above at constant offsets, with no call, check or
/// address arithmetic between them.
fn adx_double() -> Vec<String> {
    let s = |k: usize| 32 * k;
    let mut out = vec![];
    let mul = |out: &mut Vec<String>, d: usize, a: usize, b: usize| {
        out.extend(adx_mul(Ops {
            a: s(a),
            b: s(b),
            dst: Some(s(d)),
        }))
    };
    let sqr = |out: &mut Vec<String>, d: usize, a: usize| {
        out.extend(adx_sqr(Ops {
            a: s(a),
            b: 0,
            dst: Some(s(d)),
        }))
    };
    let add = |out: &mut Vec<String>, d: usize, a: usize, b: usize| {
        out.extend(add_sub(
            true,
            Ops {
                a: s(a),
                b: s(b),
                dst: Some(s(d)),
            },
        ))
    };
    let sub = |out: &mut Vec<String>, d: usize, a: usize, b: usize| {
        out.extend(add_sub(
            false,
            Ops {
                a: s(a),
                b: s(b),
                dst: Some(s(d)),
            },
        ))
    };
    let (x, y, z) = (0, 1, 2);
    let (delta, gamma, beta, alpha, t, z3, x3, y3) = (3, 4, 5, 6, 7, 8, 9, 10);
    sqr(&mut out, delta, z);
    sqr(&mut out, gamma, y);
    mul(&mut out, beta, x, gamma);
    // alpha = 3 (X - delta) (X + delta).
    sub(&mut out, alpha, x, delta);
    add(&mut out, t, x, delta);
    mul(&mut out, alpha, alpha, t);
    small_multiple(&mut out, s(alpha), s(alpha), 1, true);
    // Z3 = (Y + Z)^2 - gamma - delta.
    add(&mut out, t, y, z);
    sqr(&mut out, t, t);
    sub(&mut out, t, t, gamma);
    sub(&mut out, z3, t, delta);
    // X3 = alpha^2 - 8 beta.
    sqr(&mut out, x3, alpha);
    small_multiple(&mut out, s(beta), s(beta), 2, false);
    small_multiple(&mut out, s(t), s(beta), 1, false);
    sub(&mut out, x3, x3, t);
    // Y3 = alpha (4 beta - X3) - 8 gamma^2.
    sub(&mut out, beta, beta, x3);
    mul(&mut out, beta, alpha, beta);
    sqr(&mut out, gamma, gamma);
    small_multiple(&mut out, s(gamma), s(gamma), 3, false);
    sub(&mut out, y3, beta, gamma);
    out
}

/// IR that sets `{tag}{k}` to `keep ? a[k] : b[k]` for each limb, with no `select` on `keep` where
/// `barrier` (a target with inline assembly): `keep` is a secret (whether a carry happened), and
/// LLVM may lower a `select` to a branch. A mask `0 - keep` picks instead, `b ^ ((a ^ b) & m)`,
/// with the mask through an empty `asm` so that the optimizer can't see it is a comparison and
/// fold the expression back into a `select` (rt_abi.md "Field arithmetic", constant time;
/// veltc's `p256_masks` test reads the optimized IR). Without inline assembly (wasm32) it stays a
/// `select`.
fn choose(tag: &str, keep: &str, a: &[String], b: &[String], barrier: bool) -> String {
    let mut ir = String::new();
    if !barrier {
        for k in 0..a.len() {
            ir.push_str(&format!(
                "  %{tag}{k} = select i1 {keep}, i128 {}, i128 {}\n",
                a[k], b[k]
            ));
        }
        return ir;
    }
    ir.push_str(&format!(
        "  %{tag}kz = zext i1 {keep} to i64\n  %{tag}kn = sub i64 0, %{tag}kz\n  \
         %{tag}kb = call i64 asm \"\", \"=r,0\"(i64 %{tag}kn)\n  %{tag}m = sext i64 %{tag}kb to i128\n"
    ));
    for k in 0..a.len() {
        ir.push_str(&format!(
            "  %{tag}x{k} = xor i128 {}, {}\n  %{tag}y{k} = and i128 %{tag}x{k}, %{tag}m\n  %{tag}{k} = xor i128 {}, %{tag}y{k}\n",
            a[k], b[k], b[k]
        ));
    }
    ir
}

/// [`add_sub`] in plain IR, from `%pa`, `%pb` to `%pd` (`barrier`: [`choose`]).
fn portable_add_sub(add: bool, barrier: bool) -> String {
    let p = velt_vir::field::P256;
    let mut ir = String::new();
    for k in 0..4 {
        for (base, x) in [("%pa", "a"), ("%pb", "b")] {
            ir.push_str(&format!(
                "  %{x}p{k} = getelementptr i8, ptr {base}, i64 {}\n  %{x}{k} = load i64, ptr %{x}p{k}, align 8\n  %{x}w{k} = zext i64 %{x}{k} to i128\n",
                8 * k
            ));
        }
    }
    let (op1, op2, sh1, sh2) = if add {
        ("add", "sub", 64, 127)
    } else {
        ("sub", "add", 127, 64)
    };
    let mut c = "0".to_string();
    for k in 0..4 {
        ir.push_str(&format!(
            "  %s{k}a = {op1} i128 %aw{k}, %bw{k}\n  %s{k}b = {op1} i128 %s{k}a, {c}\n  %s{k} = and i128 %s{k}b, 18446744073709551615\n  %c{k} = lshr i128 %s{k}b, {sh1}\n"
        ));
        c = format!("%c{k}");
    }
    let mut c2 = "0".to_string();
    for k in 0..4 {
        ir.push_str(&format!(
            "  %t{k}a = {op2} i128 %s{k}, {}\n  %t{k}b = {op2} i128 %t{k}a, {c2}\n  %t{k} = and i128 %t{k}b, 18446744073709551615\n  %e{k} = lshr i128 %t{k}b, {sh2}\n",
            p[k]
        ));
        c2 = format!("%e{k}");
    }
    if add {
        ir.push_str(
            "  %nc = icmp eq i128 %c3, 0\n  %bw = icmp ne i128 %e3, 0\n  %keep = and i1 %nc, %bw\n",
        );
    } else {
        ir.push_str("  %keep = icmp eq i128 %c3, 0\n");
    }
    let kept: Vec<String> = (0..4).map(|k| format!("%s{k}")).collect();
    let reduced: Vec<String> = (0..4).map(|k| format!("%t{k}")).collect();
    ir.push_str(&choose("r", "%keep", &kept, &reduced, barrier));
    for k in 0..4 {
        ir.push_str(&format!(
            "  %w{k} = trunc i128 %r{k} to i64\n  %dp{k} = getelementptr i8, ptr %pd, i64 {}\n  store i64 %w{k}, ptr %dp{k}, align 8\n",
            8 * k
        ));
    }
    ir
}

/// `@velt.field.has_adx()`, whether the CPU has BMI2 and ADX, and what it needs: `true` where the
/// build guarantees both (`known`, from `--cpu`), else the CPU feature word's two bits
/// (`adx-x86`'s mask, rt_abi.md "CPU check"), which std's start-up hook set before any Velt code
/// runs, read as invariant.
pub(crate) fn has_adx(known: bool) -> Vec<String> {
    if known {
        return vec![
            "define internal i1 @velt.field.has_adx() alwaysinline nounwind {\nentry:\n  ret i1 true\n}"
                .to_string(),
        ];
    }
    let mask = velt_vir::simd::Ext::AdxX86.mask();
    vec![
        format!(
            "define internal i1 @velt.field.has_adx() alwaysinline nounwind {{\nentry:\n  \
             %w = load i64, ptr @velt_cpu_word, align 8, !invariant.load !{{}}\n  \
             %m = and i64 %w, {mask}\n  %r = icmp eq i64 %m, {mask}\n  ret i1 %r\n}}"
        ),
        crate::simd::WORD.to_string(),
    ]
}

/// The six accumulator registers, rotated: limb `k` of row `i` is `ACC[(i + k) % 6]`.
const ACC: [&str; 6] = ["%r8", "%r9", "%r10", "%r11", "%r12", "%r13"];

fn x(i: usize, k: usize) -> &'static str {
    ACC[(i + k) % 6]
}

/// The reduction round of row `i`: `x0` is `m`; `x1..x5` gain `m p / 2^64` (module docs), and
/// `x0`'s register is free afterwards (it holds the high part of `m (2^64 - 2^32 + 1)`).
fn reduce(out: &mut Vec<String>, i: usize) {
    let (x0, x1, x2, x3, x4, x5) = (x(i, 0), x(i, 1), x(i, 2), x(i, 3), x(i, 4), x(i, 5));
    out.push(format!("movq {x0}, %rax"));
    out.push("shlq $$32, %rax".into());
    out.push(format!("movq {x0}, %rdx"));
    out.push("shrq $$32, %rdx".into());
    out.push(format!("movq {x0}, %rcx"));
    // m - (m << 32), and m - (m >> 32) less that borrow: m (2^64 - 2^32 + 1) in two limbs.
    out.push("subq %rax, %rcx".into());
    out.push(format!("sbbq %rdx, {x0}"));
    out.push(format!("addq %rax, {x1}"));
    out.push(format!("adcq %rdx, {x2}"));
    out.push(format!("adcq %rcx, {x3}"));
    out.push(format!("adcq {x0}, {x4}"));
    out.push(format!("adcq $$0, {x5}"));
}

/// The last step: the accumulator `y` (row 4's limbs 0..3, and limb 4 a carry) is below `2p`;
/// `y - p` when that does not borrow, else `y`, stored at R15.
fn finish(out: &mut Vec<String>) {
    let y: Vec<&str> = (0..5).map(|k| x(4, k)).collect();
    out.push(format!("movq {}, %rax", y[0]));
    out.push(format!("movq {}, %rdx", y[1]));
    out.push(format!("movq {}, %rcx", y[2]));
    out.push(format!("movq {}, %rsi", y[3]));
    out.push("movl $$0xffffffff, %edi".into());
    out.push("movabsq $$0xffffffff00000001, %r14".into());
    out.push("subq $$-1, %rax".into());
    out.push("sbbq %rdi, %rdx".into());
    out.push("sbbq $$0, %rcx".into());
    out.push("sbbq %r14, %rsi".into());
    out.push(format!("sbbq $$0, {}", y[4]));
    // A borrow: y was below p, keep it.
    out.push(format!("cmovcq {}, %rax", y[0]));
    out.push(format!("cmovcq {}, %rdx", y[1]));
    out.push(format!("cmovcq {}, %rcx", y[2]));
    out.push(format!("cmovcq {}, %rsi", y[3]));
    out.push("movq %rax, (%r15)".into());
    out.push("movq %rdx, 8(%r15)".into());
    out.push("movq %rcx, 16(%r15)".into());
    out.push("movq %rsi, 24(%r15)".into());
}

/// The rows with `mul`, whose flags end every carry chain: each product's high half takes the
/// carries of the two additions into its limb, and passes on in RCX.
fn mul_rows() -> Vec<String> {
    let mut out = vec![];
    for i in 0..4 {
        let (x0, x1, x2, x3, x4, x5) = (x(i, 0), x(i, 1), x(i, 2), x(i, 3), x(i, 4), x(i, 5));
        out.push(format!("movq {}(%rdi), %r14", 8 * i));
        if i == 0 {
            // The first row sets the accumulator.
            out.push("movq (%rsi), %rax".into());
            out.push("mulq %r14".into());
            out.push(format!("movq %rax, {x0}"));
            out.push(format!("movq %rdx, {x1}"));
            for (j, xj) in [(1, x1), (2, x2)] {
                let next = x(i, j + 1);
                out.push(format!("movq {}(%rsi), %rax", 8 * j));
                out.push("mulq %r14".into());
                out.push(format!("addq %rax, {xj}"));
                out.push("adcq $$0, %rdx".into());
                out.push(format!("movq %rdx, {next}"));
            }
            out.push("movq 24(%rsi), %rax".into());
            out.push("mulq %r14".into());
            out.push(format!("addq %rax, {x3}"));
            out.push("adcq $$0, %rdx".into());
            out.push(format!("movq %rdx, {x4}"));
            out.push(format!("xorl {}, {}", low32(x5), low32(x5)));
        } else {
            out.push(format!("xorl {}, {}", low32(x5), low32(x5)));
            out.push("movq (%rsi), %rax".into());
            out.push("mulq %r14".into());
            out.push(format!("addq %rax, {x0}"));
            out.push("adcq $$0, %rdx".into());
            out.push("movq %rdx, %rcx".into());
            for (j, xj) in [(1, x1), (2, x2), (3, x3)] {
                out.push(format!("movq {}(%rsi), %rax", 8 * j));
                out.push("mulq %r14".into());
                out.push(format!("addq %rcx, {xj}"));
                out.push("adcq $$0, %rdx".into());
                out.push(format!("addq %rax, {xj}"));
                out.push("adcq $$0, %rdx".into());
                if j < 3 {
                    out.push("movq %rdx, %rcx".into());
                }
            }
            out.push(format!("addq %rdx, {x4}"));
            out.push(format!("adcq $$0, {x5}"));
        }
        reduce(&mut out, i);
    }
    finish(&mut out);
    out
}

/// The 32-bit name of a 64-bit register (`%r8` is `%r8d`).
pub(crate) fn low32(r: &str) -> String {
    format!("{r}d")
}

/// The assembly text of `lines` as LLVM IR's string escapes have it: one instruction a line.
pub(crate) fn asm_text(lines: &[String]) -> String {
    lines.join("\\0A")
}

/// The product in plain IR, from `%pa`, `%pb` to `%pd`: the reference's algorithm, which LLVM
/// lowers to the target's multiply-high and add-with-carry (`barrier`: [`choose`]).
fn portable(barrier: bool) -> String {
    let mut ir = String::new();
    let mut n = 0usize;
    let mut v = |ir: &mut String, text: String| -> String {
        n += 1;
        let name = format!("%f{n}");
        ir.push_str(&format!("  {name} = {text}\n"));
        name
    };
    let mut load = |ir: &mut String, base: &str, k: usize| -> String {
        let p = v(ir, format!("getelementptr i8, ptr {base}, i64 {}", 8 * k));
        let w = v(ir, format!("load i64, ptr {p}, align 8"));
        v(ir, format!("zext i64 {w} to i128"))
    };
    let a: Vec<String> = (0..4).map(|k| load(&mut ir, "%pa", k)).collect();
    let b: Vec<String> = (0..4).map(|k| load(&mut ir, "%pb", k)).collect();
    let p = velt_vir::field::P256.map(|w| w.to_string());
    // t[0..9] as i128 values holding 64-bit limbs.
    let mut t: Vec<String> = vec!["0".to_string(); 9];
    for i in 0..4 {
        let mut carry = "0".to_string();
        for j in 0..4 {
            let prod = v(&mut ir, format!("mul i128 {}, {}", a[i], b[j]));
            let s1 = v(&mut ir, format!("add i128 {prod}, {}", t[i + j]));
            let s = v(&mut ir, format!("add i128 {s1}, {carry}"));
            t[i + j] = v(&mut ir, format!("and i128 {s}, 18446744073709551615"));
            carry = v(&mut ir, format!("lshr i128 {s}, 64"));
        }
        t[i + 4] = carry;
    }
    for i in 0..4 {
        let m = t[i].clone();
        let mut carry = "0".to_string();
        for (j, pj) in p.iter().enumerate() {
            let prod = v(&mut ir, format!("mul i128 {m}, {pj}"));
            let s1 = v(&mut ir, format!("add i128 {prod}, {}", t[i + j]));
            let s = v(&mut ir, format!("add i128 {s1}, {carry}"));
            t[i + j] = v(&mut ir, format!("and i128 {s}, 18446744073709551615"));
            carry = v(&mut ir, format!("lshr i128 {s}, 64"));
        }
        for k in i + 4..9 {
            let s = v(&mut ir, format!("add i128 {}, {carry}", t[k]));
            t[k] = v(&mut ir, format!("and i128 {s}, 18446744073709551615"));
            carry = v(&mut ir, format!("lshr i128 {s}, 64"));
        }
    }
    // d = t[4..8] - p with its borrow; keep t when that borrows and t[8] is 0.
    let mut borrow = "0".to_string();
    let mut d = vec![];
    for (j, pj) in p.iter().enumerate() {
        let s1 = v(&mut ir, format!("sub i128 {}, {pj}", t[4 + j]));
        let s = v(&mut ir, format!("sub i128 {s1}, {borrow}"));
        d.push(v(&mut ir, format!("and i128 {s}, 18446744073709551615")));
        let neg = v(&mut ir, format!("lshr i128 {s}, 127"));
        borrow = neg;
    }
    let under = v(&mut ir, format!("icmp ne i128 {borrow}, 0"));
    let top0 = v(&mut ir, format!("icmp eq i128 {}, 0", t[8]));
    let keep = v(&mut ir, format!("and i1 {under}, {top0}"));
    let kept: Vec<String> = (0..4).map(|j| t[4 + j].clone()).collect();
    ir.push_str(&choose("fr", &keep, &kept, &d, barrier));
    for j in 0..4 {
        let w = v(&mut ir, format!("trunc i128 %fr{j} to i64"));
        let at = v(&mut ir, format!("getelementptr i8, ptr %pd, i64 {}", 8 * j));
        ir.push_str(&format!("  store i64 {w}, ptr {at}, align 8\n"));
    }
    ir
}
