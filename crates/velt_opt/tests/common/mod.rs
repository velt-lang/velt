//! Test support shared by the unit tests (included via `#[path]` from `src/lib.rs`) and the
//! integration tests: a VIR builder and a strict VIR validator.
// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

pub mod builder;
pub mod validate;
