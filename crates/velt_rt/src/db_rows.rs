//! Rows read straight into typed values: what the code velt/db's generators write
//! (velt/db/generate/columns.velt) calls for `decodeRows<T>` / `decodeRow<T>` when the row type is
//! an object of plain fields. Each column is matched to its field once per result
//! ([`velt_rt_rows_begin_named`]), and each value is read by column, typed, without the JSON
//! reader's keys. The results, and the messages of a row that does not fit, are those of the
//! JSON decoder.
//!
//! A rows state is passed as the `u64` its address is (std declares it so).
//!
//! # Safety
//! For every function: `state` came from [`velt_rt_rows_begin_named`] and is not ended yet;
//! `out` points at an all-zero value of the type written; `col` is below the column count.

use crate::bytes::VeltBytes;
use crate::str::VeltStr;
use velt_text::rows::{header_end, name_at, NAMES};
use velt_text::rows_cursor::{Cell, RawCursor};

/// 2^63 as a float: integral floats in `[-2^63, 2^63)` are exact `i64`s.
const I64_LIMIT: f64 = 9_223_372_036_854_775_808.0;

/// A read's result: the value was written, the cell is null, or it is of another kind.
const MISMATCH: u8 = 0;
const OK: u8 = 1;
const NULL: u8 = 2;

/// A result being read: the driver's cursor and each column's field.
pub struct RowsState {
    cursor: RawCursor,
    /// The field each column fills (-1: none).
    fields: Vec<i32>,
    /// What `fields` was made for: the rows header and the field names. The next result with
    /// both the same (a statement run again) reuses it.
    header: Vec<u8>,
    names: Vec<u8>,
}

thread_local! {
    /// The state of this thread's last result, for the next (each read is synchronous: one at a
    /// time per thread, but a state is taken out while in use).
    static SPARE: std::cell::Cell<Option<Box<RowsState>>> = const { std::cell::Cell::new(None) };
}

/// The state at address `st`.
unsafe fn state<'a>(st: u64) -> &'a mut RowsState {
    &mut *(st as usize as *mut RowsState)
}

/// Starts reading the rows of `cursor` (the address of a driver's `RawCursor`, kept by the
/// caller until [`velt_rt_rows_end`]) into values whose fields are named in `names`: each name
/// with its length in UTF-8 bytes in front (`2:id4:name`). The rows state's address.
///
/// # Safety
/// `cursor` is a live `RawCursor`'s address; `names` is a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_begin_named(cursor: u64, names: *const VeltStr) -> u64 {
    let raw = cursor as usize as *const RawCursor;
    let cursor = RawCursor {
        data: (*raw).data,
        vtable: (*raw).vtable,
    };
    let header = cursor.header();
    let names = (*names).as_bytes();
    let mut state = match SPARE.with(|s| s.take()) {
        Some(mut s) => {
            s.cursor = cursor;
            s
        }
        None => Box::new(RowsState {
            cursor,
            fields: Vec::new(),
            header: Vec::new(),
            names: Vec::new(),
        }),
    };
    if state.names != names || state.header != header {
        map_columns(&mut state, header, names);
    }
    Box::into_raw(state) as usize as u64
}

/// Each column's field, by name (a repeated name: each column of it fills the field).
fn map_columns(state: &mut RowsState, header: &[u8], names: &[u8]) {
    let columns = header_end(header).map_or(0, |(_, n, _)| n);
    let fields = named_fields(names);
    state.fields.clear();
    let mut at = NAMES;
    for _ in 0..columns {
        let (start, end, _) = name_at(header, at);
        at = end;
        let name = &header[start..end];
        let field = fields
            .iter()
            .position(|n| *n == name)
            .map_or(-1, |i| i as i32);
        state.fields.push(field);
    }
    state.header.clear();
    state.header.extend_from_slice(header);
    state.names.clear();
    state.names.extend_from_slice(names);
}

/// The names of a field list `2:id4:name`.
fn named_fields(names: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < names.len() {
        let colon = names[at..]
            .iter()
            .position(|&b| b == b':')
            .map_or(names.len(), |i| at + i);
        let len: usize = std::str::from_utf8(&names[at..colon])
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let start = (colon + 1).min(names.len());
        let end = (start + len).min(names.len());
        out.push(&names[start..end]);
        at = end;
    }
    out
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_end(st: u64) {
    let state = Box::from_raw(st as usize as *mut RowsState);
    SPARE.with(|s| s.set(Some(state)));
}

/// Steps to the next row: 1, or 0 when there is none.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_next(st: u64) -> u8 {
    state(st).cursor.step() as u8
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_columns(st: u64) -> u64 {
    state(st).fields.len() as u64
}

/// The field column `col` fills, or -1.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_field(st: u64, col: u64) -> i64 {
    state(st).fields[col as usize] as i64
}

/// The fields of the first 16 columns, 4 bits each (column `c` at bit `4 * c`): the field's
/// index plus one, 0 for none or for a field index of 15 or more ([`velt_rt_rows_field`] gives
/// those).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_field_map(st: u64) -> u64 {
    field_map(&state(st).fields)
}

fn field_map(fields: &[i32]) -> u64 {
    let mut map = 0u64;
    for (c, &f) in fields.iter().take(16).enumerate() {
        if (0..15).contains(&f) {
            map |= ((f + 1) as u64) << (4 * c);
        }
    }
    map
}

unsafe fn cell<'a>(st: u64, col: u64) -> Cell<'a> {
    state(st).cursor.cell(col as usize)
}

/// A float that is an integer exactly (as the JSON reader takes `3.0` for an integer): 1 and
/// the integer, else 0.
///
/// # Safety
/// `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_exact_i64(v: f64, out: *mut i64) -> u8 {
    if v.fract() == 0.0 && (-I64_LIMIT..I64_LIMIT).contains(&v) {
        out.write(v as i64);
        OK
    } else {
        MISMATCH
    }
}

/// An integer, or a float that is one exactly (as the JSON reader takes `3.0`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_i64(st: u64, col: u64, out: *mut i64) -> u8 {
    match cell(st, col) {
        Cell::Int(v) => {
            out.write(v);
            OK
        }
        Cell::Float(v) if v.fract() == 0.0 && (-I64_LIMIT..I64_LIMIT).contains(&v) => {
            out.write(v as i64);
            OK
        }
        Cell::Null => NULL,
        _ => MISMATCH,
    }
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_f64(st: u64, col: u64, out: *mut f64) -> u8 {
    match cell(st, col) {
        Cell::Int(v) => {
            out.write(v as f64);
            OK
        }
        Cell::Float(v) => {
            out.write(v);
            OK
        }
        Cell::Null => NULL,
        _ => MISMATCH,
    }
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_bool(st: u64, col: u64, out: *mut u8) -> u8 {
    match cell(st, col) {
        Cell::Bool(v) => {
            out.write(v as u8);
            OK
        }
        Cell::Null => NULL,
        _ => MISMATCH,
    }
}

/// Text as a string (not UTF-8: U+FFFD).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_str(st: u64, col: u64, out: *mut VeltStr) -> u8 {
    let t = match cell(st, col) {
        Cell::Text(t) | Cell::Ascii(t) => t,
        Cell::Null => return NULL,
        _ => return MISMATCH,
    };
    out.write(if t.is_empty() {
        VeltStr::empty()
    } else if t.is_ascii() {
        // SAFETY: ASCII is UTF-8 with one unit per byte.
        VeltStr::from_text_counted(std::str::from_utf8_unchecked(t), t.len())
    } else {
        match std::str::from_utf8(t) {
            Ok(text) => VeltStr::from_text(text),
            Err(_) => VeltStr::from_text(&String::from_utf8_lossy(t)),
        }
    });
    OK
}

/// What column `col` of the current row holds, numbered as std/db's `CellKind`: 0 NULL, 1 a
/// boolean, 2 an integer, 3 a float, 4 text, 5 a blob, 6 a nested value (JSON).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_kind(st: u64, col: u64) -> u8 {
    match cell(st, col) {
        Cell::Null => 0,
        Cell::Bool(_) => 1,
        Cell::Int(_) => 2,
        Cell::Float(_) => 3,
        Cell::Text(_) | Cell::Ascii(_) => 4,
        Cell::Blob(_) => 5,
        Cell::Json(_) => 6,
    }
}

/// Column `col` of the current row as a `u8[]`: a blob's bytes, else empty.
///
/// # Safety
/// As the module says; `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_blob(st: u64, col: u64, out: *mut VeltBytes) {
    let bytes = match cell(st, col) {
        Cell::Blob(b) => b.to_vec(),
        _ => Vec::new(),
    };
    out.write(VeltBytes::from_vec(bytes));
}

/// The JSON decoder's message for a row that does not fit: `expected <expected> at $[row].field`
/// (`row` -1: the single row, `$`; `field` empty: the row itself).
///
/// # Safety
/// The strings are valid; `out` is writable.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_fail(
    expected: *const VeltStr,
    row: i64,
    field: *const VeltStr,
    out: *mut VeltStr,
) {
    let mut path = String::from("$");
    if row >= 0 {
        path.push_str(&format!("[{row}]"));
    }
    let field = (*field).text_lossy();
    if !field.is_empty() {
        path.push('.');
        path.push_str(&field);
    }
    let msg = crate::json::error::mismatch_message(&(*expected).text_lossy(), &path);
    out.write(VeltStr::from_vec(msg.into_bytes()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_field_map_holds_the_first_16_columns_fields_below_15() {
        let map = field_map(&[2, -1, 0, 14, 15]);
        let at = |c: u64| ((map >> (4 * c)) & 15) as i64 - 1;
        assert_eq!([at(0), at(1), at(2), at(3), at(4)], [2, -1, 0, 14, -1]);
        let wide: Vec<i32> = (0..20).map(|c| c % 3).collect();
        assert_eq!((field_map(&wide) >> 60) & 15, 1);
    }

    #[test]
    fn field_names_are_read_by_their_lengths() {
        let names = named_fields("2:id4:na:e0:".as_bytes());
        assert_eq!(names, vec![&b"id"[..], &b"na:e"[..], &b""[..]]);
    }
}
