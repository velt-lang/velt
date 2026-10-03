//! "Fake compiler" ABI tests: each test hand-writes, in Rust, the `extern "C"` state machines and
//! `#[repr(C)]` states that generated code would contain, and drives them only through the public
//! runtime ABI (docs/internals/contracts/rt_abi_async.md). The string/JSON tests likewise play generated
//! code: template-literal glue, string methods, and a hand-written `JSON.parse<User>` decoder.

mod core;
mod fake;
mod fs;
mod http;
mod http_bench;
mod js_table;
mod json_reader;
mod json_union;
mod json_value;
mod json_value_edit;
mod local;
mod net;
mod perf;
mod strings;
mod text_perf;
mod utf16_model;
mod utf16_props;
mod utf16_table;
