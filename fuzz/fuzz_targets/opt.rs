//! velt_opt: a random VIR program computes the same result and extern calls after optimization.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    velt_fuzz::vir::check_opt(&velt_fuzz::vir::Input::from_bytes(data));
});
