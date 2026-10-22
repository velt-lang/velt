//! Text and JSON as Velt writes them, shared by the runtime (`velt_rt`) and the native libraries
//! of std's drivers (built on the `velt_native` SDK, std/*/native): Velt's string encoding
//! ([`wtf8`]), JavaScript's number formatting ([`fmt`]), JSON scanning and escaping, and the
//! database drivers' parameter and row JSON ([`db_json`]). One copy, so a driver in a native
//! library formats numbers and rows exactly as the runtime does.

pub mod db_json;
pub mod fmt;
pub mod json_escape;
pub mod json_scan;
pub mod wtf8;
