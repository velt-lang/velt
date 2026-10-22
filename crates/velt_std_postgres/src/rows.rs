//! Result rows as JSON text (`[{"col":value,...}]`, `velt_text::db_json::RowWriter`), each value
//! converted by [`super::types`]. Rows are read as raw bytes (a pass-through `FromSql`), so no
//! intermediate Rust values are built.

use super::error::{PgError, UNSUPPORTED};
use super::types::{decodable, push_value};
use std::error::Error as StdError;
use tokio_postgres::types::{FromSql, Type};
use tokio_postgres::{Column, Row};
use velt_text::db_json::RowWriter;
use velt_text::Out;

/// A column's raw binary value (`None` = NULL).
struct Raw<'a>(Option<&'a [u8]>);

impl<'a> FromSql<'a> for Raw<'a> {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn StdError + Sync + Send>> {
        Ok(Raw(Some(raw)))
    }

    fn from_sql_null(_: &Type) -> Result<Self, Box<dyn StdError + Sync + Send>> {
        Ok(Raw(None))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

fn unsupported(column: &Column, what: &str) -> PgError {
    PgError::new(
        UNSUPPORTED,
        format!(
            "column \"{}\" has {what}, which std/postgres cannot decode; cast it, e.g. \"{}\"::text",
            column.name(),
            column.name()
        ),
    )
}

/// Fails for a column whose type cannot be decoded.
pub fn check_columns(columns: &[Column]) -> Result<(), PgError> {
    match columns.iter().find(|c| !decodable(c.type_())) {
        Some(c) => Err(unsupported(c, &format!("type {}", c.type_().name()))),
        None => Ok(()),
    }
}

/// `rows` as a JSON array, or with `first_only` the first row's object (nothing if none),
/// appended to `out`.
pub fn to_json<O: Out>(
    columns: &[Column],
    rows: &[Row],
    first_only: bool,
    out: O,
) -> Result<O, PgError> {
    let mut w = RowWriter::with_out(out, columns.iter().map(Column::name), !first_only);
    let take = if first_only { 1 } else { rows.len() };
    for row in rows.iter().take(take) {
        let values = (0..columns.len()).map(|i| {
            row.try_get::<_, Raw>(i)
                .map(|raw| raw.0)
                .map_err(PgError::from)
        });
        push_row(&mut w, columns, values)?;
    }
    Ok(w.finish())
}

/// One row: each column's raw binary value (`None` = NULL), in column order.
pub fn push_row<'v, O: Out>(
    w: &mut RowWriter<O>,
    columns: &[Column],
    values: impl Iterator<Item = Result<Option<&'v [u8]>, PgError>>,
) -> Result<(), PgError> {
    w.begin_row();
    for (column, raw) in columns.iter().zip(values) {
        match raw? {
            None => w.null(),
            Some(bytes) => push_value(w.raw_value(), column.type_(), bytes)
                .map_err(|e| unsupported(column, &e))?,
        }
    }
    w.end_row();
    Ok(())
}
