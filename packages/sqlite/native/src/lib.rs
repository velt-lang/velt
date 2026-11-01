//! The `sqlite` package's native library: embedded SQLite through `rusqlite` (SQLite compiled in:
//! the `bundled` feature, no system library), on the native-package SDK (`velt_native`,
//! `velt_text`). Its `#[export]` functions are the `velt_sqlite__*` declarations of
//! `src/native.vlt`; the ABI is docs/native_abi.md.
//!
//! The API is **synchronous**, like better-sqlite3: a typical query takes microseconds, less
//! than a hop to a blocking thread pool and back would cost, and SQLite serializes writers
//! anyway. Long queries can run on another thread with `spawn`. Nothing here stores code
//! pointers (§13.5): `Database.transaction` is implemented in Velt.
//!
//! Parameters arrive through a parameter sink ([`sink`], filled by velt/db's `bindParams`) or as
//! JSON text (`JSON.stringify` of the user's params, parsed by [`velt_text::db_json`]); rows
//! leave through a row cursor ([`cursor`]) that velt/db's `decodeRows` reads. Handles
//! (database, statement) are keys of SDK registries, so a closed one fails cleanly.
//!
//! - [`connection`]: the connection object, open/close/exec/pragma, transaction state;
//! - [`statement`]: prepared statements, run/query;
//! - [`bind`]: JSON parameters → SQLite bindings;
//! - [`rows`]: SQLite rows → JSON;
//! - [`error`]: result codes and names;
//! - [`abi`]: the exports.

velt_native::package!(sqlite);

pub mod abi;
pub mod bind;
pub mod connection;
pub mod cursor;
pub mod error;
pub(crate) mod raw;
pub mod rows;
pub mod sink;
pub mod statement;

#[cfg(test)]
mod tests;
