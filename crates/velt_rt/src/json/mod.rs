//! JSON support for generated code: the pull reader behind compile-time generated
//! `JSON.parse<T>` decoders, the `json.Value` tree behind `JSON.parseValue`, and the escaping used
//! by `JSON.stringify` glue (via `velt_rt_strbuf_push_json_*`), and the text forms of a
//! `json.Value` (`text`: `JSON.stringify` and what `console.log` prints). ABI: rt_abi_async.md §12.
//!
//! Everything is byte-level over UTF-8 input: no allocation while lexing except for strings that
//! contain escapes (and for the owned strings/values handed to generated code).

pub mod cycle;
pub mod error;
pub mod layout;
pub mod escape;
pub mod object;
pub mod reader;
pub mod reader_abi;
pub mod scan;
pub mod text;
pub mod value;
pub mod value_abi;
pub mod value_edit;
pub mod walk;
