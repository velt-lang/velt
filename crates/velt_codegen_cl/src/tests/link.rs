//! End-to-end check of the *object* path on the host: emit an object, link it with `rustc` against a
//! tiny Rust harness that provides `main` and the rt functions, run it and compare stdout.
//! Skipped (with a note) when `rustc` cannot be run.

use std::path::{Path, PathBuf};

use super::programs;
use super::{copy_local, finish_main, float, int, main_fb, FuncBuilder, Out, ProgramBuilder};
use crate::{emit_object, host_triple, CodegenOptions};

const HARNESS: &str = r#"
use std::io::Write;
#[repr(C)] pub struct VeltStr { ptr: *const u8, len: u64, cap: u64 }
extern "C" { fn velt_main() -> i32; }
unsafe fn bytes<'a>(s: *const VeltStr) -> &'a [u8] {
    let s = &*s;
    let len = s.len as u32 as usize; // the low half; the high half is the UTF-16 length
    if len == 0 { &[] } else { std::slice::from_raw_parts(s.ptr, len) }
}
fn out(b: &[u8]) { std::io::stdout().write_all(b).unwrap(); }
#[no_mangle] pub extern "C" fn velt_rt_write_str(_s: u32, v: *const VeltStr) { out(unsafe { bytes(v) }) }
#[no_mangle] pub extern "C" fn velt_rt_write_i64(_s: u32, v: i64) { out(v.to_string().as_bytes()) }
#[no_mangle] pub extern "C" fn velt_rt_write_u64(_s: u32, v: u64) { out(v.to_string().as_bytes()) }
#[no_mangle] pub extern "C" fn velt_rt_write_f64(_s: u32, v: f64) { out(v.to_string().as_bytes()) }
#[no_mangle] pub extern "C" fn velt_rt_write_bool(_s: u32, v: u8) { out(if v == 1 { b"true" } else { b"false" }) }
#[no_mangle] pub extern "C" fn velt_rt_write_byte(_s: u32, v: u8) { out(&[v]) }
#[no_mangle] pub extern "C" fn velt_rt_flush() {
    std::io::stdout().flush().unwrap();
    // Stack walking must get through the generated frames back into `main`
    // (needs unwind info: .pdata/.xdata on Windows, .eh_frame on ELF and Mach-O).
    let bt = std::backtrace::Backtrace::force_capture().to_string();
    if !bt.contains("harness::main") {
        eprintln!("backtrace does not reach main:\n{bt}");
        std::process::exit(99);
    }
}
#[no_mangle] pub extern "C" fn velt_rt_panic(m: *const VeltStr) -> ! {
    eprintln!("panic: {}", String::from_utf8_lossy(unsafe { bytes(m) })); std::process::exit(101)
}
#[no_mangle] pub extern "C" fn velt_rt_exit(c: i32) -> ! { std::process::exit(c) }
#[no_mangle] pub extern "C" fn velt_rt_alloc(_s: u64, _a: u64) -> *mut u8 { std::ptr::null_mut() }
#[no_mangle] pub extern "C" fn velt_rt_free(_p: *mut u8, _s: u64, _a: u64) {}
#[no_mangle] pub extern "C" fn velt_rt_str_from_i64(_v: i64, _o: *mut VeltStr) {}
#[no_mangle] pub extern "C" fn velt_rt_str_concat(_a: *const VeltStr, _b: *const VeltStr, _o: *mut VeltStr) {}
#[no_mangle] pub extern "C" fn velt_rt_str_drop(_s: *mut VeltStr) {}
#[no_mangle] pub extern "C" fn velt_rt_str_cmp(a: *const VeltStr, b: *const VeltStr) -> i32 {
    unsafe { bytes(a).cmp(bytes(b)) as i32 }
}
// Defined here for programs that import them (`imported_functions_are_called_in_another_object`).
#[no_mangle] pub extern "C" fn velt_test_imported(a: i64, b: u8, c: f64) -> i64 { a * 100 + b as i64 * 10 + c as i64 }
#[no_mangle] pub extern "C" fn velt_test_kept(x: i64) -> i64 { x + 1 }
#[no_mangle] pub extern "C" fn velt_rt_pow_i64(a: i64, b: i64) -> i64 { a.wrapping_pow(b as u32) }
// The backend emits these as instructions; a call reaching them would print NaN.
#[no_mangle] pub extern "C" fn velt_rt_math_sqrt(_x: f64) -> f64 { f64::NAN }
#[no_mangle] pub extern "C" fn velt_rt_math_floor(_x: f64) -> f64 { f64::NAN }
#[no_mangle] pub extern "C" fn velt_rt_math_ceil(_x: f64) -> f64 { f64::NAN }
#[no_mangle] pub extern "C" fn velt_rt_math_trunc(_x: f64) -> f64 { f64::NAN }
#[no_mangle] pub extern "C" fn velt_rt_math_fabs(_x: f64) -> f64 { f64::NAN }
fn main() {
    let rc = unsafe { velt_main() };
    std::io::stdout().flush().unwrap();
    std::process::exit(rc);
}
"#;

fn rustc() -> Option<String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    command(&rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| rustc)
}

/// A directory of this test process for test `name` (tests run in parallel).
fn tmp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("velt_codegen_link_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn build_and_run(
    rustc: &str,
    dir: &Path,
    tp: &programs::TestProgram,
    optimize: bool,
) -> (i32, String) {
    let obj_ext = if cfg!(windows) { "obj" } else { "o" };
    let tag = format!("{}_{}", tp.name, if optimize { "opt" } else { "debug" });
    let obj = dir.join(format!("{tag}.{obj_ext}"));
    let exe = dir.join(format!("{tag}{}", std::env::consts::EXE_SUFFIX));
    let harness = dir.join("harness.rs");
    std::fs::write(&harness, HARNESS).unwrap();
    let bytes = emit_object(
        &tp.program,
        &CodegenOptions {
            target: host_triple(),
            optimize,
            cpu: None,
        },
    )
    .unwrap();
    std::fs::write(&obj, bytes).unwrap();
    let mut cmd = command(rustc);
    // `--target`: the test may run as another arch than `rustc` (x86_64 under Rosetta).
    cmd.args(["--edition", "2021", "-O", "-g", "--crate-name", "harness"])
        .args(["--target", &host_triple()])
        .arg(&harness)
        .arg("-o")
        .arg(&exe)
        .arg(format!("-Clink-arg={}", obj.display()));
    if cfg!(target_os = "linux") {
        // Link args come after rustc's libraries, so libm must follow the object for `fmod`
        // (glibc on aarch64 has it only in libm; static musl has it in libc, already scanned).
        cmd.arg("-Clink-arg=-lm");
        if cfg!(target_env = "musl") {
            cmd.arg("-Clink-arg=-lc");
        }
    }
    let o = cmd.output().unwrap();
    assert!(
        o.status.success(),
        "{tag}: linking failed:\n{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let r = command(&exe).output().unwrap();
    (
        r.status.code().unwrap_or(-1),
        String::from_utf8(r.stdout).unwrap(),
    )
}

#[test]
fn link_and_run_host_objects() {
    let Some(rustc) = rustc() else {
        eprintln!("note: rustc not available; skipping native link test");
        return;
    };
    let dir = tmp_dir("programs");
    for tp in programs::all() {
        for optimize in [false, true] {
            let (rc, stdout) = build_and_run(&rustc, &dir, &tp, optimize);
            assert_eq!(stdout, tp.stdout, "{} optimize={optimize}", tp.name);
            assert_eq!(rc, tp.exit, "{} optimize={optimize}", tp.name);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `Linkage::Import` function (vir.rs invariant 11) is declared, never defined: calls reach
/// the definition in another object of the link (here the harness), with or without a body kept
/// for inlining (emitting it would define the symbol twice, or return 0).
#[test]
fn imported_functions_are_called_in_another_object() {
    let Some(rustc) = rustc() else {
        eprintln!("note: rustc not available; skipping native link test");
        return;
    };
    use velt_vir::vir::Ty::*;
    use velt_vir::vir::*;
    let (mut pb, rt) = ProgramBuilder::new();
    let imported = pb.add(Function::new(
        "velt_test_imported".into(),
        vec![I64, U8, F64],
        I64,
        Linkage::Import,
    ));
    let mut kept = FuncBuilder::new("velt_test_kept", &[I64], I64, Linkage::Import);
    let b = kept.block();
    kept.term(b, Terminator::Return(int(0, I64)));
    let kept = pb.add(kept.finish());
    let (mut fb, b0) = main_fb();
    let r = fb.local(I64);
    let mut o = Out {
        fb: &mut fb,
        rt: &rt,
        cur: b0,
    };
    let args = vec![int(4, I64), int(2, U8), float(7.5, F64)];
    o.cur = (o.fb).call(o.cur, Callee::Func(imported), args, Some(Place::local(r)));
    o.line(copy_local(r), I64);
    let args = vec![int(41, I64)];
    o.cur = (o.fb).call(o.cur, Callee::Func(kept), args, Some(Place::local(r)));
    o.line(copy_local(r), I64);
    let cur = o.cur;
    pb.add(finish_main(fb, cur, 0));
    let tp = programs::TestProgram {
        name: "imported",
        program: pb.p,
        stdout: "427\n42\n".into(),
        exit: 0,
    };
    let dir = tmp_dir("imported");
    for optimize in [false, true] {
        let (rc, stdout) = build_and_run(&rustc, &dir, &tp, optimize);
        assert_eq!(
            (rc, stdout.as_str()),
            (0, "427\n42\n"),
            "optimize={optimize}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[path = "../../../../tests/common/command.rs"]
mod command;
use command::command;
