//! Reading an executing SQLite statement's rows for std's `JSON.parse<T[]>`, in the binary
//! form the JSON reader walks directly ([`velt_text::rows`]).
//!
//! SQLite has no boolean type, so booleans come back as 0/1. A column whose declared type is
//! `BOOL` or `BOOLEAN` (a table column read directly, not an expression) is written as
//! `true`/`false` when it holds 0 or 1, so it decodes into a `bool` field; any other column
//! stays a number.

use super::error::DbError;
use rusqlite::types::ValueRef;
use rusqlite::Statement;
use velt_text::rows::BinRows;
use velt_text::Out;

/// Step `stmt` (already bound) and encode its rows into `out`: every row, or with `first_only`
/// the first row alone (nothing at all if there is none; the statement is not stepped further).
pub fn read<O: Out>(stmt: &mut Statement<'_>, first_only: bool, out: O) -> Result<O, DbError> {
    let count = stmt.column_count();
    if first_only {
        // Nothing written until there is a row: no row is an empty result.
        let mut rows = stmt.raw_query();
        let Some(row) = rows.next()? else {
            return Ok(out);
        };
        let row_stmt = row.as_ref();
        let names = (0..count).map(|i| row_stmt.column_name(i).unwrap_or(""));
        let mut w = BinRows::with_out(out, names, true);
        let mut bools: Option<Vec<bool>> = None;
        write_row(&mut w, row, count, &mut bools)?;
        return Ok(w.finish());
    }
    // A name SQLite cannot give (out of memory) is empty.
    let names = (0..count).map(|i| stmt.column_name(i).unwrap_or(""));
    let mut w = BinRows::with_out(out, names, false);
    // Which columns are declared boolean: looked up when the first 0 or 1 turns up, since most
    // results have none and the declared types cost a call per column.
    let mut bools: Option<Vec<bool>> = None;
    let mut rows = stmt.raw_query();
    while let Some(row) = rows.next()? {
        write_row(&mut w, row, count, &mut bools)?;
    }
    Ok(w.finish())
}

/// One row's cells. SQLite has no booleans: a 0 or 1 in a column declared `BOOL`/`BOOLEAN` is
/// written as a boolean (the declared types looked up at the first 0 or 1, since most results
/// have none and they cost a call per column).
fn write_row<O: Out>(
    w: &mut BinRows<O>,
    row: &rusqlite::Row<'_>,
    count: usize,
    bools: &mut Option<Vec<bool>>,
) -> Result<(), DbError> {
    for i in 0..count {
        match row.get_ref(i)? {
            ValueRef::Null => w.null(),
            ValueRef::Integer(v @ (0 | 1)) => {
                let bools = bools.get_or_insert_with(|| declared_bools(row.as_ref()));
                if bools[i] {
                    w.bool(v == 1);
                } else {
                    w.int(v);
                }
            }
            ValueRef::Integer(v) => w.int(v),
            ValueRef::Real(v) => w.float(v),
            ValueRef::Text(t) => w.text(t),
            ValueRef::Blob(b) => w.bytes(b),
        }
    }
    w.end_row();
    Ok(())
}

/// For every column of `stmt`, whether it is declared `BOOL` or `BOOLEAN`.
fn declared_bools(stmt: &Statement<'_>) -> Vec<bool> {
    stmt.columns()
        .iter()
        .map(|c| is_bool_type(c.decl_type()))
        .collect()
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
