//! `std/sqlite`: embedded SQLite through `rusqlite` (SQLite compiled in: the `bundled`
//! feature, no system library). ABI: rt_abi_async.md §14.11. A crate on the native-package SDK
//! only (`velt_native`, `velt_text`), linked into the runtime: its `#[export]` functions are
//! std's `velt_std_sqlite__*` declarations.
//!
//! The API is **synchronous**, like better-sqlite3: a typical query takes microseconds, less
//! than a hop to a blocking thread pool and back would cost, and SQLite serializes writers
//! anyway. Long queries can run on another thread with `spawn`. Nothing here stores code
//! pointers (§13.5): `Database.transaction` is implemented in Velt.
//!
//! Parameters arrive as JSON text (`JSON.stringify` of the user's params object or array,
//! parsed by [`velt_text::db_json`]); rows leave as a `u8[]` of binary rows
//! ([`velt_text::rows`]) that std decodes with the generated `JSON.parse<T[]>` decoder. Handles
//! (database, statement) are keys of SDK registries, so a closed one fails cleanly.
//!
//! - [`connection`]: the connection object, open/close/exec/pragma, transaction state;
//! - [`statement`]: prepared statements, run/query;
//! - [`bind`]: JSON parameters → SQLite bindings;
//! - [`rows`]: SQLite rows → JSON;
//! - [`error`]: result codes and names;
//! - [`abi`]: the exports.

pub mod abi;
pub mod bind;
pub mod connection;
pub mod cursor;
pub mod error;
pub(crate) mod raw;
pub mod rows;
pub mod statement;

#[cfg(test)]
mod tests;
