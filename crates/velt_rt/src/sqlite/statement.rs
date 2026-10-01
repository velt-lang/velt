//! `VeltSqliteStmt`: a prepared statement and the statement-level ABI (prepare, run, query).
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
use crate::db_json::parse_params;
use crate::handle::Handle;
use crate::result::IoResult;
use crate::str::VeltStr;
use std::sync::Arc;

/// A statement: its connection and SQL text (trimmed, the cache key).
pub struct StmtObj {
    db: Arc<DbObj>,
    sql: String,
}

/// Opaque handle (`Arc<StmtObj>`).
pub type StmtHandle = Handle<StmtObj>;

/// `stmt.run()` result: rows changed and the rowid of the last insert (`{ i64; i64 }`).
#[repr(C)]
pub struct RunInfo {
    /// Rows inserted, updated or deleted by the statement.
    pub changes: i64,
    /// `last_insert_rowid()` of the connection after the statement.
    pub last_insert_rowid: i64,
}

unsafe fn bytes<'a>(s: *const VeltStr) -> &'a [u8] {
    (*s).as_bytes()
}

unsafe fn obj<'a>(stmt: StmtHandle) -> Result<&'a StmtObj, DbError> {
    stmt.get().ok_or_else(|| DbError::closed("statement"))
}

/// `db.prepare(sql)` → `IoResult<VeltSqliteStmt>`: compiles `sql` (exactly one statement) into
/// the connection's cache, so syntax errors surface here.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sqlite_prepare(
    db: DbHandle,
    sql: *const VeltStr,
    out: *mut IoResult<StmtHandle>,
) {
    // Velt strings are UTF-8 by construction.
    let sql = std::str::from_utf8_unchecked(bytes(sql)).trim();
    let r = super::connection::obj(db).and_then(|o| {
        o.with(|c| c.prepare_cached(sql).map(drop).map_err(DbError::from))
            .map(|()| StmtObj {
                db: o.clone(),
                sql: sql.to_string(),
            })
    });
    out.write(match r {
        Ok(s) => IoResult::ok(Handle::from_arc(Arc::new(s))),
        Err(e) => IoResult::err(e.to_velt()),
    });
}

fn run(stmt: &StmtObj, params: &[u8]) -> Result<RunInfo, DbError> {
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

/// `stmt.run()` / `runWith(params)`: execute to completion. `params` is the JSON of the
/// parameters, `""` for none (db_json::params).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sqlite_run(
    stmt: StmtHandle,
    params: *const VeltStr,
    out: *mut IoResult<RunInfo>,
) {
    let r = obj(stmt).and_then(|s| run(s, bytes(params)));
    out.write(match r {
        Ok(info) => IoResult::ok(info),
        Err(e) => IoResult::err(e.to_velt()),
    });
}

fn query(stmt: &StmtObj, params: &[u8], first_only: bool) -> Result<Vec<u8>, DbError> {
    stmt.db.with(|conn| {
        let params = parse_params(params).map_err(DbError::range)?;
        let mut st = conn.prepare_cached(&stmt.sql)?;
        bind(&mut st, &params)?;
        rows::read(&mut st, first_only)
    })
}

/// `stmt.all()` / `get()`: the rows as JSON text (`[{...},...]`), or with `first_only` the first
/// row's object (`""` if there is none).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sqlite_query(
    stmt: StmtHandle,
    params: *const VeltStr,
    first_only: u8,
    out: *mut IoResult<VeltStr>,
) {
    let r = obj(stmt).and_then(|s| query(s, bytes(params), first_only != 0));
    out.write(match r {
        Ok(json) => IoResult::ok(VeltStr::from_vec(json)),
        Err(e) => IoResult::err(e.to_velt()),
    });
}

/// Count a failure std detected for this statement (a row that does not decode into the
/// requested type) against its connection, so an enclosing `transaction` rolls back.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sqlite_note_error(
    stmt: StmtHandle,
    code: i32,
    message: *const VeltStr,
) {
    if let Some(s) = stmt.get() {
        let message = String::from_utf8_lossy(bytes(message)).into_owned();
        s.db.note_error(DbError::new(code, message));
    }
}

/// Release a statement handle (null is a no-op). Its compiled form stays in the connection's
/// cache until evicted or the connection closes.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_sqlite_stmt_free(stmt: StmtHandle) {
    stmt.release();
}
