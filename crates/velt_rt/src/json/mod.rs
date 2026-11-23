//! The runtime's JSON reader of a database driver's rows (`rows_reader`, with the text reader
//! `reader` it uses for nested JSON cells, their messages and Node's syntax messages), which goes
//! with the native-package ABI (#2563); the rest of JSON is std's Velt code
//! (docs/internals/design/json-engine.md). ABI: rt_abi_async.md §12.
//!
//! Everything is byte-level over UTF-8 input: no allocation while lexing except for strings that
//! contain escapes (and for the owned strings handed to generated code).

pub mod error;
pub mod escape;
pub mod node_syntax;
pub mod reader;
pub mod reader_abi;
pub mod rows_bin;
pub mod rows_reader;
pub mod scan;
pub mod walk;
