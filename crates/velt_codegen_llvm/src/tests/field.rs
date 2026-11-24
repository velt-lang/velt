//! The field operations' helpers (`field.rs`) compiled with clang for the host and run on edge
//! values and pseudo-random elements, against velt_vir's `FieldOp::eval`, limb for limb: on
//! x86-64 both assembly forms (the `mul` one always, the BMI2/ADX one where the CPU has them,
//! chosen by setting the helper's CPU byte), elsewhere the plain IR. Each operation also runs in
//! place (the result register one of its operands). Skipped (with a note) without clang.

use std::path::Path;
use std::process::Command;

use velt_vir::field::{double_p256, FieldOp, P256};

use crate::field_portable::Choice;

/// Edge values and pseudo-random elements below p.
fn elements() -> Vec<[u64; 4]> {
    let pm = |k: u64| [P256[0] - k, P256[1], P256[2], P256[3]];
    let mut out = vec![
        [0; 4],
        [1, 0, 0, 0],
        [2, 0, 0, 0],
        pm(1),
        pm(2),
        [u64::MAX, u64::MAX, u64::MAX, 0x7fff_ffff_ffff_ffff],
        [0, 0, 0, 0x8000_0000_0000_0000],
        [0, 0xffff_ffff_0000_0000, u64::MAX, 0xffff_ffff_0000_0000],
        [u64::MAX, 0xffff_ffff, 0, 0xffff_fffe_ffff_ffff],
    ];
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    while out.len() < 400 {
        let e = [next(), next(), next(), next()];
        // Below p: the top limb below p's, or equal with something smaller below (rare: skip).
        if e[3] < P256[3] {
            out.push(e);
        }
    }
    out
}

/// The module: the helpers, a setter of the CPU byte (x86-64) and one exported function per
/// operation calling its helper on an array header.
fn module(x86: bool, how: Choice) -> String {
    let mut m = String::new();
    let mut seen = std::collections::BTreeSet::new();
    for op in FieldOp::ALL {
        let (name, defs, _, _) =
            crate::field::helper(op.symbol(), x86, false, how).expect("a helper");
        for d in defs {
            if seen.insert(d.clone()) {
                m.push_str(&d);
                m.push('\n');
            }
        }
        if op == FieldOp::P256Double {
            m.push_str(&format!(
                "define i8 @{}(ptr %r, i64 %d, i64 %p) {{\n  %v = call i1 {name}(ptr %r, i64 %d, i64 %p)\n  %w = zext i1 %v to i8\n  ret i8 %w\n}}\n",
                op.symbol()
            ));
            continue;
        }
        let (params, args) = match op.operands() {
            1 => ("ptr %r, i64 %d, i64 %a", "ptr %r, i64 %d, i64 %a"),
            _ => (
                "ptr %r, i64 %d, i64 %a, i64 %b",
                "ptr %r, i64 %d, i64 %a, i64 %b",
            ),
        };
        m.push_str(&format!(
            "define void @{}({params}) {{\n  call void {name}({args})\n  ret void\n}}\n",
            op.symbol()
        ));
    }
    if x86 {
        m.push_str("define void @set_cpu(i8 %v) {\n  %y = icmp eq i8 %v, 2\n  %w = select i1 %y, i64 16777728, i64 0\n  store i64 %w, ptr @velt_cpu_word, align 8\n  ret void\n}\n");
    } else {
        m.push_str("define void @set_cpu(i8 %v) {\n  ret void\n}\n");
    }
    m
}

/// Reads elements (4 limbs each) from argv[1]; for each CPU form argv[3..] names (1: `mul`, 2:
/// ADX) and each operation, writes the result of every pair, then of every element in place, to
/// argv[2].
const DRIVER: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
typedef struct { uint64_t *data; uint64_t len, cap; } arr;
void set_cpu(uint8_t);
void velt_field_p256_mul(arr *, uint64_t, uint64_t, uint64_t);
void velt_field_p256_sqr(arr *, uint64_t, uint64_t);
void velt_field_p256_add(arr *, uint64_t, uint64_t, uint64_t);
void velt_field_p256_sub(arr *, uint64_t, uint64_t, uint64_t);
uint8_t velt_field_p256_double(arr *, uint64_t, uint64_t);
int main(int argc, char **argv) {
  FILE *in = fopen(argv[1], "rb"), *out = fopen(argv[2], "wb");
  uint64_t n; fread(&n, 8, 1, in);
  uint64_t *e = malloc(32 * n); fread(e, 32, n, in);
  uint64_t regs[24]; arr r = { regs, 24, 24 };
  for (int f = 3; f < argc; f++) {
    set_cpu((uint8_t)atoi(argv[f]));
    for (int op = 0; op < 4; op++) {
      for (uint64_t i = 0; i < n; i++) {
        for (uint64_t j = 0; j < n; j += 7) {
          memcpy(regs, e + 4 * i, 32); memcpy(regs + 4, e + 4 * j, 32);
          switch (op) {
            case 0: velt_field_p256_mul(&r, 2, 0, 1); break;
            case 1: velt_field_p256_sqr(&r, 2, 0); break;
            case 2: velt_field_p256_add(&r, 2, 0, 1); break;
            default: velt_field_p256_sub(&r, 2, 0, 1); break;
          }
          fwrite(regs + 8, 8, 4, out);
        }
        // In place: register 0 = op(0, 0).
        memcpy(regs, e + 4 * i, 32);
        switch (op) {
          case 0: velt_field_p256_mul(&r, 0, 0, 0); break;
          case 1: velt_field_p256_sqr(&r, 0, 0); break;
          case 2: velt_field_p256_add(&r, 0, 0, 0); break;
          default: velt_field_p256_sub(&r, 0, 0, 0); break;
        }
        fwrite(regs, 8, 4, out);
      }
    }
    // Doublings of element triples: into registers 3..5, then in place; the flag first.
    for (uint64_t i = 0; i + 2 < n; i += 3) {
      memcpy(regs, e + 4 * i, 96);
      uint64_t done = velt_field_p256_double(&r, 3, 0);
      fwrite(&done, 8, 1, out); fwrite(regs + 12, 8, 12, out);
      done = velt_field_p256_double(&r, 0, 0);
      fwrite(&done, 8, 1, out); fwrite(regs, 8, 12, out);
    }
  }
  fclose(out);
  return 0;
}
"#;

const OPS: [FieldOp; 4] = [
    FieldOp::P256Mul,
    FieldOp::P256Sqr,
    FieldOp::P256Add,
    FieldOp::P256Sub,
];

#[test]
fn field_helpers_match_the_reference() {
    if cfg!(windows) {
        eprintln!("note: skipping the field helper run on Windows");
        return;
    }
    let Some(clang) = crate::find_clang() else {
        eprintln!("note: clang not available; skipping the field helper run");
        return;
    };
    let dir = std::env::temp_dir().join(format!("velt_field_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    struct Cleanup<'a>(&'a Path);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0);
        }
    }
    let _cleanup = Cleanup(&dir);
    // The plain IR runs on every host with each way of choosing the reduced result it has: the
    // masks behind the value barrier, the `select` wasm32 keeps, and on arm64 the `csel`s in
    // assembly; x86-64 also runs both assembly forms.
    run(&clang, &dir, false, Choice::Mask, &["1"]);
    run(&clang, &dir, false, Choice::Select, &["1"]);
    if cfg!(target_arch = "aarch64") {
        run(&clang, &dir, false, Choice::Csel, &["1"]);
    }
    #[cfg(target_arch = "x86_64")]
    {
        let mut forms = vec!["1"];
        if std::arch::is_x86_feature_detected!("bmi2") && std::arch::is_x86_feature_detected!("adx")
        {
            forms.push("2");
        }
        run(&clang, &dir, true, Choice::Mask, &forms);
    }
}

/// Compiles the helpers (the assembly forms when `x86`; the portable form choosing `how`), runs
/// `forms` and compares.
fn run(clang: &Path, dir: &Path, x86: bool, how: Choice, forms: &[&str]) {
    let (ll, c, exe) = (dir.join("f.ll"), dir.join("d.c"), dir.join("f.exe"));
    std::fs::write(&ll, module(x86, how)).expect("write .ll");
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
    let es = elements();
    let mut bytes = (es.len() as u64).to_le_bytes().to_vec();
    for e in &es {
        for w in e {
            bytes.extend(w.to_le_bytes());
        }
    }
    let (input, output) = (dir.join("in.bin"), dir.join("out.bin"));
    std::fs::write(&input, bytes).expect("write inputs");
    let status = Command::new(&exe)
        .arg(&input)
        .arg(&output)
        .args(forms)
        .status()
        .expect("run");
    assert!(status.success(), "driver failed");
    let got = std::fs::read(&output).expect("read results");
    let mut got = got
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| u64::from_le_bytes(*c));
    let mut next = || got.next().expect("a result");
    let mut bad = vec![];
    for form in forms {
        for op in OPS {
            for a in &es {
                for b in es.iter().step_by(7) {
                    let (want, have) = (op.eval(*a, *b), std::array::from_fn(|_| next()));
                    if want != have && bad.len() < 20 {
                        bad.push(format!("x86 {x86} form {form} {op:?}({a:x?}, {b:x?}): {have:x?}, want {want:x?}"));
                    }
                }
                let (want, have) = (op.eval(*a, *a), std::array::from_fn(|_| next()));
                if want != have && bad.len() < 20 {
                    bad.push(format!(
                        "form {form} {op:?} in place ({a:x?}): {have:x?}, want {want:x?}"
                    ));
                }
            }
        }
        // The fused doubling: done only by the ADX form, then as the reference computes it.
        let fused = x86 && *form == "2";
        for chunk in es.as_chunks::<3>().0 {
            for in_place in [false, true] {
                let done = next() & 0xff;
                let have: Vec<[u64; 4]> = (0..3).map(|_| std::array::from_fn(|_| next())).collect();
                if (done != 0) != fused && bad.len() < 20 {
                    bad.push(format!(
                        "x86 {x86} form {form} double: done {done}, want {fused}"
                    ));
                }
                if fused {
                    let want = double_p256(chunk[0], chunk[1], chunk[2]);
                    if have != want && bad.len() < 20 {
                        bad.push(format!(
                            "form {form} double (in place {in_place}) {chunk:x?}: {have:x?}, want {want:x?}"
                        ));
                    }
                }
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The fast form's check is the CPU feature word's BMI2 and ADX bits (so `VELT_CPU_DISABLE`
/// turns it off), or nothing where `--cpu` guarantees both; never a CPUID of its own.
#[test]
fn the_fast_form_is_chosen_by_the_cpu_feature_word() {
    let mask = velt_vir::simd::Ext::AdxX86.mask();
    assert_eq!(mask, (1 << 24) | (1 << 9), "ADX's bit and BMI2's");
    for op in FieldOp::ALL {
        let (_, defs, _, _) =
            crate::field::helper(op.symbol(), true, false, Choice::Mask).expect("a helper");
        let text = defs.join("\n");
        assert!(!text.contains("cpuid"), "{op:?} asks CPUID itself");
        if matches!(op, FieldOp::P256Add | FieldOp::P256Sub) {
            continue;
        }
        assert!(text.contains("load i64, ptr @velt_cpu_word"), "{op:?}");
        assert!(text.contains(&format!("and i64 %w, {mask}")), "{op:?}");
        assert!(
            defs.iter().any(|d| d == crate::simd::WORD),
            "{op:?} defines the word"
        );
        let (_, known, _, _) =
            crate::field::helper(op.symbol(), true, true, Choice::Mask).expect("a helper");
        let known = known.join("\n");
        assert!(
            !known.contains("@velt_cpu_word"),
            "{op:?} reads the word under `--cpu`"
        );
        assert!(known.contains("define internal i1 @velt.field.has_adx() alwaysinline nounwind {\nentry:\n  ret i1 true"));
    }
}

/// arm64's portable form reduces in assembly, choosing by `csel`s on the borrow of p's
/// subtraction ([`Choice::Csel`], `field_portable::reduce_csel`): after clang's optimizer no
/// `select` is left in the module (the `p256_masks` test refuses one whose condition comes from
/// the secret, and the backend may make a `select` a branch), and the code has no conditional
/// branch and, in assembly, a `csel` per limb of each of `add`, `sub`, `mul` and `sqr`. Run on
/// arm64 by the reference test above.
#[test]
fn the_arm64_choice_is_csels_in_assembly() {
    let Some(clang) = crate::find_clang() else {
        eprintln!("note: clang not available; skipping");
        return;
    };
    let target = "--target=aarch64-unknown-linux-gnu";
    let compile = |emit: &[&str]| -> Option<String> {
        let dir = std::env::temp_dir().join(format!("velt_csel_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let ll = dir.join("f.ll");
        std::fs::write(&ll, module(false, Choice::Csel)).expect("write .ll");
        let out = Command::new(&clang)
            .args(["-O2", "-w", target, "-S", "-o", "-"])
            .args(emit)
            .arg(&ll)
            .output()
            .expect("run clang");
        let _ = std::fs::remove_dir_all(&dir);
        let err = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() && err.contains("unable to create target") {
            return None;
        }
        assert!(out.status.success(), "clang failed:\n{err}");
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let Some(ir) = compile(&["-emit-llvm"]) else {
        eprintln!("note: this clang has no aarch64; skipping");
        return;
    };
    let selects: Vec<&str> = ir.lines().filter(|l| l.contains(" select ")).collect();
    assert!(selects.is_empty(), "{selects:#?}");
    let asm = compile(&[]).expect("aarch64 again");
    let code: Vec<&str> = asm
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('.') && !l.starts_with("//") && !l.ends_with(':'))
        .collect();
    let branches: Vec<&&str> = code
        .iter()
        .filter(|l| {
            let op = l.split_whitespace().next().unwrap_or("");
            op.starts_with("b.") || ["cbz", "cbnz", "tbz", "tbnz"].contains(&op)
        })
        .collect();
    assert!(branches.is_empty(), "{branches:#?}");
    // The `csel`s of the asm blocks (the compiler may make its own from other comparisons).
    let mut inside = false;
    let mut csels = 0;
    for l in asm.lines().map(str::trim) {
        match l {
            "//APP" => inside = true,
            "//NO_APP" => inside = false,
            _ if inside && l.starts_with("csel") => csels += 1,
            _ => {}
        }
    }
    assert_eq!(csels, 4 * 4, "{asm}");
}
