//! A compiled SQLite statement the driver owns (`sqlite3_stmt`, through rusqlite's `ffi`),
//! rather than one rusqlite's statement cache lends per call: a [`Prepared`] keeps its own for
//! as long as the connection knows its SQL, so a query neither looks it up by its text nor
//! hands it back, and reads cells without rusqlite's per-value checks.
//!
//! A statement is only used under its connection's lock (the connection is opened without
//! SQLite's own mutex), and finalized before the connection closes.
//!
//! [`Prepared`]: super::statement::Prepared

use super::error::DbError;
use rusqlite::ffi;
use rusqlite::Connection;
use std::ffi::{c_int, CStr};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use velt_text::rows_cursor::Cell;

/// An owned `sqlite3_stmt` (null for SQL without a statement, such as a comment, and once
/// finalized).
pub struct RawStmt(AtomicPtr<ffi::sqlite3_stmt>);

/// `SQLITE_TRANSIENT`: SQLite copies the bound text or blob.
fn transient() -> ffi::sqlite3_destructor_type {
    ffi::SQLITE_TRANSIENT()
}

impl RawStmt {
    /// Compiles `sql` on `conn` (which accepted it already, so this only fails if the schema
    /// changed in between).
    pub fn prepare(conn: &Connection, sql: &str) -> Result<RawStmt, DbError> {
        let len = c_int::try_from(sql.len())
            .map_err(|_| DbError::new(ffi::SQLITE_TOOBIG, "SQL too long"))?;
        let mut stmt = ptr::null_mut();
        // SAFETY: a live connection (its lock is held); `sql` outlives the call.
        unsafe {
            let db = conn.handle();
            let rc = ffi::sqlite3_prepare_v3(
                db,
                sql.as_ptr().cast(),
                len,
                ffi::SQLITE_PREPARE_PERSISTENT,
                &mut stmt,
                ptr::null_mut(),
            );
            if rc != ffi::SQLITE_OK {
                return Err(failure(db, rc));
            }
        }
        Ok(RawStmt(AtomicPtr::new(stmt)))
    }

    #[inline]
    fn ptr(&self) -> *mut ffi::sqlite3_stmt {
        self.0.load(Ordering::Relaxed)
    }

    /// Finalizes the statement now (before its connection closes); later calls see none.
    pub fn finalize(&self) {
        let p = self.0.swap(ptr::null_mut(), Ordering::Relaxed);
        if !p.is_null() {
            // SAFETY: owned, finalized once (the pointer is gone).
            unsafe { ffi::sqlite3_finalize(p) };
        }
    }

    pub fn parameter_count(&self) -> usize {
        match self.ptr() {
            p if p.is_null() => 0,
            // SAFETY: a live statement.
            p => unsafe { ffi::sqlite3_bind_parameter_count(p) as usize },
        }
    }

    /// The name of parameter `i` (from 1) with its prefix (`:id`), or None for a `?`.
    pub fn parameter_name(&self, i: usize) -> Option<&str> {
        // SAFETY: a live statement; SQLite keeps the name as long as the statement.
        unsafe {
            let n = ffi::sqlite3_bind_parameter_name(self.ptr(), i as c_int);
            if n.is_null() {
                return None;
            }
            CStr::from_ptr(n).to_str().ok()
        }
    }

    /// Binds parameter `i` (from 1); text and blobs are copied.
    pub fn bind(&self, i: usize, v: Bind<'_>) -> Result<(), DbError> {
        let (p, i) = (self.ptr(), i as c_int);
        // SAFETY: a live statement; text and blobs are copied (`SQLITE_TRANSIENT`).
        let rc = unsafe {
            match v {
                Bind::Null => ffi::sqlite3_bind_null(p, i),
                Bind::Int(v) => ffi::sqlite3_bind_int64(p, i, v),
                Bind::Float(v) => ffi::sqlite3_bind_double(p, i, v),
                Bind::Text(t) => ffi::sqlite3_bind_text64(
                    p,
                    i,
                    t.as_ptr().cast(),
                    t.len() as u64,
                    transient(),
                    ffi::SQLITE_UTF8 as u8,
                ),
                Bind::Blob(b) => {
                    ffi::sqlite3_bind_blob64(p, i, b.as_ptr().cast(), b.len() as u64, transient())
                }
            }
        };
        match rc {
            ffi::SQLITE_OK => Ok(()),
            // SAFETY: a live statement.
            rc => Err(unsafe { failure(ffi::sqlite3_db_handle(p), rc) }),
        }
    }

    /// Steps: true for a row, false once done (the statement is then reset).
    pub fn step(&self) -> Result<bool, DbError> {
        let p = self.ptr();
        if p.is_null() {
            return Ok(false);
        }
        // SAFETY: a live statement.
        unsafe {
            match ffi::sqlite3_step(p) {
                ffi::SQLITE_ROW => Ok(true),
                ffi::SQLITE_DONE => {
                    ffi::sqlite3_reset(p);
                    Ok(false)
                }
                rc => {
                    let e = failure(ffi::sqlite3_db_handle(p), rc);
                    ffi::sqlite3_reset(p);
                    Err(e)
                }
            }
        }
    }

    /// Ends a run: the statement is reset and its parameters cleared.
    pub fn finish(&self) {
        let p = self.ptr();
        if !p.is_null() {
            // SAFETY: a live statement.
            unsafe {
                ffi::sqlite3_reset(p);
                ffi::sqlite3_clear_bindings(p);
            }
        }
    }

    /// `SQLITE_STMTSTATUS_*` counter `op`.
    pub fn status(&self, op: c_int) -> i32 {
        match self.ptr() {
            p if p.is_null() => 0,
            // SAFETY: a live statement.
            p => unsafe { ffi::sqlite3_stmt_status(p, op, 0) },
        }
    }

    pub fn column_count(&self) -> usize {
        match self.ptr() {
            p if p.is_null() => 0,
            // SAFETY: a live statement.
            p => unsafe { ffi::sqlite3_column_count(p) as usize },
        }
    }

    /// The name of column `col` (empty if SQLite cannot give it; not UTF-8 becomes U+FFFD).
    pub fn column_name(&self, col: usize) -> String {
        // SAFETY: a live statement, `col` below the column count.
        unsafe {
            let n = ffi::sqlite3_column_name(self.ptr(), col as c_int);
            match n.is_null() {
                true => String::new(),
                false => CStr::from_ptr(n).to_string_lossy().into_owned(),
            }
        }
    }

    /// Whether column `col` is a table column declared `BOOL` or `BOOLEAN`.
    pub fn declared_bool(&self, col: usize) -> bool {
        // SAFETY: as `column_name`.
        unsafe {
            let t = ffi::sqlite3_column_decltype(self.ptr(), col as c_int);
            !t.is_null() && {
                let t = CStr::from_ptr(t).to_bytes();
                t.eq_ignore_ascii_case(b"BOOLEAN") || t.eq_ignore_ascii_case(b"BOOL")
            }
        }
    }

    /// Cell `col` of the current row, valid until the next step.
    ///
    /// # Safety
    /// The statement is on a row and `col` is below its column count.
    #[inline]
    pub unsafe fn cell(&self, col: usize) -> Cell<'_> {
        let (p, i) = (self.ptr(), col as c_int);
        match ffi::sqlite3_column_type(p, i) {
            ffi::SQLITE_INTEGER => Cell::Int(ffi::sqlite3_column_int64(p, i)),
            ffi::SQLITE_FLOAT => Cell::Float(ffi::sqlite3_column_double(p, i)),
            ffi::SQLITE_TEXT => {
                // The text first, then its length (SQLite's advice).
                let t = ffi::sqlite3_column_text(p, i);
                let n = ffi::sqlite3_column_bytes(p, i) as usize;
                Cell::Text(bytes(t, n))
            }
            ffi::SQLITE_BLOB => {
                let b = ffi::sqlite3_column_blob(p, i);
                let n = ffi::sqlite3_column_bytes(p, i) as usize;
                Cell::Blob(bytes(b.cast(), n))
            }
            _ => Cell::Null,
        }
    }
}

impl Drop for RawStmt {
    fn drop(&mut self) {
        self.finalize();
    }
}

// SAFETY: used only under its connection's lock (module docs).
unsafe impl Send for RawStmt {}
unsafe impl Sync for RawStmt {}

unsafe fn bytes<'a>(p: *const u8, n: usize) -> &'a [u8] {
    match p.is_null() || n == 0 {
        true => &[],
        false => std::slice::from_raw_parts(p, n),
    }
}

/// A parameter value.
pub enum Bind<'a> {
    Null,
    Int(i64),
    Float(f64),
    Text(&'a [u8]),
    Blob(&'a [u8]),
}

/// The failure `rc` on connection `db`, worded as rusqlite words it (the message SQLite keeps
/// for it, or the code's own).
unsafe fn failure(db: *mut ffi::sqlite3, rc: c_int) -> DbError {
    let msg = if db.is_null() || ffi::sqlite3_errcode(db) != rc {
        ffi::sqlite3_errstr(rc)
    } else {
        ffi::sqlite3_errmsg(db)
    };
    let msg = (!msg.is_null()).then(|| CStr::from_ptr(msg).to_string_lossy().into_owned());
    DbError::from(rusqlite::Error::SqliteFailure(ffi::Error::new(rc), msg))
}
