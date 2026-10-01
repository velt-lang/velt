//! `std/postgres`: a PostgreSQL client over `tokio-postgres` (TLS: rustls with the runtime's
//! `ring` provider). ABI: rt_abi_async.md §14.13.
//!
//! Every server round trip is an async leaf future. Parameters arrive as JSON text
//! (`JSON.stringify` of the user's object or array, parsed by [`crate::db_json`]) and are
//! converted for the parameter types the server inferred; rows leave as JSON text for std's
//! `JSON.parse<T[]>`. Nothing here stores code pointers (§13.5): `transaction(fn)` is
//! implemented in Velt over `begin` / `end` and the client's failure counter.
//!
//! - [`config`]: connection strings and `sslmode`;
//! - [`socket`]: opening the socket and running the startup over it;
//! - [`tls`]: the rustls connector;
//! - [`wire`]: the stream under tokio-postgres that batches inject their message groups into;
//! - [`connection`]: one connection, its statement cache and transaction depth;
//! - [`batch`]: one statement × N parameter sets in one group with one `Sync`;
//! - [`statements`]: the LRU prepared-statement cache;
//! - [`placeholders`]: `:name` / `$name` → `$n`;
//! - [`bind`]: JSON values → parameters of the prepared types;
//! - [`types`], [`temporal`], [`network`]: column values → JSON; [`rows`]: result sets → JSON;
//! - [`client`], [`pool`]: the handles and the ABI; [`copy`]: `COPY` streaming;
//! - [`error`]: `PgError` (SQLSTATE or Node-style code) and its JSON form.

pub mod batch;
pub mod bind;
pub mod client;
pub mod config;
pub mod connection;
pub mod copy;
pub mod error;
pub mod network;
pub mod placeholders;
pub mod pool;
pub mod rows;
pub mod socket;
pub mod statements;
pub mod temporal;
pub mod tls;
pub mod types;
pub mod wire;

#[cfg(test)]
mod tests;
