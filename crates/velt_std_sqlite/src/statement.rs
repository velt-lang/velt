//! Prepared statements and the statement-level operations (prepare, run, query) that
//! [`crate::abi`] exports.
//!
//! The connection keeps one [`Prepared`] per SQL text ([`DbObj::prepared`]): the compiled
//! statement (owned, [`RawStmt`]) and the shape of its rows. Preparing the same SQL again (a
//! statement prepared per request) finds it without asking SQLite; a statement handle keeps an
//! `Arc` of it and of the connection object, and uses the statement under the connection's
//! lock. Closing the database finalizes every statement first, so `close()` is safe while
//! handles exist (they then fail with "not open").

use super::bind::bind;
use super::connection::{DbHandle, DbObj};
use super::error::DbError;
use super::raw::RawStmt;
use super::rows::{self, ShapeCell};
use std::sync::Arc;
use velt_native::handles::Single;
use velt_text::db_json::parse_params;

/// A statement: what it runs and its connection.
pub struct StmtObj {
    // Dropped first: the connection object may go with `db`, and it finalizes its statements
    // before it closes, which needs this one to be its own alone by then.
    prepared: Arc<Prepared>,
    db: Arc<DbObj>,
}

/// SQL a connection compiled: the statement, and the shape of its rows once a query read them.
pub struct Prepared {
    pub(crate) stmt: RawStmt,
    pub(crate) shape: ShapeCell,
}

impl Prepared {
    pub(crate) fn new(stmt: RawStmt) -> Prepared {
        Prepared {
            stmt,
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
    Ok(Single::new(Arc::new(StmtObj { prepared, db: o })))
}

/// `stmt.run()` / `runWith(params)`: execute to completion. `params` is the JSON of the
/// parameters, empty for none (db_json::params).
pub fn run(stmt: StmtHandle, params: &[u8]) -> Result<RunInfo, DbError> {
    let stmt = obj(stmt)?;
    stmt.db.with(|conn| {
        let params = parse_params(params).map_err(DbError::range)?;
        let st = &stmt.prepared.stmt;
        if let Err(e) = bind(st, &params) {
            st.finish();
            return Err(e);
        }
        rows::drain(st)?;
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
    super::cursor::open(stmt.db.lock_owned(), &stmt.prepared, first_only, |st| {
        let params = parse_params(params).map_err(DbError::range)?;
        bind(st, &params)
    })
}

/// `stmt.getWith(params)` and the like, for a parameter type std hands over value by value: a
/// sink taking them ([`super::sink`]).
pub fn bind_begin(stmt: StmtHandle) -> Result<u64, DbError> {
    let stmt = obj(stmt)?;
    super::sink::begin(stmt.db.lock_owned(), &stmt.prepared)
}

/// Count a failure std detected for this statement (a row that does not decode into the
/// requested type) against its connection, so an enclosing `transaction` rolls back.
pub fn note_error(stmt: StmtHandle, e: DbError) {
    if let Ok(s) = obj(stmt) {
        s.db.note_error(e);
    }
}

/// Release a statement handle (null is a no-op). Its compiled form stays with the connection
/// until it is forgotten or the connection closes.
///
/// # Safety
/// `stmt` is not used afterwards (std's `Statement` zeroes its handle).
pub unsafe fn free(stmt: StmtHandle) {
    stmt.release();
}
