//! Native execution on the host via `cranelift-jit`, sharing `build_module` with the object path.
//! Runtime externs are mapped to small Rust functions that append to a thread-local buffer.

use std::cell::RefCell;

use cranelift_jit::{JITBuilder, JITModule};
use velt_vir::vir;

use super::programs;

thread_local! {
    static OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

#[repr(C)]
struct VeltStr {
    ptr: *const u8,
    len: u64,
    cap: u64,
}

fn out(s: &[u8]) {
    OUT.with(|o| o.borrow_mut().extend_from_slice(s));
}
unsafe fn str_bytes<'a>(s: *const VeltStr) -> &'a [u8] {
    let s = &*s;
    if s.len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(s.ptr, s.len as usize)
    }
}

extern "C" fn rt_write_str(_stream: u32, s: *const VeltStr) {
    out(unsafe { str_bytes(s) });
}
extern "C" fn rt_write_i64(_stream: u32, v: i64) {
    out(v.to_string().as_bytes());
}
extern "C" fn rt_write_u64(_stream: u32, v: u64) {
    out(v.to_string().as_bytes());
}
extern "C" fn rt_write_f64(_stream: u32, v: f64) {
    out(v.to_string().as_bytes());
}
extern "C" fn rt_write_bool(_stream: u32, v: u8) {
    assert!(v <= 1, "bool must be 0/1, got {v}");
    out(if v == 1 { b"true" } else { b"false" });
}
extern "C" fn rt_write_byte(_stream: u32, b: u8) {
    out(&[b]);
}
extern "C" fn rt_flush() {}
extern "C" fn rt_panic(s: *const VeltStr) {
    panic!(
        "velt panic: {}",
        String::from_utf8_lossy(unsafe { str_bytes(s) })
    );
}
extern "C" fn rt_exit(code: i32) {
    panic!("velt exit {code}");
}
extern "C" fn rt_alloc(_size: u64, _align: u64) -> *mut u8 {
    unimplemented!()
}
extern "C" fn rt_free(_p: *mut u8, _size: u64, _align: u64) {}
extern "C" fn rt_str_from_i64(_v: i64, _out: *mut VeltStr) {
    unimplemented!()
}
extern "C" fn rt_str_concat(_a: *const VeltStr, _b: *const VeltStr, _out: *mut VeltStr) {
    unimplemented!()
}
extern "C" fn rt_str_drop(_s: *mut VeltStr) {}
extern "C" fn rt_str_cmp(a: *const VeltStr, b: *const VeltStr) -> i32 {
    let (a, b) = unsafe { (str_bytes(a), str_bytes(b)) };
    a.cmp(b) as i32
}
extern "C" fn rt_pow_i64(a: i64, b: i64) -> i64 {
    if b < 0 {
        return i64::from(a == 1);
    }
    a.wrapping_pow(b as u32)
}
extern "C" fn c_memcpy(d: *mut u8, s: *const u8, n: usize) -> *mut u8 {
    unsafe { std::ptr::copy_nonoverlapping(s, d, n) };
    d
}
extern "C" fn c_memmove(d: *mut u8, s: *const u8, n: usize) -> *mut u8 {
    unsafe { std::ptr::copy(s, d, n) };
    d
}
extern "C" fn c_memset(d: *mut u8, c: i32, n: usize) -> *mut u8 {
    unsafe { std::ptr::write_bytes(d, c as u8, n) };
    d
}
extern "C" fn c_fmod(a: f64, b: f64) -> f64 {
    a % b
}
extern "C" fn c_fmodf(a: f32, b: f32) -> f32 {
    a % b
}
// Cranelift's libcalls for rounding when the ISA lacks a rounding instruction.
extern "C" fn c_floor(x: f64) -> f64 {
    x.floor()
}
extern "C" fn c_ceil(x: f64) -> f64 {
    x.ceil()
}
extern "C" fn c_trunc(x: f64) -> f64 {
    x.trunc()
}

/// JIT-compile `program` for the host, run `velt_main`, return (exit code, stdout).
pub(crate) fn run(program: &vir::Program, optimize: bool) -> Result<(i32, String), String> {
    let isa = crate::isa::make_isa(&crate::host_triple(), optimize, true)?;
    let mut jb = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    for (name, p) in stub_symbols() {
        jb.symbol(name, p);
    }
    let mut module = JITModule::new(jb);
    let built = crate::module::build_module(&mut module, program, &crate::module::Naming::Program)?;
    module.finalize_definitions().map_err(|e| e.to_string())?;
    let main_idx = program
        .funcs
        .iter()
        .position(|f| f.symbol == "velt_main")
        .ok_or("no velt_main")?;
    let code = module.get_finalized_function(built.funcs[main_idx].ok_or("velt_main not defined")?);
    let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(code) };
    let (rc, stdout) = call_capturing(main);
    // Keep the code alive only for the duration of the call.
    unsafe { module.free_memory() };
    Ok((rc, stdout))
}

/// Call `main`, returning its exit code and what it printed.
pub(super) fn call_capturing(main: extern "C" fn() -> i32) -> (i32, String) {
    OUT.with(|o| o.borrow_mut().clear());
    let rc = main();
    let stdout = OUT.with(|o| String::from_utf8(std::mem::take(&mut *o.borrow_mut())).unwrap());
    (rc, stdout)
}

/// The stub runtime: rt functions plus the libc/libm symbols Cranelift may call.
pub(super) fn stub_symbols() -> [(&'static str, *const u8); 24] {
    [
        ("velt_rt_write_str", rt_write_str as *const u8),
        ("velt_rt_write_i64", rt_write_i64 as *const u8),
        ("velt_rt_write_u64", rt_write_u64 as *const u8),
        ("velt_rt_write_f64", rt_write_f64 as *const u8),
        ("velt_rt_write_bool", rt_write_bool as *const u8),
        ("velt_rt_write_byte", rt_write_byte as *const u8),
        ("velt_rt_flush", rt_flush as *const u8),
        ("velt_rt_panic", rt_panic as *const u8),
        ("velt_rt_exit", rt_exit as *const u8),
        ("velt_rt_alloc", rt_alloc as *const u8),
        ("velt_rt_free", rt_free as *const u8),
        ("velt_rt_str_from_i64", rt_str_from_i64 as *const u8),
        ("velt_rt_str_concat", rt_str_concat as *const u8),
        ("velt_rt_str_drop", rt_str_drop as *const u8),
        ("velt_rt_str_cmp", rt_str_cmp as *const u8),
        ("velt_rt_pow_i64", rt_pow_i64 as *const u8),
        ("memcpy", c_memcpy as *const u8),
        ("memmove", c_memmove as *const u8),
        ("memset", c_memset as *const u8),
        ("fmod", c_fmod as *const u8),
        ("fmodf", c_fmodf as *const u8),
        ("floor", c_floor as *const u8),
        ("ceil", c_ceil as *const u8),
        ("trunc", c_trunc as *const u8),
    ]
}

fn check(tp: programs::TestProgram) {
    for optimize in [false, true] {
        let (rc, stdout) = run(&tp.program, optimize)
            .unwrap_or_else(|e| panic!("{} (optimize={optimize}): {e}", tp.name));
        assert_eq!(
            stdout, tp.stdout,
            "{} (optimize={optimize}) stdout",
            tp.name
        );
        assert_eq!(rc, tp.exit, "{} (optimize={optimize}) exit code", tp.name);
    }
}

#[test]
fn jit_fib() {
    check(programs::fib());
}

#[test]
fn jit_int_ops() {
    check(programs::int_ops());
}

#[test]
fn jit_float_ops() {
    check(programs::float_ops());
}

#[test]
fn jit_inline_math() {
    check(programs::math());
}

#[test]
fn jit_casts() {
    check(programs::casts());
}

#[test]
fn jit_aggregates() {
    check(programs::aggregates());
}

#[test]
fn jit_switch() {
    check(programs::switch());
}

#[test]
fn jit_indirect_calls() {
    check(programs::indirect());
}

#[test]
fn jit_strings_and_rt_calls() {
    check(programs::strings());
}

#[test]
fn jit_vtable_relocations() {
    check(programs::vtables());
}

#[test]
fn jit_dynamic_memcopy_memmove_memset() {
    check(programs::mem_ops());
}

/// `DevSession` (the `velt dev` JIT) runs every test program, one session for all of them.
#[test]
fn dev_session_runs_programs() {
    let mut session = crate::DevSession::new(&stub_symbols());
    for tp in programs::all() {
        let program = session
            .load(&tp.program)
            .unwrap_or_else(|e| panic!("{}: {e}", tp.name));
        let (rc, stdout) = call_capturing(program.main());
        assert_eq!(
            (rc, stdout.as_str()),
            (tp.exit, tp.stdout.as_str()),
            "{}",
            tp.name
        );
    }
}

/// Stack walks get through JIT frames: a backtrace taken inside a runtime call made by JIT code
/// reaches the Rust frame that called `velt_main` (needs the registered unwind info).
#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
fn backtrace_walks_through_jit_frames() {
    thread_local! {
        static TRACE: RefCell<String> = const { RefCell::new(String::new()) };
    }
    extern "C" fn write_i64_tracing(stream: u32, v: i64) {
        TRACE.with(|t| {
            if t.borrow().is_empty() {
                *t.borrow_mut() = std::backtrace::Backtrace::force_capture().to_string();
            }
        });
        rt_write_i64(stream, v);
    }
    #[inline(never)]
    fn enter_jit(main: extern "C" fn() -> i32) -> i32 {
        std::hint::black_box(main())
    }
    let mut symbols = stub_symbols();
    let slot = symbols.iter_mut().find(|(n, _)| *n == "velt_rt_write_i64");
    slot.expect("stub for velt_rt_write_i64").1 = write_i64_tracing as *const u8;
    let mut session = crate::DevSession::new(&symbols);
    let fib = programs::all().into_iter().find(|p| p.name == "fib");
    let program = session.load(&fib.expect("fib program").program).unwrap();
    OUT.with(|o| o.borrow_mut().clear());
    assert_eq!(enter_jit(program.main()), 0);
    let trace = TRACE.with(|t| t.take());
    assert!(
        trace.contains("enter_jit"),
        "the backtrace stops in JIT code:\n{trace}"
    );
}
