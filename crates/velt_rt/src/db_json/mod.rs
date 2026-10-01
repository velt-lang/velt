//! Database-agnostic JSON plumbing shared by the database drivers (`std/sqlite`, and any
//! client/server driver that binds parameters and returns rows the same way).
//!
//! std passes query parameters as `JSON.stringify(params)` of a user object or array, and
//! decodes result rows with the compile-time generated `JSON.parse<T[]>`, so the runtime never
//! needs to know the user's types:
//! - [`params`] parses that JSON into typed values ([`DbValue`]) keyed by name or position;
//! - [`rows`] writes result rows as a JSON array of objects (`[{"col":val,...}]`), keyed by
//!   column name, with exact integers and JS-formatted floats.

pub mod params;
pub mod rows;

pub use params::{parse_param_list, parse_params, DbValue, Params};
pub use rows::{push_byte_array, RowWriter};

#[cfg(test)]
mod tests;
