//! A statement's rows as a live cursor (`velt_text::rows_cursor`, native_abi.md "Row cursors"):
//! velt/db's decoder reads each value from the stepped statement into the Velt value it builds,
//! instead of from a buffer the rows were written into first.
//!
//! The cursor holds the connection's lock while the Velt side decodes (a synchronous call: nothing else
//! of the Velt side runs meanwhile), and releases it when the Velt side gives the cursor back ([`close`]), which
//! reports a failure SQLite met while stepping.

use super::connection::Locked;
use super::error::DbError;
use super::raw::RawStmt;
use super::rows::{self, Shape};
use super::statement::Prepared;
use std::sync::Arc;
use velt_text::rows_cursor::{close_cursor, open_cursor, Cell, RowCursor};

/// The rows of a stepped statement.
pub(crate) struct SqliteCursor {
    prepared: Arc<Prepared>,
    shape: Arc<Shape>,
    single: bool,
    /// The first row was stepped when the cursor was made and not handed out yet.
    pending: bool,
    done: bool,
    failure: Option<DbError>,
    /// Dropped last: the lock under which the statement is used.
    locked: Locked,
}

impl SqliteCursor {
    #[inline]
    fn stmt(&self) -> &RawStmt {
        &self.prepared.stmt
    }
}

impl RowCursor for SqliteCursor {
    fn header(&self) -> &[u8] {
        self.shape.header(self.single)
    }

    fn step(&mut self) -> bool {
        if std::mem::take(&mut self.pending) {
            return true;
        }
        if self.done {
            return false;
        }
        match self.stmt().step() {
            Ok(true) => true,
            Ok(false) => {
                self.done = true;
                false
            }
            Err(e) => {
                self.failure = Some(e);
                self.done = true;
                false
            }
        }
    }

    #[inline]
    fn cell(&self, col: usize) -> Cell<'_> {
        if self.done {
            return Cell::Null;
        }
        // SAFETY: on a row (not done); `Shape::cell` checks `col`.
        unsafe { self.shape.cell(self.stmt(), col) }
    }
}

impl Drop for SqliteCursor {
    fn drop(&mut self) {
        // Still under the lock (dropped after this).
        self.stmt().finish();
        if let Some(e) = &self.failure {
            self.locked.note(e.clone());
        }
    }
}

/// Binds the parameters to `prepared`'s statement on the locked connection (`bind`), steps it
/// to its first row and makes a cursor over its rows (with `single`, the first row alone),
/// registered for the Velt side: its handle, or 0 when `single` found no row.
pub(crate) fn open(
    locked: Locked,
    prepared: &Arc<Prepared>,
    single: bool,
    bind: impl FnOnce(&RawStmt) -> Result<(), DbError>,
) -> Result<u64, DbError> {
    // Registered with the runtime: the Velt side holds a handle the runtime checks, not an
    // address.
    Ok(make(locked, prepared, single, bind)?.map_or(0, open_cursor))
}

/// [`open`]'s cursor before it is registered (`None` when `single` found no row).
pub(crate) fn make(
    mut locked: Locked,
    prepared: &Arc<Prepared>,
    single: bool,
    bind: impl FnOnce(&RawStmt) -> Result<(), DbError>,
) -> Result<Option<Box<SqliteCursor>>, DbError> {
    let first = locked
        .conn()
        .and_then(|_| bind(&prepared.stmt))
        .and_then(|()| prepared.stmt.step());
    let pending = match first {
        Ok(on) => on,
        Err(e) => {
            prepared.stmt.finish();
            locked.note(e.clone());
            return Err(e);
        }
    };
    if single && !pending {
        prepared.stmt.finish();
        return Ok(None);
    }
    // After the first step: SQLite recompiled the statement if the schema changed.
    let shape = rows::shape(&prepared.stmt, &prepared.shape);
    Ok(Some(Box::new(SqliteCursor {
        prepared: prepared.clone(),
        shape,
        single,
        pending,
        done: !pending,
        failure: None,
        locked,
    })))
}

/// Releases a cursor [`open`] made (0 is a no-op), and with it the connection's lock; the
/// failure SQLite met while stepping, if any (counted against the connection). A handle that
/// names no open cursor (closed already, made up) is `SQLITE_MISUSE`.
pub fn close(handle: u64) -> Result<(), DbError> {
    if handle == 0 {
        return Ok(());
    }
    let cursor =
        close_cursor::<SqliteCursor>(handle).ok_or_else(|| DbError::closed("row cursor"))?;
    finish(cursor)
}

/// Releases a cursor taken back: the failure SQLite met while stepping, if any.
pub(crate) fn finish(cursor: Box<SqliteCursor>) -> Result<(), DbError> {
    let failure = cursor.failure.clone();
    drop(cursor);
    failure.map_or(Ok(()), Err)
}
