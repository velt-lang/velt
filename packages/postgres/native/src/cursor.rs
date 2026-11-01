//! A query's rows as a row cursor (`velt_text::rows_cursor`, native_abi.md "Row cursors"): the Velt side
//! decodes them from the rows the server sent, value by value, instead of from JSON text made
//! of them first. Each value is the one `super::types::push_value` would write as JSON: numbers,
//! bools, text, bytea and json straight from the row; the others rendered when asked for
//! (`super::types::cell`), once per row.

use super::error::PgError;
use super::rows::{unsupported, Raw};
use super::types;
use std::cell::UnsafeCell;
use tokio_postgres::{Row, Statement};
use velt_text::rows::encode_names;
use velt_text::rows_cursor::{close_cursor, open_cursor, Cell, RowCursor};

struct PgCursor {
    statement: Statement,
    rows: Vec<Row>,
    header: Vec<u8>,
    single: bool,
    /// The row the cursor is on, and the next one.
    current: Option<usize>,
    next: usize,
    /// The current row's cells as handed out, and what was rendered for them: kept until the
    /// next step, so a cell stays valid that long (rows and renderings do not move meanwhile).
    cells: UnsafeCell<Vec<Option<Cell<'static>>>>,
    rendered: UnsafeCell<Vec<Vec<u8>>>,
    failure: UnsafeCell<Option<PgError>>,
}

impl RowCursor for PgCursor {
    fn header(&self) -> &[u8] {
        &self.header
    }

    fn step(&mut self) -> bool {
        if (self.single && self.next > 0) || self.next >= self.rows.len() {
            self.current = None;
            return false;
        }
        self.current = Some(self.next);
        self.next += 1;
        self.cells.get_mut().iter_mut().for_each(|c| *c = None);
        true
    }

    fn cell(&self, col: usize) -> Cell<'_> {
        let Some(row) = self.current.map(|i| &self.rows[i]) else {
            return Cell::Null;
        };
        // SAFETY: only this cursor touches its cells and renderings, one call at a time; a
        // cell handed out stays put until `step` (which takes `&mut self`).
        unsafe {
            let cells = &mut *self.cells.get();
            if let Some(c) = cells.get(col).copied().flatten() {
                return c;
            }
            let column = &self.statement.columns()[col];
            let raw = match row.try_get::<_, Raw>(col) {
                Ok(Raw(Some(raw))) => raw,
                Ok(Raw(None)) => return Cell::Null,
                Err(e) => return self.fail(PgError::from(e)),
            };
            let rendered = &mut *self.rendered.get();
            let scratch = &mut rendered[col];
            let c = match types::cell(column.type_(), raw, scratch) {
                Ok(c) => std::mem::transmute::<Cell<'_>, Cell<'static>>(c),
                Err(what) => return self.fail(unsupported(column, &what)),
            };
            cells[col] = Some(c);
            c
        }
    }
}

impl PgCursor {
    /// A value that could not be read: null to the decoder, and the failure the Velt side learns when it
    /// gives the cursor back.
    unsafe fn fail(&self, e: PgError) -> Cell<'static> {
        (*self.failure.get()).get_or_insert(e);
        Cell::Null
    }
}

/// A cursor over `rows` of `statement` (with `single`, the first row alone): its handle for the Velt side,
/// or 0 when `single` has no row.
pub fn open(statement: Statement, rows: Vec<Row>, single: bool) -> u64 {
    if single && rows.is_empty() {
        return 0;
    }
    let columns = statement.columns().len();
    let mut header = vec![single as u8];
    header.extend(encode_names(statement.columns().iter().map(|c| c.name())));
    // Registered with the runtime: the Velt side holds a handle the runtime checks, not an
    // address.
    open_cursor(Box::new(PgCursor {
        statement,
        rows,
        header,
        single,
        current: None,
        next: 0,
        cells: UnsafeCell::new(vec![None; columns]),
        rendered: UnsafeCell::new(vec![Vec::new(); columns]),
        failure: UnsafeCell::new(None),
    }))
}

/// Releases a cursor [`open`] made (0 is a no-op); a value that could not be read, if any. A
/// handle that names no open cursor (closed already, made up) is `ECLOSED`.
pub fn close(handle: u64) -> Result<(), PgError> {
    if handle == 0 {
        return Ok(());
    }
    let cursor = close_cursor::<PgCursor>(handle).ok_or_else(|| PgError::closed("row cursor"))?;
    cursor.failure.into_inner().map_or(Ok(()), Err)
}
