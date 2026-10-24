//! What std's decoder needs of an executing statement's rows (`velt_text::rows_cursor`): the
//! rows header and each cell.
//!
//! SQLite has no boolean type, so booleans come back as 0/1. A column whose declared type is
//! `BOOL` or `BOOLEAN` (a table column read directly, not an expression) is written as
//! `true`/`false` when it holds 0 or 1, so it decodes into a `bool` field; any other column
//! stays a number.

use super::error::DbError;
use super::raw::RawStmt;
use parking_lot::Mutex;
use rusqlite::ffi;
use std::sync::Arc;
use velt_text::rows::encode_names;
use velt_text::rows_cursor::Cell;

/// What a prepared statement's rows look like: the rows header (with the column names) and
/// which columns are declared boolean. Asking SQLite costs a call per column (each a UTF-8
/// check of the name), so a statement keeps them until SQLite compiles it anew.
pub struct Shape {
    /// SQLite's count of recompilations (a schema change) when the shape was read.
    reprepared: i32,
    count: usize,
    /// The rows header (`velt_text::rows`), for many rows and for a single one.
    rows_header: Vec<u8>,
    single_header: Vec<u8>,
    bools: Vec<bool>,
}

/// A statement's [`Shape`], once known.
pub type ShapeCell = Mutex<Option<Arc<Shape>>>;

impl Shape {
    fn of(stmt: &RawStmt) -> Shape {
        let count = stmt.column_count();
        let names: Vec<String> = (0..count).map(|i| stmt.column_name(i)).collect();
        let names = encode_names(names.iter().map(String::as_str));
        let header = |single: u8| [&[single][..], &names].concat();
        Shape {
            reprepared: stmt.status(ffi::SQLITE_STMTSTATUS_REPREPARE),
            count,
            rows_header: header(0),
            single_header: header(1),
            bools: (0..count).map(|i| stmt.declared_bool(i)).collect(),
        }
    }

    /// The rows header: `single`, the column count and names.
    pub fn header(&self, single: bool) -> &[u8] {
        match single {
            true => &self.single_header,
            false => &self.rows_header,
        }
    }

    /// Cell `col` of the row `stmt` is on. SQLite has no booleans: a 0 or 1 in a column
    /// declared `BOOL`/`BOOLEAN` is a boolean.
    ///
    /// # Safety
    /// `stmt` is on a row; the cell is valid until it steps.
    pub unsafe fn cell<'s>(&self, stmt: &'s RawStmt, col: usize) -> Cell<'s> {
        if col >= self.count {
            return Cell::Null;
        }
        match stmt.cell(col) {
            Cell::Int(v @ (0 | 1)) if self.bools[col] => Cell::Bool(v == 1),
            c => c,
        }
    }
}

/// The shape of `stmt`, which has stepped (so SQLite recompiled it if the schema changed),
/// from `cell` while it still applies.
pub fn shape(stmt: &RawStmt, cell: &ShapeCell) -> Arc<Shape> {
    let mut kept = cell.lock();
    match &*kept {
        Some(s) if s.reprepared == stmt.status(ffi::SQLITE_STMTSTATUS_REPREPARE) => s.clone(),
        _ => {
            let s = Arc::new(Shape::of(stmt));
            *kept = Some(s.clone());
            s
        }
    }
}

/// Step `stmt` (already bound) to completion, discarding any rows.
pub fn drain(stmt: &RawStmt) -> Result<(), DbError> {
    let r = loop {
        match stmt.step() {
            Ok(true) => continue,
            Ok(false) => break Ok(()),
            Err(e) => break Err(e),
        }
    };
    stmt.finish();
    r
}
