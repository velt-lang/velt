//! velt/arch's instructions (#3222 step 4) compute what their manuals say: a release program
//! calls every instruction of this machine's architecture behind its class's check, and each
//! answer is compared with a reference written from the instruction's description (Intel's
//! guide, Arm's ACLE). Where the CPU lacks a feature the program prints `-` for its rows, which
//! must agree with what Rust detects. The LLVM test (`velt_codegen_llvm` `tests/arch.rs`) checks
//! each row is the instruction clang makes of its C intrinsic; this one checks what it computes,
//! operand order included.

use velt_vir::arch::{Row, Val, ROWS};
use velt_vir::simd::{CpuArch, Ext};

mod no_window;
mod test_dir;

/// This machine's architecture, if velt/arch has instructions for it.
fn host() -> Option<CpuArch> {
    if cfg!(target_arch = "x86_64") {
        Some(CpuArch::X86_64)
    } else if cfg!(target_arch = "aarch64") {
        Some(CpuArch::Aarch64)
    } else {
        None
    }
}

/// Whether this CPU has `ext`, as Rust detects it.
fn detected(ext: Ext) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        match ext {
            Ext::Sse42X86 => std::arch::is_x86_feature_detected!("sse4.2"),
            Ext::PopcntX86 => std::arch::is_x86_feature_detected!("popcnt"),
            Ext::LzcntX86 => std::arch::is_x86_feature_detected!("lzcnt"),
            Ext::Bmi1X86 => std::arch::is_x86_feature_detected!("bmi1"),
            Ext::Bmi2X86 => std::arch::is_x86_feature_detected!("bmi2"),
            Ext::Ssse3X86 => std::arch::is_x86_feature_detected!("ssse3"),
            _ => panic!("ICE: no detection for {ext:?}"),
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        match ext {
            Ext::CrcArm => std::arch::is_aarch64_feature_detected!("crc"),
            Ext::ClmulArm => std::arch::is_aarch64_feature_detected!("pmull"),
            Ext::DotprodArm => std::arch::is_aarch64_feature_detected!("dotprod"),
            Ext::I8mmArm => std::arch::is_aarch64_feature_detected!("i8mm"),
            _ => panic!("ICE: no detection for {ext:?}"),
        }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = ext;
        false
    }
}

/// Argument `i` of a row whose parameter `i` has type `v`.
#[derive(Clone, Debug)]
enum Arg {
    Int(u64),
    Bytes([u8; 16]),
    Words([u32; 4]),
    Quads([u64; 2]),
}

fn arg(v: Val, i: usize) -> Arg {
    let bytes = |m: u32, a: u32| -> [u8; 16] { std::array::from_fn(|k| (k as u32 * m + a) as u8) };
    match v {
        Val::U8 => Arg::Int([0xa5, 0x3c][i % 2]),
        Val::U16 => Arg::Int([0xbeef, 0x1234][i % 2]),
        Val::U32 => Arg::Int([0xdead_beef, 0x0ff0_f00f, 0x0000_0813][i % 3]),
        Val::U64 => Arg::Int([0x0123_4567_89ab_cdef, 0xf0f0_f0f0_0ff0_0ff0][i % 2]),
        // The second byte vector has bit 7 set in some bytes (`pshufb` zeroes those).
        Val::U8x16 | Val::I8x16 => {
            Arg::Bytes([bytes(17, 3), bytes(29, 0x87), bytes(11, 200)][i % 3])
        }
        Val::U32x4 | Val::I32x4 => Arg::Words([1, 0x7fff_fff0, 0xffff_fffe, 1000]),
        Val::U64x2 => Arg::Quads(
            [
                [0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210],
                [0x0f0f_1234_5678_9abc, 0x8000_0000_0000_0003],
            ][i % 2],
        ),
    }
}

/// `a` written as a Velt expression of type `v`.
fn velt_arg(a: &Arg, v: Val) -> String {
    let list = |xs: Vec<String>| xs.join(", ");
    match a {
        Arg::Int(x) => format!("{x}"),
        Arg::Bytes(b) if v == Val::I8x16 => {
            format!(
                "i8x16.load([{}])",
                list(b.iter().map(|x| (*x as i8).to_string()).collect())
            )
        }
        Arg::Bytes(b) => format!(
            "u8x16.load([{}])",
            list(b.iter().map(|x| x.to_string()).collect())
        ),
        Arg::Words(w) if v == Val::I32x4 => {
            format!(
                "i32x4.load([{}])",
                list(w.iter().map(|x| (*x as i32).to_string()).collect())
            )
        }
        Arg::Words(w) => format!(
            "u32x4.load([{}])",
            list(w.iter().map(|x| x.to_string()).collect())
        ),
        Arg::Quads(q) => format!(
            "u64x2.load([{}])",
            list(q.iter().map(|x| x.to_string()).collect())
        ),
    }
}

/// The Velt expression printing the result `x` of type `v`.
fn velt_show(v: Val) -> &'static str {
    match v {
        Val::U8 | Val::U16 | Val::U32 | Val::U64 => "String(x)",
        Val::U8x16 => "lanes16(x)",
        Val::I8x16 => "lanes16(x.asU8x16())",
        Val::U32x4 => "lanes4(x)",
        Val::I32x4 => "lanes4(x.asU32x4())",
        Val::U64x2 => "String(x.lane(0)) + \",\" + String(x.lane(1))",
    }
}

/// The program: each row of `arch`, behind its class's check, as `method=answer` or `method=-`.
fn program(arch: CpuArch) -> String {
    let rows: Vec<&Row> = ROWS.iter().filter(|r| r.ext.arch() == arch).collect();
    let mut classes: Vec<&str> = rows.iter().map(|r| r.ext.arch_class()).collect();
    classes.sort_unstable();
    classes.dedup();
    let module = rows[0].ext.arch_module();
    let mut s = format!(
        "import {{ {} }} from \"{module}\";\nimport {{ i32x4, i8x16, u32x4, u64x2, u8x16 }} from \"velt/simd\";\n\n\
         function lanes16(v: u8x16): string {{\n  let s = \"\";\n  for (let i: usize = 0; i < 16; i++) {{\n    s += (i > 0 ? \",\" : \"\") + String(v.lane(i));\n  }}\n  return s;\n}}\n\n\
         function lanes4(v: u32x4): string {{\n  let s = \"\";\n  for (let i: usize = 0; i < 4; i++) {{\n    s += (i > 0 ? \",\" : \"\") + String(v.lane(i));\n  }}\n  return s;\n}}\n\nfunction main() {{\n",
        classes.join(", ")
    );
    for r in rows {
        let class = r.ext.arch_class();
        let args: Vec<String> = (r.params.iter().enumerate())
            .map(|(i, (_, v))| velt_arg(&arg(*v, i), *v))
            .collect();
        s.push_str(&format!(
            "  if ({class}.isSupported()) {{\n    const x = {class}.{}({});\n    console.log(\"{}=\" + {});\n  }} else {{\n    console.log(\"{}=-\");\n  }}\n",
            r.method,
            args.join(", "),
            r.method,
            velt_show(r.ret),
            r.method
        ));
    }
    s.push_str("}\n");
    s
}

fn int(a: &Arg) -> u64 {
    match a {
        Arg::Int(x) => *x,
        _ => panic!("ICE: not a scalar"),
    }
}

fn bytes(a: &Arg) -> [u8; 16] {
    match a {
        Arg::Bytes(b) => *b,
        _ => panic!("ICE: not bytes"),
    }
}

fn words(a: &Arg) -> [u32; 4] {
    match a {
        Arg::Words(w) => *w,
        _ => panic!("ICE: not words"),
    }
}

fn quads(a: &Arg) -> [u64; 2] {
    match a {
        Arg::Quads(q) => *q,
        _ => panic!("ICE: not quads"),
    }
}

/// The reflected CRC of `crc` extended by the `n` low bytes of `data`, low byte first, with
/// polynomial `poly` (reflected): no inversion, as the instructions compute it.
fn crc(poly: u32, mut crc: u32, data: u64, n: usize) -> u32 {
    for k in 0..n {
        crc ^= ((data >> (8 * k)) & 0xff) as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ poly
            } else {
                crc >> 1
            };
        }
    }
    crc
}

const CRC32C: u32 = 0x82f6_3b78;
const CRC32: u32 = 0xedb8_8320;

/// The carry-less product of `a` and `b`, as (low 64 bits, high 64 bits).
fn clmul(a: u64, b: u64) -> (u64, u64) {
    let (mut lo, mut hi) = (0u64, 0u64);
    for i in 0..64 {
        if (b >> i) & 1 != 0 {
            lo ^= a << i;
            hi ^= if i == 0 { 0 } else { a >> (64 - i) };
        }
    }
    (lo, hi)
}

fn pdep(v: u64, mask: u64) -> u64 {
    let (mut out, mut k) = (0u64, 0);
    for i in 0..64 {
        if (mask >> i) & 1 != 0 {
            out |= ((v >> k) & 1) << i;
            k += 1;
        }
    }
    out
}

fn pext(v: u64, mask: u64) -> u64 {
    let (mut out, mut k) = (0u64, 0);
    for i in 0..64 {
        if (mask >> i) & 1 != 0 {
            out |= ((v >> i) & 1) << k;
            k += 1;
        }
    }
    out
}

/// `bextr` on a `bits`-bit value.
fn bextr(v: u64, control: u64, bits: u32) -> u64 {
    let (start, len) = ((control & 0xff) as u32, ((control >> 8) & 0xff) as u32);
    if start >= bits {
        return 0;
    }
    let field = v >> start;
    if len >= bits {
        field
    } else {
        field & ((1u64 << len) - 1)
    }
}

/// `bzhi` on a `bits`-bit value.
fn bzhi(v: u64, index: u64, bits: u32) -> u64 {
    let i = (index & 0xff) as u32;
    if i >= bits {
        v
    } else {
        v & ((1u64 << i) - 1)
    }
}

fn join<T: ToString>(xs: impl IntoIterator<Item = T>) -> String {
    xs.into_iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// What row `r` computes from its arguments, as the program prints it.
fn reference(r: &Row) -> String {
    let a: Vec<Arg> = (r.params.iter().enumerate())
        .map(|(i, (_, v))| arg(*v, i))
        .collect();
    let m32 = |x: u64| x as u32 as u64;
    match r.c {
        "_mm_crc32_u8" | "__crc32cb" => crc(CRC32C, int(&a[0]) as u32, int(&a[1]), 1).to_string(),
        "_mm_crc32_u16" | "__crc32ch" => crc(CRC32C, int(&a[0]) as u32, int(&a[1]), 2).to_string(),
        "_mm_crc32_u32" | "__crc32cw" => crc(CRC32C, int(&a[0]) as u32, int(&a[1]), 4).to_string(),
        "_mm_crc32_u64" | "__crc32cd" => crc(CRC32C, int(&a[0]) as u32, int(&a[1]), 8).to_string(),
        "__crc32b" => crc(CRC32, int(&a[0]) as u32, int(&a[1]), 1).to_string(),
        "__crc32h" => crc(CRC32, int(&a[0]) as u32, int(&a[1]), 2).to_string(),
        "__crc32w" => crc(CRC32, int(&a[0]) as u32, int(&a[1]), 4).to_string(),
        "__crc32d" => crc(CRC32, int(&a[0]) as u32, int(&a[1]), 8).to_string(),
        "_mm_popcnt_u32" | "_mm_popcnt_u64" => int(&a[0]).count_ones().to_string(),
        "_lzcnt_u32" => (int(&a[0]) as u32).leading_zeros().to_string(),
        "_lzcnt_u64" => int(&a[0]).leading_zeros().to_string(),
        "_tzcnt_u32" => (int(&a[0]) as u32).trailing_zeros().to_string(),
        "_tzcnt_u64" => int(&a[0]).trailing_zeros().to_string(),
        "_andn_u32" => m32(!int(&a[0]) & int(&a[1])).to_string(),
        "_andn_u64" => (!int(&a[0]) & int(&a[1])).to_string(),
        "_blsi_u32" => m32(int(&a[0]) & int(&a[0]).wrapping_neg()).to_string(),
        "_blsi_u64" => (int(&a[0]) & int(&a[0]).wrapping_neg()).to_string(),
        "_blsr_u32" => m32(int(&a[0]) & int(&a[0]).wrapping_sub(1)).to_string(),
        "_blsr_u64" => (int(&a[0]) & int(&a[0]).wrapping_sub(1)).to_string(),
        "_blsmsk_u32" => m32(int(&a[0]) ^ m32(int(&a[0]).wrapping_sub(1))).to_string(),
        "_blsmsk_u64" => (int(&a[0]) ^ int(&a[0]).wrapping_sub(1)).to_string(),
        "_bextr2_u32" => bextr(int(&a[0]), int(&a[1]), 32).to_string(),
        "_bextr2_u64" => bextr(int(&a[0]), int(&a[1]), 64).to_string(),
        "_bzhi_u32" => bzhi(int(&a[0]), int(&a[1]), 32).to_string(),
        "_bzhi_u64" => bzhi(int(&a[0]), int(&a[1]), 64).to_string(),
        "_pdep_u32" => m32(pdep(int(&a[0]), m32(int(&a[1])))).to_string(),
        "_pdep_u64" => pdep(int(&a[0]), int(&a[1])).to_string(),
        "_pext_u32" => pext(int(&a[0]), m32(int(&a[1]))).to_string(),
        "_pext_u64" => pext(int(&a[0]), int(&a[1])).to_string(),
        "_mm_shuffle_epi8" => {
            let (v, m) = (bytes(&a[0]), bytes(&a[1]));
            join((0..16).map(|i| {
                if m[i] & 0x80 != 0 {
                    0
                } else {
                    v[(m[i] & 15) as usize]
                }
            }))
        }
        "vmull_p64" => {
            let (lo, hi) = clmul(quads(&a[0])[0], quads(&a[1])[0]);
            format!("{lo},{hi}")
        }
        "vmull_high_p64" => {
            let (lo, hi) = clmul(quads(&a[0])[1], quads(&a[1])[1]);
            format!("{lo},{hi}")
        }
        "vdotq_u32" | "vdotq_s32" | "vusdotq_s32" => {
            let (acc, x, y) = (words(&a[0]), bytes(&a[1]), bytes(&a[2]));
            let left = |k: usize| {
                if r.c == "vdotq_s32" {
                    x[k] as i8 as i64
                } else {
                    x[k] as i64
                }
            };
            let right = |k: usize| {
                if r.c == "vdotq_u32" {
                    y[k] as i64
                } else {
                    y[k] as i8 as i64
                }
            };
            join((0..4).map(|i| {
                let sum: i64 = (0..4).map(|k| left(4 * i + k) * right(4 * i + k)).sum();
                (acc[i] as i64).wrapping_add(sum) as u32
            }))
        }
        "vmmlaq_u32" | "vmmlaq_s32" => {
            let (acc, x, y) = (words(&a[0]), bytes(&a[1]), bytes(&a[2]));
            let signed = r.c == "vmmlaq_s32";
            let at = |b: [u8; 16], k: usize| {
                if signed {
                    b[k] as i8 as i64
                } else {
                    b[k] as i64
                }
            };
            join((0..4).map(|lane| {
                let (row, col) = (lane / 2, lane % 2);
                let sum: i64 = (0..8)
                    .map(|k| at(x, row * 8 + k) * at(y, col * 8 + k))
                    .sum();
                (acc[lane] as i64).wrapping_add(sum) as u32
            }))
        }
        other => panic!("no reference for `{other}`: add one"),
    }
}

#[test]
fn every_instruction_computes_what_its_manual_says() {
    let Some(arch) = host() else {
        eprintln!("note: velt/arch has no instructions for this architecture");
        return;
    };
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("main.velt");
    std::fs::write(&file, program(arch)).unwrap();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["run", "--release", file.to_str().unwrap()])
        .current_dir(dir.path())
        .env_remove("VELT_CPU_DISABLE")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}\n{stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut bad = vec![];
    for r in ROWS.iter().filter(|r| r.ext.arch() == arch) {
        let line = (stdout.lines())
            .find_map(|l| l.strip_prefix(&format!("{}=", r.method)))
            .unwrap_or_else(|| panic!("no line for {}:\n{stdout}", r.method));
        let want = if detected(r.ext) {
            reference(r)
        } else {
            "-".to_string()
        };
        if line != want {
            bad.push(format!("{} ({}): {line}, not {want}", r.method, r.c));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Every row has a reference, whichever machine runs the test.
#[test]
fn every_row_has_a_reference() {
    for r in ROWS {
        reference(r);
    }
}
