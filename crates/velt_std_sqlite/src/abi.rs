//! The exports std/sqlite declares (`velt_std_sqlite__*`, rt_abi_async.md §14.11): thin
//! wrappers over [`crate::connection`] and [`crate::statement`]. A database or statement handle
//! is a registry key (`u64`): a closed or null one fails with "SQLITE_MISUSE".

use crate::connection::{self, DbHandle};
use crate::error::{code_name, DbError};
use crate::statement::{self, RunInfo, StmtHandle};
use velt_native::handles::Key;
use velt_native::{export, Error, Str};

fn io<T>(r: Result<T, DbError>) -> Result<T, Error> {
    r.map_err(|e| e.to_error())
}

fn db(h: u64) -> DbHandle {
    Key::from_bits(h)
}

fn stmt(h: u64) -> StmtHandle {
    velt_native::handles::Single::from_bits(h)
}

/// `open(path, opts)` → `IoResult<u64>` (see [`connection::open`]).
#[export]
fn velt_std_sqlite__open(
    path: &str,
    readonly: bool,
    create: bool,
    timeout_ms: u32,
) -> Result<u64, Error> {
    io(connection::open(path, readonly, create, timeout_ms).map(Key::bits))
}

/// `db.close()` → `IoStatus`.
#[export]
fn velt_std_sqlite__close(db_h: u64) -> Result<(), Error> {
    io(connection::close(db(db_h)))
}

/// `db.exec(sql)` → `IoStatus`.
#[export]
fn velt_std_sqlite__exec(db_h: u64, sql: &str) -> Result<(), Error> {
    io(connection::exec(db(db_h), sql))
}

/// `db.pragma(source)` → `IoResult<string>`.
#[export]
fn velt_std_sqlite__pragma(db_h: u64, source: &str) -> Result<String, Error> {
    io(connection::pragma(db(db_h), source))
}

/// Whether a transaction is open; false once closed.
#[export]
fn velt_std_sqlite__in_transaction(db_h: u64) -> bool {
    connection::in_transaction(db(db_h))
}

/// Failed operations so far.
#[export]
fn velt_std_sqlite__error_count(db_h: u64) -> u64 {
    connection::error_count(db(db_h))
}

/// Forget failures after `count`.
#[export]
fn velt_std_sqlite__reset_error_count(db_h: u64, count: u64) {
    connection::reset_error_count(db(db_h), count)
}

/// The most recent failure as a failed status (success if there was none).
#[export]
fn velt_std_sqlite__last_error(db_h: u64) -> Result<(), Error> {
    connection::last_error(db(db_h)).map_or(Ok(()), |e| Err(e.to_error()))
}

/// `db.prepare(sql)` → `IoResult<u64>`.
#[export]
fn velt_std_sqlite__prepare(db_h: u64, sql: &Str) -> Result<u64, Error> {
    io(statement::prepare(db(db_h), sql.as_bytes()).map(|s| s.bits()))
}

/// `stmt.run()` → `IoResult<RunSlot>`; `params` is JSON (empty: none), read as stored: it comes
/// from `JSON.stringify`, which escapes lone surrogates, so it is UTF-8 and needs no check.
#[export]
fn velt_std_sqlite__run(stmt_h: u64, params: &Str) -> Result<RunInfo, Error> {
    io(statement::run(stmt(stmt_h), params.as_bytes()))
}

/// `stmt.all()` / `get()` → `IoResult<u64>`: a cursor over the rows
/// (`velt_text::rows_cursor`) for `__intrinsic_json_parse_cursor`, given back to
/// `velt_std_sqlite__close_rows`; 0 if `first_only` found no row. `params` is JSON, as for
/// `run`.
#[export]
fn velt_std_sqlite__open_rows(stmt_h: u64, params: &Str, first_only: bool) -> Result<u64, Error> {
    io(statement::open_rows(
        stmt(stmt_h),
        params.as_bytes(),
        first_only,
    ))
}

/// Releases a cursor `velt_std_sqlite__open_rows` made (0: nothing); fails with what SQLite
/// met while stepping its rows.
#[export]
fn velt_std_sqlite__close_rows(cursor: u64) -> Result<(), Error> {
    // SAFETY: std gives back each cursor it was handed, once, after decoding.
    io(unsafe { super::cursor::close(cursor) })
}

/// Count a decode failure std found against the statement's connection.
#[export]
fn velt_std_sqlite__note_error(stmt_h: u64, code: i32, message: &str) {
    statement::note_error(stmt(stmt_h), DbError::new(code, message))
}

/// Release a statement handle (null is a no-op); std's `Statement` releases it once.
#[export]
fn velt_std_sqlite__stmt_free(stmt_h: u64) {
    // SAFETY: the owning `Statement` zeroes its handle after this.
    unsafe { statement::free(stmt(stmt_h)) }
}

/// The name of SQLite result `code` ("SQLITE_CONSTRAINT_UNIQUE", …).
#[export]
fn velt_std_sqlite__error_name(code: i32) -> String {
    code_name(code).to_string()
}
