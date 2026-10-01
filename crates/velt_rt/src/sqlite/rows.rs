//! Reading an executing SQLite statement's rows into JSON text for std's `JSON.parse<T[]>`
//! (the encoding is [`crate::db_json::rows`]).
//!
//! SQLite has no boolean type, so booleans come back as 0/1. A column whose declared type is
//! `BOOL` or `BOOLEAN` (a table column read directly, not an expression) is written as
//! `true`/`false` when it holds 0 or 1, so it decodes into a `bool` field; any other column
//! stays a number.

use super::error::DbError;
use crate::db_json::RowWriter;
use rusqlite::types::ValueRef;
use rusqlite::Statement;

/// Step `stmt` (already bound) and encode its rows: every row as a JSON array, or with
/// `first_only` the first row's object (empty text if there is none; the statement is not
/// stepped further).
pub fn read(stmt: &mut Statement<'_>, first_only: bool) -> Result<Vec<u8>, DbError> {
    let columns = stmt.columns();
    let bools: Vec<bool> = columns
        .iter()
        .map(|c| is_bool_type(c.decl_type()))
        .collect();
    let mut w = RowWriter::new(columns.iter().map(|c| c.name()));
    drop(columns);
    let mut rows = stmt.raw_query();
    while let Some(row) = rows.next()? {
        w.begin_row();
        for (i, &is_bool) in bools.iter().enumerate() {
            match row.get_ref(i)? {
                ValueRef::Null => w.null(),
                ValueRef::Integer(v @ (0 | 1)) if is_bool => w.bool(v == 1),
                ValueRef::Integer(v) => w.int(v),
                ValueRef::Real(v) => w.float(v),
                ValueRef::Text(t) => w.text(t),
                ValueRef::Blob(b) => w.bytes(b),
            }
        }
        w.end_row();
        if first_only {
            return Ok(w.into_rows());
        }
    }
    Ok(if first_only {
        w.into_rows()
    } else {
        w.into_array()
    })
}

fn is_bool_type(decl: Option<&str>) -> bool {
    decl.is_some_and(|t| t.eq_ignore_ascii_case("BOOLEAN") || t.eq_ignore_ascii_case("BOOL"))
}

/// Step `stmt` (already bound) to completion, discarding any rows.
pub fn drain(stmt: &mut Statement<'_>) -> Result<(), DbError> {
    let mut rows = stmt.raw_query();
    while rows.next()?.is_some() {}
    Ok(())
}
