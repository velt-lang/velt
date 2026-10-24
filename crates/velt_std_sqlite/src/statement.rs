//! Prepared statements and the statement-level operations (prepare, run, query) that
//! [`crate::abi`] exports.
//!
//! rusqlite's `Statement` borrows its `Connection`, so a handle cannot own one. A statement
//! handle instead keeps an `Arc` of the connection object and of its [`Prepared`] (the SQL text
//! and the shape of its rows), and every call takes the compiled statement out of the
//! connection's prepared-statement cache (`prepare_cached`: a hash lookup; SQLite compiles it
//! once) under the connection lock. This keeps `close()` of the database safe while statements
//! exist.
//!
//! The connection keeps one [`Prepared`] per SQL text ([`DbObj::prepared`]): preparing the same
//! SQL again (a statement prepared per request) finds it without asking SQLite, and every
//! handle of that SQL shares the shape its first query read.

use super::bind::bind;
use super::connection::{DbHandle, DbObj};
use super::error::DbError;
use super::rows::{self, ShapeCell};
use std::sync::Arc;
use velt_native::handles::Single;
use velt_text::db_json::parse_params;

/// A statement: its connection and what it runs.
pub struct StmtObj {
    db: Arc<DbObj>,
    prepared: Arc<Prepared>,
}

/// SQL a connection compiled: the text (trimmed, the key of rusqlite's cache) and the shape of
/// its rows once a query read them.
pub struct Prepared {
    pub(crate) sql: String,
    shape: ShapeCell,
}

impl Prepared {
    pub(crate) fn new(sql: String) -> Prepared {
        Prepared {
            sql,
            shape: ShapeCell::default(),
        }
    }
}

/// A statement's handle: std's `Statement` class owns it (it never copies the handle, and
/// `close()` or dropping releases it once and forgets it), so it is the object's address.
pub type StmtHandle = Single<StmtObj>;

/// `stmt.run()` result: rows changed and the rowid of the last insert (std's `RunSlot`).
#[derive(velt_native::Value, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct RunInfo {
    /// Rows inserted, updated or deleted by the statement.
    pub changes: i64,
    /// `last_insert_rowid()` of the connection after the statement.
    pub last_insert_rowid: i64,
}

fn obj<'a>(stmt: StmtHandle) -> Result<&'a StmtObj, DbError> {
    // SAFETY: std's `Statement` passes a handle it has not released (or 0, after `close()`).
    unsafe { stmt.get() }.ok_or_else(|| DbError::closed("statement"))
}

/// `db.prepare(sql)`: compiles `sql` (exactly one statement) into the connection's cache, so
/// syntax errors surface here; SQL this connection compiled before is not compiled again.
pub fn prepare(db: DbHandle, sql: &[u8]) -> Result<StmtHandle, DbError> {
    let o = super::connection::obj(db)?;
    let prepared = o.prepared(sql.trim_ascii())?;
    Ok(Single::new(Arc::new(StmtObj { db: o, prepared })))
}

/// `stmt.run()` / `runWith(params)`: execute to completion. `params` is the JSON of the
/// parameters, empty for none (db_json::params).
pub fn run(stmt: StmtHandle, params: &[u8]) -> Result<RunInfo, DbError> {
    let stmt = obj(stmt)?;
    stmt.db.with(|conn| {
        let params = parse_params(params).map_err(DbError::range)?;
        let mut st = conn.prepare_cached(&stmt.prepared.sql)?;
        bind(&mut st, &params)?;
        rows::drain(&mut st, &stmt.prepared.shape)?;
        Ok(RunInfo {
            changes: conn.changes() as i64,
            last_insert_rowid: conn.last_insert_rowid(),
        })
    })
}

/// `stmt.all()` / `get()`: binds `params` (JSON, empty for none) and steps to the first row;
/// a cursor over the rows ([`super::cursor`]) for std's decoder, which gives it back to
/// [`super::cursor::close`]; with `first_only` the first row alone, and 0 when there is none.
pub fn open_rows(stmt: StmtHandle, params: &[u8], first_only: bool) -> Result<u64, DbError> {
    let stmt = obj(stmt)?;
    let p = &stmt.prepared;
    super::cursor::open(stmt.db.lock_owned(), &p.sql, &p.shape, first_only, |st| {
        let params = parse_params(params).map_err(DbError::range)?;
        bind(st, &params)
    })
}

/// Count a failure std detected for this statement (a row that does not decode into the
/// requested type) against its connection, so an enclosing `transaction` rolls back.
pub fn note_error(stmt: StmtHandle, e: DbError) {
    if let Ok(s) = obj(stmt) {
        s.db.note_error(e);
    }
}

/// Release a statement handle (null is a no-op). Its compiled form stays in the connection's
/// cache until evicted or the connection closes.
///
/// # Safety
/// `stmt` is not used afterwards (std's `Statement` zeroes its handle).
pub unsafe fn free(stmt: StmtHandle) {
    stmt.release();
}
