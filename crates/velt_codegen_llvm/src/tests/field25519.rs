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
    let (name, defs, _, _) =
        crate::field::helper("velt_field_x25519_ladder", x86).expect("a helper");
    let mut m = defs.join("\n");
    m.push_str(&format!(
        "\ndefine i8 @ladder(ptr %r, i64 %d, i64 %u, i64 %k) {{\n  %v = call i1 {name}(ptr %r, i64 %d, i64 %u, i64 %k)\n  %w = zext i1 %v to i8\n  ret i8 %w\n}}\n"
    ));
    m.push_str(match x86 {
        true => "define void @set_cpu(i8 %v) {\n  store i8 %v, ptr @velt.field.cpu, align 1\n  ret void\n}\n",
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
