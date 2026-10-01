//! `std/sqlite`: embedded SQLite through `rusqlite` (SQLite compiled in: the `bundled`
//! feature, no system library). ABI: rt_abi_async.md §14.11.
//!
//! The API is **synchronous**, like better-sqlite3: a typical query takes microseconds, less
//! than a hop to a blocking thread pool and back would cost, and SQLite serializes writers
//! anyway. Long queries can run on another thread with `spawn`. Nothing here stores code
//! pointers (§13.5): `Database.transaction` is implemented in Velt.
//!
//! Parameters arrive as JSON text (`JSON.stringify` of the user's params object or array) and
//! rows leave as JSON text for `JSON.parse<T[]>`; both are the database-agnostic
//! [`crate::db_json`]. Handles (`VeltSqliteDb`, `VeltSqliteStmt`) are `Arc`s.
//!
//! - [`connection`]: the connection object, open/close/exec/pragma, transaction state;
//! - [`statement`]: prepared statements, run/query;
//! - [`bind`]: JSON parameters → SQLite bindings;
//! - [`rows`]: SQLite rows → JSON;
//! - [`error`]: result codes and names.

pub mod bind;
pub mod connection;
pub mod error;
pub mod rows;
pub mod statement;

#[cfg(test)]
mod tests;
