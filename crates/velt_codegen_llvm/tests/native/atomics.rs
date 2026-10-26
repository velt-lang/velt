//! The `velt_atomic_*` externs compiled for the host (`clang -O3`): every operation at every
//! width against a reference, on words whose neighbouring bytes must stay untouched, and
//! counters incremented from several threads. No symbol is defined for the externs, so a call
//! left in the code fails to link. Also: the orderings survive to arm64 assembly.

use std::fmt::Write;

use crate::common::builder::*;
use crate::harness;
use velt_vir::atomic::{word, AtomicCall, Order, Rmw};
use velt_vir::vir::Ty::*;
use velt_vir::vir::{Callee, Const, Operand, Program, Rvalue, Terminator};

/// Every operation at every width, with orderings cycling through the valid ones; the first
/// three are what the threads and the assembly check use, and entries 5 to 7 (8-bit `load`,
/// compare-and-swap and `umax`) what the threads use on bytes of one word.
fn calls() -> Vec<AtomicCall> {
    use Order::*;
    let mut calls = vec![
        AtomicCall::Rmw {
            op: Rmw::Add,
            bits: 64,
            order: Relaxed,
        },
        AtomicCall::Load {
            bits: 64,
            order: Acquire,
        },
        AtomicCall::Cas {
            bits: 64,
            order: AcqRel,
            failure: Relaxed,
            weak: true,
        },
        AtomicCall::Store {
            bits: 64,
            order: Release,
        },
        AtomicCall::Cas {
            bits: 64,
            order: AcqRel,
            failure: Acquire,
            weak: false,
        },
        AtomicCall::Load {
            bits: 8,
            order: Relaxed,
        },
        AtomicCall::Cas {
            bits: 8,
            order: SeqCst,
            failure: SeqCst,
            weak: false,
        },
        AtomicCall::Rmw {
            op: Rmw::UMax,
            bits: 8,
            order: Relaxed,
        },
    ];
    for bits in [8, 16, 32, 64] {
        for (load, store) in [(Relaxed, Relaxed), (Acquire, Release), (SeqCst, SeqCst)] {
            calls.push(AtomicCall::Load { bits, order: load });
            calls.push(AtomicCall::Store { bits, order: store });
        }
        for (i, op) in Rmw::ALL.into_iter().enumerate() {
            let order = Order::ALL[i % Order::ALL.len()];
            calls.push(AtomicCall::Rmw { op, bits, order });
        }
        for (order, failure) in [(AcqRel, Acquire), (Relaxed, Relaxed), (SeqCst, SeqCst)] {
            for weak in [false, true] {
                calls.push(AtomicCall::Cas {
                    bits,
                    order,
                    failure,
                    weak,
                });
            }
        }
    }
    calls
}

/// What `call` gives and leaves in the word, from the word `w` and the arguments `a` and `b`
/// (each reduced to the word's width).
fn reference(call: AtomicCall, w: u64, a: u64, b: u64) -> (u64, u64) {
    let bits = call.bits();
    let m = if bits == 64 {
        u64::MAX
    } else {
        (1 << bits) - 1
    };
    let (w, a, b) = (w & m, a & m, b & m);
    let signed = |x: u64| ((x << (64 - bits)) as i64) >> (64 - bits);
    match call {
        AtomicCall::Load { .. } => (w, w),
        AtomicCall::Store { .. } => (0, a),
        AtomicCall::Rmw { op, .. } => {
            let new = match op {
                Rmw::Xchg => a,
                Rmw::Add => w.wrapping_add(a),
                Rmw::Sub => w.wrapping_sub(a),
                Rmw::And => w & a,
                Rmw::Or => w | a,
                Rmw::Xor => w ^ a,
                Rmw::Max => signed(w).max(signed(a)) as u64,
                Rmw::Min => signed(w).min(signed(a)) as u64,
                Rmw::UMax => w.max(a),
                Rmw::UMin => w.min(a),
            };
            (w, new & m)
        }
        AtomicCall::Cas { weak, .. } => {
            let new = if w == a { b } else { w };
            (if weak { (w == a) as u64 } else { w }, new)
        }
        AtomicCall::Fence(_) | AtomicCall::SpinHint => (0, w),
    }
}

/// `velt_main`, per call an exported `entry_<i>(p: Ptr, a: U64, b: U64) -> U64` (the call on
/// the word at `p` with `a` and `b` cast to the word's type, its result cast to `U64`, 0 when
/// it gives nothing), and `fences()`: every fence and the spin hint.
fn program(calls: &[AtomicCall]) -> Program {
    let mut pb = ProgramBuilder::new();
    for (i, &call) in calls.iter().enumerate() {
        let (params, ret) = call.signature();
        let ext = pb.ext(&call.symbol(), &params, ret, false);
        let mut fb = FuncBuilder::export(&format!("entry_{i}"), &[Ptr, U64, U64], U64);
        let w = word(call.bits());
        let (a, b, out) = (fb.local(w), fb.local(w), fb.local(U64));
        let r = (ret != Unit).then(|| fb.local(ret));
        let bb = fb.block();
        fb.assign(bb, a, Rvalue::Cast(copy_local(fb.param(1)), w));
        fb.assign(bb, b, Rvalue::Cast(copy_local(fb.param(2)), w));
        let args = [copy_local(fb.param(0)), copy_local(a), copy_local(b)];
        let bb = fb.call(bb, Callee::Extern(ext), args[..params.len()].to_vec(), r);
        let result = match r {
            Some(r) => Rvalue::Cast(copy_local(r), U64),
            None => Rvalue::Use(int(0, U64)),
        };
        fb.assign(bb, out, result);
        fb.term(bb, Terminator::Return(copy_local(out)));
        pb.add(fb.finish());
    }
    let mut fb = FuncBuilder::export("fences", &[], Unit);
    let mut bb = fb.block();
    let hints = Order::ALL[1..]
        .iter()
        .map(|&o| AtomicCall::Fence(o))
        .chain([AtomicCall::SpinHint]);
    for call in hints {
        let ext = pb.ext(&call.symbol(), &[], Unit, false);
        bb = fb.call(bb, Callee::Extern(ext), vec![], None);
    }
    fb.ret(bb, Operand::Const(Const::Unit, Unit));
    pb.add(fb.finish());
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let b = fb.block();
    fb.ret(b, int(0, I32));
    pb.add(fb.finish());
    pb.finish()
}

/// Words and arguments: zero, small numbers, the sign bit and all ones of each width, and
/// patterns with high bits beyond the narrower widths.
const VALUES: [u64; 10] = [
    0,
    1,
    5,
    0x7F,
    0x80,
    0xFFFF,
    0x8000_0000,
    0x8000_0000_0000_0000,
    u64::MAX,
    0x0123_4567_89AB_CDEF,
];

const THREADS: u64 = 8;
const ROUNDS: u64 = 100_000;

/// Prints, per entry and inputs, `i w a b -> result cell`; then four words that `THREADS`
/// threads change `ROUNDS` times each: a counter incremented with `add`, one with a `load` /
/// weak CAS loop, and per thread its own byte of a third word incremented with an 8-bit CAS
/// loop (operands with other bits above the byte) and its own byte of a fourth raised with an
/// 8-bit `umax`: a lost update or a write to a neighbouring byte changes the totals.
fn harness_source(calls: &[AtomicCall]) -> String {
    let mut s = String::from("extern \"C\" {\n    fn fences();\n");
    for i in 0..calls.len() {
        let _ = writeln!(s, "    fn entry_{i}(p: *mut u64, a: u64, b: u64) -> u64;");
    }
    s.push_str("}\n");
    let _ = writeln!(s, "const VALUES: [u64; 10] = {VALUES:?};");
    let _ = writeln!(
        s,
        "const ENTRIES: [unsafe extern \"C\" fn(*mut u64, u64, u64) -> u64; {}] = [{}];",
        calls.len(),
        (0..calls.len())
            .map(|i| format!("entry_{i}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = write!(
        s,
        r#"
struct Cell(*mut u64);
unsafe impl Send for Cell {{}}
unsafe impl Sync for Cell {{}}
fn main() {{
    unsafe {{ fences() }};
    for (i, f) in ENTRIES.iter().enumerate() {{
        for w in VALUES {{
            for a in VALUES {{
                for b in [VALUES[1], VALUES[8]] {{
                    let mut cell = w;
                    let r = unsafe {{ f(&mut cell, a, b) }};
                    println!("{{i}} {{w}} {{a}} {{b}} -> {{r}} {{cell}}");
                }}
            }}
        }}
    }}
    let counters = Box::leak(Box::new([0u64; 4]));
    let base = counters.as_mut_ptr() as usize;
    std::thread::scope(|scope| {{
        for t in 0..{THREADS} {{
            scope.spawn(move || {{
                let cell = |i: usize| (base + 8 * i) as *mut u64;
                let byte = |i: usize| (base + 8 * i + t) as *mut u64;
                for r in 0..{ROUNDS} {{
                    unsafe {{ entry_0(cell(0), 1, 0) }};
                    let mut seen = unsafe {{ entry_1(cell(1), 0, 0) }};
                    while unsafe {{ entry_2(cell(1), seen, seen + 1) }} == 0 {{
                        seen = unsafe {{ entry_1(cell(1), 0, 0) }};
                    }}
                    let mut old = unsafe {{ entry_5(byte(2), 0, 0) }};
                    loop {{
                        let high = 0xDEAD_BEEF_0000_0000u64;
                        let got = unsafe {{ entry_6(byte(2), old | high, (old + 1) | high) }};
                        if got == old {{
                            break;
                        }}
                        old = got;
                    }}
                    unsafe {{ entry_7(byte(3), (r % 200) | 0xFF00, 0) }};
                }}
            }});
        }}
    }});
    println!("counters {{}} {{}} {{:#x}} {{:#x}}", counters[0], counters[1], counters[2], counters[3]);
}}
"#
    );
    s
}

/// The expected output of the harness.
fn expected(calls: &[AtomicCall]) -> String {
    let mut s = String::new();
    for (i, &call) in calls.iter().enumerate() {
        let bits = call.bits();
        let keep = if bits == 64 { 0 } else { u64::MAX << bits };
        for w in VALUES {
            for a in VALUES {
                for b in [VALUES[1], VALUES[8]] {
                    let (r, word) = reference(call, w, a, b);
                    let _ = writeln!(s, "{i} {w} {a} {b} -> {r} {}", (w & keep) | word);
                }
            }
        }
    }
    let n = THREADS * ROUNDS;
    let bytes = |v: u64| (0..THREADS).fold(0, |w, t| w | v << (8 * t));
    let _ = writeln!(
        s,
        "counters {n} {n} {:#x} {:#x}",
        bytes(ROUNDS % 256),
        bytes(199)
    );
    s
}

#[test]
fn atomics_match_reference_and_count_across_threads() {
    let Some((clang, rustc)) = harness::tools() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("velt_llvm_atomics_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let calls = calls();
    let ir = velt_codegen_llvm::emit_ir(&program(&calls), "").expect("emit_ir");
    let ll = dir.join("atomics.ll");
    std::fs::write(&ll, ir).expect("write .ll");
    let objects = harness::compile_all(&clang, &[ll]);
    let source = dir.join("harness.rs");
    std::fs::write(&source, harness_source(&calls)).expect("write harness");
    let output = harness::link_and_run(&rustc, &dir, &source, &objects);
    let _ = std::fs::remove_dir_all(&dir);
    let want = expected(&calls);
    let bad: Vec<String> = output
        .lines()
        .zip(want.lines())
        .filter(|(got, want)| got != want)
        .map(|(got, want)| format!("got `{got}`, want `{want}`"))
        .take(20)
        .collect();
    assert!(bad.is_empty(), "mismatches:\n{}", bad.join("\n"));
    assert_eq!(output.lines().count(), want.lines().count(), "{output}");
}

/// On arm64 (Apple, so with LSE and RCpc) the acquire load, the release store and the
/// acq_rel compare-and-swap are `ldapr`, `stlr` and `casal`, as Rust's are: the orderings reach
/// the machine code, and no call is left.
#[test]
fn orderings_reach_arm64_assembly() {
    let Some(clang) = velt_codegen_llvm::find_clang() else {
        eprintln!("note: clang not available; skipping");
        return;
    };
    let calls = calls();
    let ir = velt_codegen_llvm::emit_ir(&program(&calls[..5]), "aarch64-apple-darwin").unwrap();
    let out = std::process::Command::new(clang)
        .args(["-O3", "-S", "-x", "ir", "-Wno-override-module"])
        .args(["--target=aarch64-apple-darwin", "-mmacosx-version-min=11.0"])
        .args(["-", "-o", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write as _;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(ir.as_bytes())?;
            child.wait_with_output()
        })
        .expect("run clang");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let asm = String::from_utf8_lossy(&out.stdout);
    // The body of one exported entry: from its label to the next `.globl`.
    let body = |i: usize| -> String {
        let start = asm
            .find(&format!("_entry_{i}:"))
            .unwrap_or_else(|| panic!("no entry_{i} in\n{asm}"));
        let rest = &asm[start..];
        rest[..rest.find(".globl").unwrap_or(rest.len())].to_string()
    };
    for (i, want) in [
        (0, "ldadd\t"),
        (1, "ldapr\t"),
        (3, "stlr\t"),
        (4, "casal\t"),
    ] {
        let b = body(i);
        assert!(b.contains(want), "entry_{i} has no `{want}`:\n{b}");
        assert!(!b.contains("\tbl\t"), "entry_{i} calls:\n{b}");
    }
    assert!(body(2).contains("casal\t"), "{}", body(2));
}
