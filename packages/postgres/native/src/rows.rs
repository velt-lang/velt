//! Result rows as JSON text (`[{"col":value,...}]`, `velt_text::db_json::RowWriter`), each value
//! converted by [`super::types`]. Rows are read as raw bytes (a pass-through `FromSql`), so no
//! intermediate Rust values are built.

use super::error::{PgError, UNSUPPORTED};
use super::types::{decodable, push_value};
use std::error::Error as StdError;
use tokio_postgres::types::{FromSql, Type};
use tokio_postgres::Column;
use velt_text::db_json::RowWriter;
use velt_text::Out;

/// A column's raw binary value (`None` = NULL).
pub(crate) struct Raw<'a>(pub(crate) Option<&'a [u8]>);

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

pub(crate) fn unsupported(column: &Column, what: &str) -> PgError {
    PgError::new(
        UNSUPPORTED,
        format!(
            "column \"{}\" has {what}, which the postgres package cannot decode; cast it, e.g. \"{}\"::text",
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
