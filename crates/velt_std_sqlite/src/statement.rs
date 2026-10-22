//! Prepared statements and the statement-level operations (prepare, run, query) that
//! [`crate::abi`] exports.
//!
//! rusqlite's `Statement` borrows its `Connection`, so a handle cannot own one. A statement
//! handle instead keeps an `Arc` of the connection object and its SQL text, and every call
//! takes the compiled statement out of the connection's prepared-statement cache
//! (`prepare_cached`: a hash lookup; SQLite compiles it once) under the connection lock. This
//! keeps `close()` of the database safe while statements exist.

use super::bind::bind;
use super::connection::{DbHandle, DbObj};
use super::error::DbError;
use super::rows;
use std::sync::Arc;
use velt_native::handles::{Key, Registry};
use velt_text::db_json::parse_params;
use velt_text::Out;

/// A statement: its connection and SQL text (trimmed, the cache key).
pub struct StmtObj {
    db: Arc<DbObj>,
    sql: String,
}

/// Opaque handle: a key into [`STATEMENTS`].
pub type StmtHandle = Key<StmtObj>;

static STATEMENTS: Registry<StmtObj> = Registry::new();

/// `stmt.run()` result: rows changed and the rowid of the last insert (std's `RunSlot`).
#[derive(velt_native::Value, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct RunInfo {
    /// Rows inserted, updated or deleted by the statement.
    pub changes: i64,
    /// `last_insert_rowid()` of the connection after the statement.
    pub last_insert_rowid: i64,
}

fn obj(stmt: StmtHandle) -> Result<Arc<StmtObj>, DbError> {
    STATEMENTS
        .get(stmt)
        .ok_or_else(|| DbError::closed("statement"))
}

/// `db.prepare(sql)`: compiles `sql` (exactly one statement) into the connection's cache, so
/// syntax errors surface here.
pub fn prepare(db: DbHandle, sql: &str) -> Result<StmtHandle, DbError> {
    let sql = sql.trim();
    let o = super::connection::obj(db)?;
    o.with(|c| c.prepare_cached(sql).map(drop).map_err(DbError::from))?;
    Ok(STATEMENTS.insert(StmtObj {
        db: o,
        sql: sql.to_string(),
    }))
}

/// `stmt.run()` / `runWith(params)`: execute to completion. `params` is the JSON of the
/// parameters, empty for none (db_json::params).
pub fn run(stmt: StmtHandle, params: &[u8]) -> Result<RunInfo, DbError> {
    let stmt = obj(stmt)?;
    stmt.db.with(|conn| {
        let params = parse_params(params).map_err(DbError::range)?;
        let mut st = conn.prepare_cached(&stmt.sql)?;
        bind(&mut st, &params)?;
        rows::drain(&mut st)?;
        Ok(RunInfo {
            changes: conn.changes() as i64,
            last_insert_rowid: conn.last_insert_rowid(),
        })
    })
}

/// `stmt.all()` / `get()`: the rows in binary form ([`velt_text::rows`]) appended to `out`:
/// every row, or with `first_only` the first row's object; nothing if `first_only` found no
/// row.
pub fn query<O: Out>(
    stmt: StmtHandle,
    params: &[u8],
    first_only: bool,
    out: O,
) -> Result<O, DbError> {
    let stmt = obj(stmt)?;
    stmt.db.with(|conn| {
        let params = parse_params(params).map_err(DbError::range)?;
        let mut st = conn.prepare_cached(&stmt.sql)?;
        bind(&mut st, &params)?;
        rows::read(&mut st, first_only, out)
    })
}

/// Count a failure std detected for this statement (a row that does not decode into the
/// requested type) against its connection, so an enclosing `transaction` rolls back.
pub fn note_error(stmt: StmtHandle, e: DbError) {
    if let Some(s) = STATEMENTS.get(stmt) {
        s.db.note_error(e);
    }
}

/// Release a statement handle (null is a no-op). Its compiled form stays in the connection's
/// cache until evicted or the connection closes.
pub fn free(stmt: StmtHandle) {
    STATEMENTS.remove(stmt);
}
