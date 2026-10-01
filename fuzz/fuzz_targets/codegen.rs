//! Backends: random VIR (plain and optimized) compiles with Cranelift and to LLVM IR.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    velt_fuzz::vir::check_codegen(&velt_fuzz::vir::Input::from_bytes(data));
});
