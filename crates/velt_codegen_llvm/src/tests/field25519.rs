//! The X25519 ladder's helper (`field25519.rs`) compiled with clang for the host and run on
//! RFC 7748's vector, edge u-coordinates (0, 1, p - 1, p, p + 1, 2^255 - 1, 2^256 - 1) and
//! pseudo-random ones with pseudo-random clamped scalars, against velt_vir's `x25519_ladder`: on
//! x86-64 with BMI2 and ADX the assembly, elsewhere the helper must say it did nothing (false).
//! Skipped (with a note) without clang.

use std::path::Path;
use std::process::Command;

use velt_vir::field::{x25519_ladder, P25519};

/// `(u, k)` pairs: u below 2^256, k clamped.
fn inputs() -> Vec<([u64; 4], [u64; 4])> {
    let clamp = |mut k: [u64; 4]| {
        k[0] &= !7;
        k[3] &= 0x7fff_ffff_ffff_ffff;
        k[3] |= 0x4000_0000_0000_0000;
        k
    };
    let mut s = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let words = |hex: &str| -> [u64; 4] {
        let b: Vec<u8> = (0..32)
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        std::array::from_fn(|j| u64::from_le_bytes(b[8 * j..8 * j + 8].try_into().unwrap()))
    };
    let mut rfc_u = words("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c");
    rfc_u[3] &= 0x7fff_ffff_ffff_ffff;
    let rfc_k = clamp(words(
        "a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4",
    ));
    let p = P25519;
    let mut out = vec![(rfc_u, rfc_k)];
    let edges = [
        [0, 0, 0, 0],
        [1, 0, 0, 0],
        [p[0] - 1, p[1], p[2], p[3]],
        p,
        [p[0] + 1, p[1], p[2], p[3]],
        [u64::MAX, u64::MAX, u64::MAX, 0x7fff_ffff_ffff_ffff],
        [u64::MAX; 4],
        [9, 0, 0, 0],
    ];
    for u in edges {
        out.push((u, clamp([next(), next(), next(), next()])));
    }
    while out.len() < 60 {
        out.push((
            [next(), next(), next(), next()],
            clamp([next(), next(), next(), next()]),
        ));
    }
    out
}

/// Reads pairs (u, k: 8 limbs each) from argv[1], sets the CPU byte to argv[3], and writes for
/// each the flag and registers 0 and 1 to argv[2].
const DRIVER: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
typedef struct { uint64_t *data; uint64_t len, cap; } arr;
void set_cpu(uint8_t);
uint8_t ladder(arr *, uint64_t, uint64_t, uint64_t);
int main(int argc, char **argv) {
  FILE *in = fopen(argv[1], "rb"), *out = fopen(argv[2], "wb");
  uint64_t n; fread(&n, 8, 1, in);
  uint64_t *e = malloc(64 * n); fread(e, 64, n, in);
  set_cpu((uint8_t)atoi(argv[3]));
  uint64_t regs[16]; arr r = { regs, 16, 16 };
  for (uint64_t i = 0; i < n; i++) {
    memset(regs, 0, sizeof regs);
    memcpy(regs + 8, e + 8 * i, 64);
    uint64_t done = ladder(&r, 0, 2, 3);
    fwrite(&done, 8, 1, out); fwrite(regs, 8, 8, out);
  }
  fclose(out);
  return 0;
}
"#;

#[test]
fn the_x25519_ladder_matches_the_reference() {
    if cfg!(windows) {
        eprintln!("note: skipping the X25519 ladder run on Windows");
        return;
    }
    let Some(clang) = crate::find_clang() else {
        eprintln!("note: clang not available; skipping the X25519 ladder run");
        return;
    };
    let dir = std::env::temp_dir().join(format!("velt_x25519_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    struct Cleanup<'a>(&'a Path);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0);
        }
    }
    let _cleanup = Cleanup(&dir);
    let x86 = cfg!(target_arch = "x86_64");
    #[cfg(target_arch = "x86_64")]
    let adx =
        std::arch::is_x86_feature_detected!("bmi2") && std::arch::is_x86_feature_detected!("adx");
    #[cfg(not(target_arch = "x86_64"))]
    let adx = false;
    // Form 2: the CPU has BMI2 and ADX (asserted only where it does); form 1: it has not.
    run(&clang, &dir, x86, "1", false);
    if adx {
        run(&clang, &dir, x86, "2", true);
    }
}

fn run(clang: &Path, dir: &Path, x86: bool, cpu: &str, fused: bool) {
    let (name, defs, _, _) = crate::field::helper(
        "velt_field_x25519_ladder",
        x86,
        false,
        crate::field_portable::Choice::Mask,
    )
    .expect("a helper");
    let mut m = defs.join("\n");
    m.push_str(&format!(
        "\ndefine i8 @ladder(ptr %r, i64 %d, i64 %u, i64 %k) {{\n  %v = call i1 {name}(ptr %r, i64 %d, i64 %u, i64 %k)\n  %w = zext i1 %v to i8\n  ret i8 %w\n}}\n"
    ));
    m.push_str(match x86 {
        true => "define void @set_cpu(i8 %v) {\n  %y = icmp eq i8 %v, 2\n  %w = select i1 %y, i64 16777728, i64 0\n  store i64 %w, ptr @velt_cpu_word, align 8\n  ret void\n}\n",
        false => "define void @set_cpu(i8 %v) {\n  ret void\n}\n",
    });
    let (ll, c, exe) = (dir.join("x.ll"), dir.join("d.c"), dir.join("x.exe"));
    std::fs::write(&ll, m).expect("write .ll");
    std::fs::write(&c, DRIVER).expect("write driver");
    let built = Command::new(clang)
        .args(["-O2", "-w", "-o"])
        .arg(&exe)
        .arg(&ll)
        .arg(&c)
        .output()
        .expect("run clang");
    let err = String::from_utf8_lossy(&built.stderr);
    assert!(built.status.success(), "clang failed:\n{err}");
    let pairs = inputs();
    let mut bytes = (pairs.len() as u64).to_le_bytes().to_vec();
    for (u, k) in &pairs {
        for w in u.iter().chain(k) {
            bytes.extend(w.to_le_bytes());
        }
    }
    let (input, output) = (dir.join("in.bin"), dir.join("out.bin"));
    std::fs::write(&input, bytes).expect("write inputs");
    let status = Command::new(&exe)
        .arg(&input)
        .arg(&output)
        .arg(cpu)
        .status()
        .expect("run");
    assert!(status.success(), "driver failed");
    let got = std::fs::read(&output).expect("read results");
    let mut got = got
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| u64::from_le_bytes(*c));
    let mut bad = vec![];
    for (u, k) in &pairs {
        let done = got.next().expect("a flag") & 0xff;
        let x2: [u64; 4] = std::array::from_fn(|_| got.next().expect("a limb"));
        let z2: [u64; 4] = std::array::from_fn(|_| got.next().expect("a limb"));
        if (done != 0) != fused {
            bad.push(format!("cpu {cpu}: done {done}, want {fused}"));
            continue;
        }
        if fused && (x2, z2) != x25519_ladder(*u, *k) && bad.len() < 10 {
            bad.push(format!(
                "u {u:x?} k {k:x?}: {x2:x?} {z2:x?}, want {:x?}",
                x25519_ladder(*u, *k)
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// A little-endian number of any length, for the reference below.
type Big = Vec<u64>;

fn add_big(a: &[u64], b: &[u64]) -> Big {
    let n = a.len().max(b.len()) + 1;
    let mut out = vec![0; n];
    let mut carry = 0u128;
    for (i, o) in out.iter_mut().enumerate() {
        let s = carry + *a.get(i).unwrap_or(&0) as u128 + *b.get(i).unwrap_or(&0) as u128;
        *o = s as u64;
        carry = s >> 64;
    }
    out
}

fn mul_big(a: &[u64], b: &[u64]) -> Big {
    let mut out = vec![0u64; a.len() + b.len()];
    for (i, &x) in a.iter().enumerate() {
        let mut carry = 0u128;
        for (j, &y) in b.iter().enumerate() {
            let t = out[i + j] as u128 + x as u128 * y as u128 + carry;
            out[i + j] = t as u64;
            carry = t >> 64;
        }
        out[i + b.len()] = carry as u64;
    }
    out
}

/// `x` mod 2^255 - 19, fully reduced: the limbs above 2^256 folded in as 38 until four are left,
/// then p subtracted while the value is at least p.
fn modp(x: &[u64]) -> [u64; 4] {
    let mut x: Big = x.to_vec();
    while x.len() > 4 && x[4..].iter().any(|&w| w != 0) {
        let hi = mul_big(&x[4..], &[38]);
        x = add_big(&x[..4], &hi);
    }
    let mut v = [x[0], x[1], x[2], x[3]];
    let ge = |v: &[u64; 4]| {
        (0..4)
            .rev()
            .find(|&i| v[i] != P25519[i])
            .is_none_or(|i| v[i] > P25519[i])
    };
    while ge(&v) {
        let mut borrow = 0u128;
        for i in 0..4 {
            let d = (v[i] as u128).wrapping_sub(P25519[i] as u128 + borrow);
            v[i] = d as u64;
            borrow = (d >> 127) & 1;
        }
    }
    v
}

/// `p - y` for `y` below p.
fn p_minus(y: [u64; 4]) -> [u64; 4] {
    let mut out = [0u64; 4];
    let mut borrow = 0u128;
    for i in 0..4 {
        let d = (P25519[i] as u128).wrapping_sub(y[i] as u128 + borrow);
        out[i] = d as u64;
        borrow = (d >> 127) & 1;
    }
    out
}

/// `(2^256 k - 1) / d`, rounded down (`k`, `d` below 2^32): a number whose product with `d` has
/// its low 256 bits just under 2^256 and `k - 1` above them.
fn just_under(k: u64, d: u64) -> [u64; 4] {
    let n: Big = vec![u64::MAX, u64::MAX, u64::MAX, u64::MAX, k - 1];
    let mut out = [0u64; 5];
    let mut rem = 0u128;
    for i in (0..5).rev() {
        let cur = (rem << 64) | n[i] as u128;
        out[i] = (cur / d as u128) as u64;
        rem = cur % d as u128;
    }
    assert_eq!(out[4], 0, "below 2^256");
    [out[0], out[1], out[2], out[3]]
}

/// Runs `(op, a, b)` cases through the step's operations (`pieces`): op 0 add, 1 sub, 2 mul,
/// 3 sqr, 4 a24, 5 freeze; writes each result (slot 2).
const PIECES_DRIVER: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
void piece_add(uint64_t *); void piece_sub(uint64_t *); void piece_mul(uint64_t *);
void piece_sqr(uint64_t *); void piece_a24(uint64_t *); void piece_freeze(uint64_t *);
int main(int argc, char **argv) {
  FILE *in = fopen(argv[1], "rb"), *out = fopen(argv[2], "wb");
  uint64_t n; fread(&n, 8, 1, in);
  for (uint64_t i = 0; i < n; i++) {
    uint64_t c[9]; fread(c, 8, 9, in);
    uint64_t s[12]; memset(s, 0, sizeof s);
    memcpy(s, c + 1, 32); memcpy(s + 4, c + 5, 32);
    switch (c[0]) {
      case 0: piece_add(s); break;
      case 1: piece_sub(s); break;
      case 2: piece_mul(s); break;
      case 3: piece_sqr(s); break;
      case 4: piece_a24(s); break;
      default: piece_freeze(s); break;
    }
    fwrite(s + 8, 8, 4, out);
  }
  fclose(out);
  return 0;
}
"#;

/// The step's operations on operands that force their rare second carries and borrows, which a
/// random ladder reaches with a probability near 2^-250: a sum just over 2^256 - 38, a
/// difference of 0 and 2^256 - 1, a product by a24 whose low half is just under 2^256 with a big
/// carry limb (the fold's second 38), and values near 2^256 for the freeze. Each result must be
/// congruent to the reference (the freeze's equal to it: fully reduced).
#[test]
fn the_step_operations_fold_every_carry() {
    if cfg!(windows) || !cfg!(target_arch = "x86_64") {
        eprintln!("note: skipping the X25519 step operations off x86-64 Linux and macOS");
        return;
    }
    #[cfg(target_arch = "x86_64")]
    let adx =
        std::arch::is_x86_feature_detected!("bmi2") && std::arch::is_x86_feature_detected!("adx");
    #[cfg(not(target_arch = "x86_64"))]
    let adx = false;
    if !adx {
        eprintln!("note: this CPU has no BMI2 and ADX; skipping the X25519 step operations");
        return;
    }
    let Some(clang) = crate::find_clang() else {
        eprintln!("note: clang not available; skipping the X25519 step operations");
        return;
    };
    let dir = std::env::temp_dir().join(format!("velt_x25519_pieces_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    struct Cleanup<'a>(&'a Path);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0);
        }
    }
    let _cleanup = Cleanup(&dir);
    let p = P25519;
    let m = u64::MAX;
    let edges: Vec<[u64; 4]> = vec![
        [0, 0, 0, 0],
        [1, 0, 0, 0],
        [38, 0, 0, 0],
        [p[0] - 1, p[1], p[2], p[3]],
        p,
        [m, m, m, 0x7fff_ffff_ffff_ffff],
        [0, 0, 0, 1 << 63],
        [m - 37, m, m, m],
        [m - 29, m, m, m],
        [m - 1, m, m, m],
        [m, m, m, m],
        just_under(100_000, 121_665),
        just_under(121_664, 121_665),
    ];
    let mut cases: Vec<(u64, [u64; 4], [u64; 4])> = vec![];
    for &a in &edges {
        for &b in &edges {
            cases.push((0, a, b));
            cases.push((1, a, b));
            cases.push((2, a, b));
        }
        cases.push((3, a, a));
        cases.push((4, a, a));
        cases.push((5, a, a));
    }
    let mut path = dir.join("cases.bin");
    let mut bytes = (cases.len() as u64).to_le_bytes().to_vec();
    for (op, a, b) in &cases {
        bytes.extend(op.to_le_bytes());
        for w in a.iter().chain(b) {
            bytes.extend(w.to_le_bytes());
        }
    }
    std::fs::write(&path, bytes).expect("cases");
    let ll = dir.join("pieces.ll");
    std::fs::write(&ll, crate::field25519::pieces()).expect("module");
    let c = dir.join("driver.c");
    std::fs::write(&c, PIECES_DRIVER).expect("driver");
    let exe = dir.join("pieces");
    let st = Command::new(&clang)
        .args(["-O1", "-w"])
        .arg(&ll)
        .arg(&c)
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("clang");
    assert!(st.success(), "clang failed");
    let out = dir.join("out.bin");
    let st = Command::new(&exe)
        .arg(&path)
        .arg(&out)
        .status()
        .expect("run");
    assert!(st.success(), "the driver failed");
    path = out;
    let got = std::fs::read(&path).expect("results");
    for (i, (op, a, b)) in cases.iter().enumerate() {
        let r: [u64; 4] = std::array::from_fn(|k| {
            u64::from_le_bytes(got[32 * i + 8 * k..32 * i + 8 * k + 8].try_into().unwrap())
        });
        let want = match op {
            0 => modp(&add_big(a, b)),
            1 => modp(&add_big(&modp(a), &p_minus(modp(b)))),
            2 | 3 => modp(&mul_big(a, b)),
            4 => modp(&mul_big(a, &[121_665])),
            _ => modp(a),
        };
        match op {
            5 => assert_eq!(r, want, "freeze of {a:x?}"),
            _ => assert_eq!(modp(&r), want, "op {op} of {a:x?} and {b:x?}: {r:x?}"),
        }
    }
}

/// The ladder's final freeze subtracts p by a mask behind a value barrier, never by a `select`
/// on whether the reduced coordinate reached p, which is a secret: in the helper's IR as made,
/// and after clang's optimizer, no `select` takes the freeze's comparison (`%xge`, `%zge`) as
/// its condition.
#[test]
fn the_freeze_chooses_by_a_mask() {
    let (_, defs, _, _) = crate::field::helper(
        "velt_field_x25519_ladder",
        true,
        false,
        crate::field_portable::Choice::Mask,
    )
    .expect("a helper");
    let text = defs.join("\n");
    let on_ge = |ir: &str| -> Vec<String> {
        ir.lines()
            .map(str::trim)
            .filter(|l| {
                l.split_once(" = select i1 ").is_some_and(|(_, rest)| {
                    rest.split(',').next().is_some_and(|c| c.contains("ge"))
                })
            })
            .map(str::to_string)
            .collect()
    };
    assert!(on_ge(&text).is_empty(), "{:?}", on_ge(&text));
    if !cfg!(target_arch = "x86_64") || cfg!(windows) {
        return;
    }
    let Some(clang) = crate::find_clang() else {
        eprintln!("note: clang not available; skipping the optimized freeze");
        return;
    };
    let dir = std::env::temp_dir().join(format!("velt_freeze_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let ll = dir.join("f.ll");
    let out = dir.join("f.opt.ll");
    let module = format!(
        "{text}\ndefine i8 @ladder(ptr %r, i64 %d, i64 %u, i64 %k) {{\n  %v = call i1 @velt.field.x25519_ladder(ptr %r, i64 %d, i64 %u, i64 %k)\n  %w = zext i1 %v to i8\n  ret i8 %w\n}}\n"
    );
    std::fs::write(&ll, module).expect("write .ll");
    let st = Command::new(&clang)
        .args(["-O2", "-S", "-emit-llvm", "-w", "-x", "ir"])
        .arg(&ll)
        .arg("-o")
        .arg(&out)
        .status()
        .expect("clang");
    assert!(st.success(), "clang failed");
    let optimized = std::fs::read_to_string(&out).expect("optimized IR");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        optimized.contains("x25519_ladder"),
        "the helper is in the optimized IR"
    );
    let found = on_ge(&optimized);
    assert!(
        found.is_empty(),
        "the freeze selects on the secret:\n{}",
        found.join("\n")
    );
}
