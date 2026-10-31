//! Rows read straight into typed values (`Glue::RowsRead`, VIR json/rows_read.rs): what std's
//! `JSON.parse` of a driver's row cursor (`jsonParseCursor`) compiles to when the
//! row type is an object of plain fields. Each column is matched to its field once per result
//! ([`velt_rt_rows_begin`]), and each value is read by column, typed, without the JSON reader's
//! keys. The results, and the messages of a row that does not fit, are those of the JSON decoder.
//!
//! # Safety
//! For every function: `state` came from [`velt_rt_rows_begin`] and is not freed yet; `out`
//! points at an all-zero value of the type written; `col` is below the column count.

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

/// A result being read: the driver's cursor (none for a `RowSource` written in Velt, which the
/// compiled code reads itself) and each column's field.
pub struct RowsState {
    cursor: Option<RawCursor>,
    /// The field each column fills (-1: none).
    fields: Vec<i32>,
    /// What `fields` was made for: the field table's address and the rows header. The next
    /// result with both the same (a statement run again) reuses it.
    table: usize,
    header: Vec<u8>,
}

thread_local! {
    /// The state of this thread's last result, for the next (each read is synchronous: one at a
    /// time per thread, but a state is taken out while in use).
    static SPARE: std::cell::Cell<Option<Box<RowsState>>> = const { std::cell::Cell::new(None) };
}

/// Starts reading the rows of `cursor` (a driver's `RawCursor`, kept by the caller until
/// [`velt_rt_rows_end`]) into objects whose fields are named in `fields` (static data: a `u32`
/// count, then per field a `u32` byte length and the name).
///
/// # Safety
/// As the module says; `fields` is such a table.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_begin(
    cursor: *const RawCursor,
    fields: *const u8,
) -> *mut RowsState {
    let cursor = RawCursor {
        data: (*cursor).data,
        vtable: (*cursor).vtable,
    };
    let header = cursor.header();
    let mut state = match SPARE.with(|s| s.take()) {
        Some(mut s) => {
            s.cursor = Some(cursor);
            s
        }
        None => Box::new(RowsState {
            cursor: Some(cursor),
            fields: Vec::new(),
            table: 0,
            header: Vec::new(),
        }),
    };
    if state.table != fields as usize || state.header != header {
        map_columns(&mut state, header, fields);
    }
    Box::into_raw(state)
}

/// Each column's field, by name (a repeated name: each column of it fills the field).
unsafe fn map_columns(state: &mut RowsState, header: &[u8], fields: *const u8) {
    let columns = header_end(header).map_or(0, |(_, n, _)| n);
    let names = field_names(fields);
    state.fields.clear();
    let mut at = NAMES;
    for _ in 0..columns {
        let (start, end, _) = name_at(header, at);
        at = end;
        let name = &header[start..end];
        let field = names
            .iter()
            .position(|n| *n == name)
            .map_or(-1, |i| i as i32);
        state.fields.push(field);
    }
    state.table = fields as usize;
    state.header.clear();
    state.header.extend_from_slice(header);
}

unsafe fn field_names<'a>(table: *const u8) -> Vec<&'a [u8]> {
    let u32_at = |p: *const u8| u32::from_le_bytes(std::ptr::read_unaligned(p as *const [u8; 4]));
    let count = u32_at(table) as usize;
    let mut p = table.add(4);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let len = u32_at(p) as usize;
        out.push(std::slice::from_raw_parts(p.add(4), len));
        p = p.add(4 + len);
    }
    out
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_end(state: *mut RowsState) {
    let state = Box::from_raw(state);
    SPARE.with(|s| s.set(Some(state)));
}

/// Steps to the next row: 1, or 0 when there is none.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_next(state: *mut RowsState) -> u8 {
    (*state).cursor.as_ref().is_some_and(|c| c.step()) as u8
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_columns(state: *const RowsState) -> u64 {
    (*state).fields.len() as u64
}

/// The field column `col` fills, or -1.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_field(state: *const RowsState, col: u64) -> i64 {
    (&(*state).fields)[col as usize] as i64
}

unsafe fn cell<'a>(state: *const RowsState, col: u64) -> Cell<'a> {
    match &(*state).cursor {
        Some(c) => c.cell(col as usize),
        None => Cell::Null,
    }
}

/// Starts reading a `RowSource` written in Velt (std/db): a state without a cursor, whose
/// column map [`velt_rt_rows_map_column`] fills from the source's column names.
///
/// # Safety
/// `fields` is a field table (see [`velt_rt_rows_begin`]).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_map(fields: *const u8) -> *mut RowsState {
    let mut state = SPARE.with(|s| s.take()).unwrap_or_else(|| {
        Box::new(RowsState {
            cursor: None,
            fields: Vec::new(),
            table: 0,
            header: Vec::new(),
        })
    });
    state.cursor = None;
    state.fields.clear();
    // The map is the source's, not a header's: not kept for the next result.
    state.table = fields as usize;
    state.header.clear();
    state.header.push(0xFF);
    Box::into_raw(state)
}

/// The next column of a [`velt_rt_rows_map`] state is named `name`.
///
/// # Safety
/// As the module says; `name` is a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_map_column(state: *mut RowsState, name: *const VeltStr) {
    let state = &mut *state;
    let name = (*name).text_lossy();
    let field = field_names(state.table as *const u8)
        .iter()
        .position(|n| *n == name.as_bytes())
        .map_or(-1, |i| i as i32);
    state.fields.push(field);
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
pub unsafe extern "C" fn velt_rt_rows_i64(state: *const RowsState, col: u64, out: *mut i64) -> u8 {
    match cell(state, col) {
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
pub unsafe extern "C" fn velt_rt_rows_f64(state: *const RowsState, col: u64, out: *mut f64) -> u8 {
    match cell(state, col) {
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
pub unsafe extern "C" fn velt_rt_rows_bool(state: *const RowsState, col: u64, out: *mut u8) -> u8 {
    match cell(state, col) {
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
pub unsafe extern "C" fn velt_rt_rows_str(
    state: *const RowsState,
    col: u64,
    out: *mut VeltStr,
) -> u8 {
    let t = match cell(state, col) {
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

/// A blob as a `u8[]`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_rows_bytes(
    state: *const RowsState,
    col: u64,
    out: *mut VeltBytes,
) -> u8 {
    match cell(state, col) {
        Cell::Blob(b) => {
            out.write(VeltBytes::from_vec(b.to_vec()));
            OK
        }
        Cell::Null => NULL,
        _ => MISMATCH,
    }
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
