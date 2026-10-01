//! Properties checked by the fuzz targets in `fuzz_targets/` (one module per stage). Kept in a
//! library so each property is unit-tested on known inputs and the targets stay one-liners.
//! A property failure is a panic, which libFuzzer reports as a crash with the input saved.

pub mod entry;
pub mod frontend;
pub mod json;
pub mod numbers;
pub mod syntax;
pub mod vir;

/// Shared VIR builders/validator of the optimizer's tests (reused, not copied).
#[path = "../../crates/velt_opt/tests/common/mod.rs"]
mod common;

/// The LLVM backend tests' random VIR generator (edge-value arithmetic, casts, loops, memory).
#[path = "../../crates/velt_codegen_llvm/tests/native/random.rs"]
mod random;
