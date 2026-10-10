//! JSON support (rt_abi_async.md §12): velt_rt's reader, `json.Value` tree, accessors and
//! escaping, compiled from its sources.

#[path = "../../../velt_rt/src/json/cycle.rs"]
pub mod cycle;
#[path = "../../../velt_rt/src/json/error.rs"]
pub mod error;
#[path = "../../../velt_rt/src/json/escape.rs"]
pub mod escape;
#[path = "../../../velt_rt/src/json/layout.rs"]
pub mod layout;
#[path = "../../../velt_rt/src/json/object.rs"]
pub mod object;
#[path = "../../../velt_rt/src/json/reader.rs"]
pub mod reader;
#[path = "../../../velt_rt/src/json/reader_abi.rs"]
pub mod reader_abi;
#[path = "../../../velt_rt/src/json/scan.rs"]
pub mod scan;
#[path = "../../../velt_rt/src/json/text.rs"]
pub mod text;
#[path = "../../../velt_rt/src/json/value.rs"]
pub mod value;
#[path = "../../../velt_rt/src/json/value_abi.rs"]
pub mod value_abi;
#[path = "../../../velt_rt/src/json/value_edit.rs"]
pub mod value_edit;
#[path = "../../../velt_rt/src/json/walk.rs"]
pub mod walk;
