//! JSON support (rt_abi_async.md §12): velt_rt's reader of a database driver's rows and the
//! escaping, compiled from its sources.

#[path = "../../../velt_rt/src/json/error.rs"]
pub mod error;
#[path = "../../../velt_rt/src/json/escape.rs"]
pub mod escape;
#[path = "../../../velt_rt/src/json/node_syntax.rs"]
pub mod node_syntax;
#[path = "../../../velt_rt/src/json/reader.rs"]
pub mod reader;
#[path = "../../../velt_rt/src/json/reader_abi.rs"]
pub mod reader_abi;
#[path = "../../../velt_rt/src/json/rows_bin.rs"]
pub mod rows_bin;
#[path = "../../../velt_rt/src/json/rows_reader.rs"]
pub mod rows_reader;
#[path = "../../../velt_rt/src/json/scan.rs"]
pub mod scan;
#[path = "../../../velt_rt/src/json/walk.rs"]
pub mod walk;
