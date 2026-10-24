//! Database-agnostic JSON plumbing shared by the database drivers (`std/postgres`, and any
//! client/server driver that binds parameters and returns rows the same way; `std/sqlite` binds
//! its parameters here and writes its rows in binary form, `json/rows_bin.rs`).
//!
//! std passes query parameters as `JSON.stringify(params)` of a user object or array, and
//! decodes result rows with the compile-time generated `JSON.parse<T[]>`, so the runtime never
//! needs to know the user's types:
//! - [`params`] parses that JSON into typed values ([`DbValue`]) keyed by name or position;
//! - [`rows`] writes result rows as a JSON array of objects (`[{"col":val,...}]`), keyed by
//!   column name, with exact integers and JS-formatted floats.

pub use velt_text::db_json::{params, rows};
pub use velt_text::db_json::{
    parse_param_list, parse_params, push_byte_array, DbValue, Params, RowWriter,
};

#[cfg(test)]
mod tests;
