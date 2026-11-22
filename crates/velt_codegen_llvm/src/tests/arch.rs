//! velt/arch's instructions (#3222 step 4): each row of velt_vir's table, its helper called from
//! a function gated on the row's feature, compiles to the row's instruction, the one clang
//! compiles the row's C intrinsic to.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use velt_vir::arch::{ArchOp, Lane, Row, Val, ROWS};
use velt_vir::simd::CpuArch;

use crate::simd::{Arch, Caps};
use crate::types::{abi_ret, abi_type};

/// The target triple and backend architecture of `arch`.
fn target(arch: CpuArch) -> (&'static str, Arch) {
    match arch {
        CpuArch::X86_64 => ("x86_64-unknown-linux-gnu", Arch::Sse2),
        CpuArch::Aarch64 => ("aarch64-unknown-linux-gnu", Arch::Neon),
    }
}

/// The module of a function gated on the row's feature that calls the row's helper.
fn gated_module(op: ArchOp, arch: Arch) -> String {
    let (name, decls, params, ret) =
        crate::simd::helper(&op.symbol(), arch, Caps::NONE).expect("ICE: a row has a helper");
    let ps: Vec<String> = params.iter().map(|p| abi_type(*p).unwrap()).collect();
    let formals: Vec<String> = (ps.iter().enumerate())
        .map(|(i, t)| format!("{t} %p{i}"))
        .collect();
    // `ret` takes the plain type (`i16`), the signature its ABI form (`zeroext i16`).
    let plain = crate::types::llvm_type(ret).unwrap_or("void");
    let ret = abi_ret(ret).unwrap();
    // A wide result is stored through the last argument: the helper returns nothing.
    let (call, give) = match ret.as_str() {
        "void" => (String::new(), "ret void".to_string()),
        _ => ("%v = ".to_string(), format!("ret {plain} %v")),
    };
    let mut m = decls.join("\n");
    m.push_str(&format!(
        "\ndefine {ret} @t({}) #0 {{\n  {call}call {ret} {name}({})\n  {give}\n}}\n\
         attributes #0 = {{ \"target-features\"=\"{}\" }}\n",
        formals.join(", "),
        formals.join(", "),
        op.ext().gated_features()
    ));
    m
}

/// The C type of `v` on `arch`.
fn c_type(v: Val, arch: CpuArch) -> &'static str {
    match (v, arch) {
        (Val::U8, _) => "uint8_t",
        (Val::U16, _) => "uint16_t",
        (Val::U32, _) => "uint32_t",
        (Val::U64, _) => "uint64_t",
        (Val::V256(Lane::F32), _) => "__m256",
        (Val::V256(Lane::F64), _) => "__m256d",
        (Val::V256(_), _) => "__m256i",
        (Val::V512(Lane::F32), _) => "__m512",
        (Val::V512(Lane::F64), _) => "__m512d",
        (Val::V512(_), _) => "__m512i",
        (_, CpuArch::X86_64) => "__m128i",
        (Val::U8x16, _) => "uint8x16_t",
        (Val::I8x16, _) => "int8x16_t",
        (Val::U32x4, _) => "uint32x4_t",
        (Val::I32x4, _) => "int32x4_t",
        (Val::U64x2, _) => "poly64x2_t",
    }
}

/// A C function returning the row's C intrinsic of its parameters.
fn c_function(r: &Row) -> String {
    let arch = r.ext.arch();
    let header = match arch {
        CpuArch::X86_64 => "#include <immintrin.h>\n",
        CpuArch::Aarch64 => "#include <arm_acle.h>\n#include <arm_neon.h>\n",
    };
    // `vmull_p64` takes the 64-bit polynomials themselves and gives a `poly128_t`.
    let (param, ret) = match r.c {
        "vmull_p64" => ("poly64_t", "poly128_t"),
        "vmull_high_p64" => ("poly64x2_t", "poly128_t"),
        _ => ("", c_type(r.ret, arch)),
    };
    let formals: Vec<String> = (r.params.iter().enumerate())
        .map(|(i, (_, v))| {
            let t = if param.is_empty() {
                c_type(*v, arch)
            } else {
                param
            };
            format!("{t} a{i}")
        })
        .collect();
    let mut args: Vec<String> = (0..r.params.len()).map(|i| format!("a{i}")).collect();
    // Intel's `permutexvar` takes the indices first; the method takes the value first.
    if r.swapped() {
        args.swap(0, 1);
    }
    format!(
        "#include <stdint.h>\n{header}{ret} t({}) {{ return {}({}); }}\n",
        formals.join(", "),
        r.c,
        args.join(", ")
    )
}

/// The assembly clang makes of `source` (`-x ir` or `-x c`) for `triple` with `features`.
fn assembly(clang: &Path, lang: &str, source: &str, triple: &str, features: &str) -> String {
    let mut cmd = Command::new(clang);
    cmd.args(["-S", "-O2", "-ffreestanding", "-x", lang, "-", "-o", "-"])
        .arg(format!("--target={triple}"));
    for f in features.split(',') {
        cmd.args(["-Xclang", "-target-feature", "-Xclang", f]);
    }
    let mut child = (cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()))
    .spawn()
    .expect("clang runs");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(source.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "clang -x {lang}:\n{}\n{source}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Whether `asm` has the instruction `instr` (on x86, AT&T's operand-size suffix aside; a `*`
/// stands for any text, as in `vfmadd*ps`, and `|` separates equivalent forms LLVM may choose,
/// as in `vpermd|vpermps`) on a `register` (`ymm`, `zmm`; any where `None`).
fn has_instruction(asm: &str, instr: &str, arch: CpuArch, register: Option<&str>) -> bool {
    let one = |m: &str, form: &str| match form.split_once('*') {
        Some((head, tail)) => {
            m.len() >= head.len() + tail.len() && m.starts_with(head) && m.ends_with(tail)
        }
        None => {
            m == form
                || (arch == CpuArch::X86_64
                    && m.len() == form.len() + 1
                    && m.starts_with(form)
                    && m.ends_with(['b', 'w', 'l', 'q']))
        }
    };
    let named = |m: &str| instr.split('|').any(|form| one(m, form));
    asm.lines().any(|line| {
        let m = line.split_whitespace().next().unwrap_or("");
        named(m) && register.is_none_or(|r| line.contains(r))
    })
}

#[test]
fn every_row_is_the_instruction_clang_makes_of_its_c_intrinsic() {
    let Some(clang) = crate::find_clang() else {
        eprintln!("note: clang not available; skipping velt/arch's instructions");
        return;
    };
    let mut bad = vec![];
    for (i, r) in ROWS.iter().enumerate() {
        let op = ArchOp(i as u16);
        let (triple, arch) = target(r.ext.arch());
        let features = r.ext.gated_features();
        let ours = assembly(&clang, "ir", &gated_module(op, arch), triple, features);
        let theirs = assembly(&clang, "c", &c_function(r), triple, features);
        for (who, asm) in [("velt", &ours), ("clang", &theirs)] {
            // A 256-bit row is one `ymm` instruction and a 512-bit one a `zmm` one, not
            // split into narrower halves.
            let register = match op.width() {
                256 => Some("ymm"),
                512 => Some("zmm"),
                _ => None,
            };
            if !has_instruction(asm, r.instr, r.ext.arch(), register) {
                bad.push(format!(
                    "{} ({}): {who} has no `{}`:\n{asm}",
                    r.method, r.c, r.instr
                ));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The helper of a row on the other architecture is a zero (dead behind its false check), with
/// no feature.
#[test]
fn a_row_on_the_other_architecture_is_a_zero() {
    let op = ArchOp::of_symbol("velt_arch_sse42_crc32_u64").unwrap();
    let (_, decls, _, _) = crate::simd::helper(&op.symbol(), Arch::Neon, Caps::NONE).unwrap();
    let text = decls.join("\n");
    assert!(text.contains("ret i64 0"), "{text}");
    assert!(!text.contains("target-features"), "{text}");
    let (_, decls, _, _) = crate::simd::helper(&op.symbol(), Arch::Sse2, Caps::NONE).unwrap();
    assert!(decls
        .join("\n")
        .contains("\"target-features\"=\"+sse4.2,+crc32\""));
}
