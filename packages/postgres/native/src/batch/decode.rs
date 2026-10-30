//! A batch's replies (every server message up to its `ReadyForQuery`) as one JSON array with
//! an entry per execution (see [`Mode`]), or the first error the server reported.

use super::Mode;
use crate::error::PgError;
use crate::rows::push_row;
use bytes::BytesMut;
use postgres_protocol::message::backend::{
    CommandCompleteBody, DataRowBody, ErrorResponseBody, Message,
};
use tokio_postgres::fallible_iterator::FallibleIterator;
use tokio_postgres::Column;
use velt_text::db_json::RowWriter;
use velt_text::Out;

fn protocol(what: impl std::fmt::Display) -> PgError {
    PgError::new("UNKNOWN", format!("unexpected batch reply: {what}"))
}

/// The JSON of `executions` results from `replies` (see the module docs).
pub fn results<O: Out>(
    mut replies: BytesMut,
    columns: &[Column],
    executions: usize,
    mode: Mode,
    mut out: O,
) -> Result<O, PgError> {
    out.push(b'[');
    // Each execution's writer owns the output while its rows come in.
    let mut out = Some(out);
    let mut done = 0;
    let mut set: Option<RowWriter<O>> = None;
    let mut error = None;
    while let Some(message) = Message::parse(&mut replies).map_err(protocol)? {
        match message {
            Message::ParseComplete | Message::CloseComplete => {}
            Message::BindComplete => {
                let mut o = out
                    .take()
                    .ok_or_else(|| protocol("a BindComplete inside a result"))?;
                if done > 0 {
                    o.push(b',');
                }
                let names = columns.iter().map(Column::name);
                set = Some(RowWriter::with_out(o, names, mode == Mode::Rows));
            }
            Message::DataRow(row) => {
                let w = set
                    .as_mut()
                    .ok_or_else(|| protocol("a row before BindComplete"))?;
                if mode == Mode::Rows || (mode == Mode::FirstRow && w.rows() == 0) {
                    push_data_row(w, columns, &row)?;
                }
            }
            Message::CommandComplete(_) | Message::EmptyQueryResponse => {
                let w = set
                    .take()
                    .ok_or_else(|| protocol("a result before BindComplete"))?;
                out = Some(finish_set(w, &message, mode)?);
                done += 1;
            }
            Message::ErrorResponse(body) => {
                // The server skips to the Sync after an error: there is only one.
                error.get_or_insert(db_error(&body));
            }
            Message::ReadyForQuery(_) => break,
            _ => return Err(protocol("an unknown message")),
        }
    }
    if let Some(e) = error {
        return Err(e);
    }
    if done != executions {
        return Err(protocol(format!(
            "{done} results for {executions} executions"
        )));
    }
    let mut out = out.ok_or_else(|| protocol("a result without its end"))?;
    out.push(b']');
    Ok(out)
}

/// Close one execution's entry; the output back.
fn finish_set<O: Out>(w: RowWriter<O>, end: &Message, mode: Mode) -> Result<O, PgError> {
    let empty = w.rows() == 0;
    let mut out = w.finish();
    match mode {
        Mode::Rows => {}
        Mode::FirstRow if empty => out.extend(b"null"),
        Mode::FirstRow => {}
        Mode::Count => {
            let n = match end {
                Message::CommandComplete(body) => rows_affected(body)?,
                _ => 0,
            };
            velt_text::fmt::push_u64(&mut out, n);
        }
    }
    Ok(out)
}

fn push_data_row<O: Out>(
    w: &mut RowWriter<O>,
    columns: &[Column],
    row: &DataRowBody,
) -> Result<(), PgError> {
    let buffer = row.buffer();
    let ranges: Vec<_> = row.ranges().collect().map_err(protocol)?;
    if ranges.len() != columns.len() {
        return Err(protocol("a row with the wrong number of columns"));
    }
    let values = ranges.into_iter().map(|r| Ok(r.map(|r| &buffer[r])));
    push_row(w, columns, values)
}

/// The count at the end of a command tag (`UPDATE 3`, `INSERT 0 1`, `SELECT 5`); 0 if none.
fn rows_affected(body: &CommandCompleteBody) -> Result<u64, PgError> {
    let tag = body.tag().map_err(protocol)?;
    Ok(tag
        .rsplit(' ')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0))
}

/// The server's error: SQLSTATE, message, detail and constraint.
fn db_error(body: &ErrorResponseBody) -> PgError {
    let mut e = PgError::new("UNKNOWN", "");
    let mut fields = body.fields();
    while let Ok(Some(f)) = fields.next() {
        let value = String::from_utf8_lossy(f.value_bytes()).into_owned();
        match f.type_() {
            b'C' => e.code = value,
            b'M' => e.message = value,
            b'D' => e.detail = Some(value),
            b'n' => e.constraint = Some(value),
            _ => {}
        }
    }
    e
}
