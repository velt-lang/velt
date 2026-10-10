//! Velt runtime: process entry, allocation, strings, printing, panics, and the tokio-based async
//! runtime with timers, fs, TCP and HTTP, plus the string-method, string-builder and JSON support
//! used by generated code. Exposes a C ABI documented in docs/internals/contracts/rt_abi.md
//! (sync core) and docs/internals/contracts/rt_abi_async.md (async, std/fs, std/net, std/http, process).

//!
//! Every exported symbol is `#[no_mangle] extern "C"`. Pointer arguments follow the contracts
//! (valid, aligned, non-null unless stated), which is the safety contract of the `unsafe extern "C"`
//! functions below.
#![allow(clippy::missing_safety_doc)]

#[cfg(velt_rt_host)]
pub mod abi_symbols;
pub mod array;
pub mod bigint;
pub mod build_profile;
pub mod bytes;
pub mod bytes_ops;
pub mod cell_owner;
pub mod child;
pub mod db_json;
#[cfg(all(debug_assertions, not(velt_rt_host)))]
pub mod debug_alloc;
pub mod dev;
pub mod drop_depth;
pub mod entry;
pub mod fmt;
pub mod fnv;
pub mod freed;
pub mod fs;
pub mod handle;
pub mod hash;
pub mod html;
pub mod http;
pub mod inspect;
mod inspect_cycles;
mod inspect_layout;
pub mod io;
pub mod json;
pub mod localtime;
pub mod math;
pub mod mem;
pub mod memory_usage;
pub mod native;
pub mod net;
pub mod os;
pub mod panic;
pub mod postgres;
pub mod prng;
pub mod process;
pub mod random;
pub mod redis;
pub mod regex;
pub mod registry;
pub mod result;
pub mod shared;
pub mod sqlite;
pub mod stdin;
pub mod str;
pub mod str_array;
pub mod str_ops;
pub mod strbuf;
pub mod symbol;
pub mod task;
pub mod timer;
pub mod tls;
pub mod transfer_map;
pub mod weak;
pub mod ws;

pub use crate::str::VeltStr;

/// The allocator under everything (mimalloc unless built without the `mimalloc` feature).
#[cfg(feature = "mimalloc")]
type InnerAlloc = mimalloc::MiMalloc;
#[cfg(not(feature = "mimalloc"))]
type InnerAlloc = std::alloc::System;

#[cfg(any(not(debug_assertions), velt_rt_host))]
#[global_allocator]
static GLOBAL: InnerAlloc = InnerAlloc {};

// The debug runtime linked into programs can check every allocation (`VELT_RT_DEBUG_ALLOC=1`).
#[cfg(all(debug_assertions, not(velt_rt_host)))]
#[global_allocator]
static GLOBAL: debug_alloc::DebugAlloc<InnerAlloc> =
    debug_alloc::DebugAlloc::new(InnerAlloc {}, &GLOBAL_QUARANTINE);

#[cfg(all(debug_assertions, not(velt_rt_host)))]
static GLOBAL_QUARANTINE: debug_alloc::Quarantine = debug_alloc::Quarantine::new();

/// ABI tests written as a "fake compiler": hand-written C-ABI state machines driven through the
/// public ABI. They live under tests/abi/ but are compiled into the unit-test binary because the
/// non-test rlib defines the C `main` (see entry.rs), which integration tests cannot link.
#[cfg(test)]
#[path = "../tests/abi/mod.rs"]
mod abi_tests;
