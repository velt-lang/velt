//! The field operations' portable forms (`field.rs`): plain IR with 128-bit multiplies, for every
//! target without the x86-64 assembly (arm64, wasm32), and their constant-time choice of the
//! reduced result ([`choose`]).

// The generators name limb `k` of each operand by index.
#![allow(clippy::needless_range_loop)]

/// How [`choose`] picks the reduced result without the optimizer seeing a choice it may turn into
/// a branch or a load through a selected address: whether to keep it is a secret (whether a
/// carry happened). rt_abi.md "Field arithmetic", constant time; veltc's `p256_masks` test reads
/// the optimized IR and finds any `select` whose condition comes from the secret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    /// arm64: `cmp` and a `csel` per limb in one inline-asm block, what a `select` compiles to
    /// there, where the optimizer can't see it. A mask per limb (`Mask`) cost ECDH 10% on arm64.
    Csel,
    /// Another target with inline assembly (x86-64, which only takes the portable form in
    /// tests): a mask `0 - keep` through an empty `asm` value barrier, `b ^ ((a ^ b) & m)`.
    Mask,
    /// No inline assembly (wasm32): a `select`, which the engine compiles as it likes.
    Select,
}

impl Choice {
    /// The form for code generation on `arch`.
    pub(crate) fn of(arch: crate::simd::Arch) -> Choice {
        match arch {
            crate::simd::Arch::Neon => Choice::Csel,
            crate::simd::Arch::Sse2 => Choice::Mask,
            crate::simd::Arch::Wasm => Choice::Select,
        }
    }
}

/// IR that sets `{tag}{k}` to `keep ? a[k] : b[k]` for each limb (64-bit values in `i128`s), by
/// `how`.
pub(crate) fn choose(tag: &str, keep: &str, a: &[String], b: &[String], how: Choice) -> String {
    let mut ir = String::new();
    let n = a.len();
    match how {
        Choice::Select => {
            for k in 0..n {
                ir.push_str(&format!(
                    "  %{tag}{k} = select i1 {keep}, i128 {}, i128 {}\n",
                    a[k], b[k]
                ));
            }
        }
        Choice::Mask => {
            ir.push_str(&format!(
                "  %{tag}kz = zext i1 {keep} to i64\n  %{tag}kn = sub i64 0, %{tag}kz\n  \
                 %{tag}kb = call i64 asm \"\", \"=r,0\"(i64 %{tag}kn)\n  %{tag}m = sext i64 %{tag}kb to i128\n"
            ));
            for k in 0..n {
                ir.push_str(&format!(
                    "  %{tag}x{k} = xor i128 {}, {}\n  %{tag}y{k} = and i128 %{tag}x{k}, %{tag}m\n  %{tag}{k} = xor i128 {}, %{tag}y{k}\n",
                    a[k], b[k], b[k]
                ));
            }
        }
        Choice::Csel => {
            // Outputs $0..n tied to a's limbs, then b's limbs at $2n.., then the condition at $3n.
            ir.push_str(&format!("  %{tag}kz = zext i1 {keep} to i64\n"));
            let mut text = format!("cmp ${}, #0", 3 * n);
            for k in 0..n {
                text.push_str(&format!("\\0Acsel ${k}, ${k}, ${}, ne", 2 * n + k));
                ir.push_str(&format!(
                    "  %{tag}a{k} = trunc i128 {} to i64\n  %{tag}b{k} = trunc i128 {} to i64\n",
                    a[k], b[k]
                ));
            }
            let outs = vec!["=r"; n].join(",");
            let tied: Vec<String> = (0..n).map(|k| k.to_string()).collect();
            let rs = vec!["r"; n + 1].join(",");
            let tys = vec!["i64"; n].join(", ");
            let args: Vec<String> = (0..n)
                .map(|k| format!("i64 %{tag}a{k}"))
                .chain((0..n).map(|k| format!("i64 %{tag}b{k}")))
                .chain(std::iter::once(format!("i64 %{tag}kz")))
                .collect();
            ir.push_str(&format!(
                "  %{tag}cs = call {{ {tys} }} asm \"{text}\", \"{outs},{},{rs},~{{cc}}\"({})\n",
                tied.join(","),
                args.join(", ")
            ));
            for k in 0..n {
                ir.push_str(&format!(
                    "  %{tag}w{k} = extractvalue {{ {tys} }} %{tag}cs, {k}\n  %{tag}{k} = zext i64 %{tag}w{k} to i128\n"
                ));
            }
        }
    }
    ir
}

/// `field.rs`'s `add_sub` in plain IR, from `%pa`, `%pb` to `%pd` (`how`: [`choose`]).
pub(crate) fn add_sub(add: bool, how: Choice) -> String {
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
    ir.push_str(&choose("r", "%keep", &kept, &reduced, how));
    for k in 0..4 {
        ir.push_str(&format!(
            "  %w{k} = trunc i128 %r{k} to i64\n  %dp{k} = getelementptr i8, ptr %pd, i64 {}\n  store i64 %w{k}, ptr %dp{k}, align 8\n",
            8 * k
        ));
    }
    ir
}

/// The product in plain IR, from `%pa`, `%pb` to `%pd`: the reference's algorithm, which LLVM
/// lowers to the target's multiply-high and add-with-carry (`how`: [`choose`]).
pub(crate) fn product(how: Choice) -> String {
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
    ir.push_str(&choose("fr", &keep, &kept, &d, how));
    for j in 0..4 {
        let w = v(&mut ir, format!("trunc i128 %fr{j} to i64"));
        let at = v(&mut ir, format!("getelementptr i8, ptr %pd, i64 {}", 8 * j));
        ir.push_str(&format!("  store i64 {w}, ptr {at}, align 8\n"));
    }
    ir
}
