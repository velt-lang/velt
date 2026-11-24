//! What the row readers velt/db's generators write (velt/db/generate/columns.velt) call besides a
//! `RowSource`'s own methods: the integer a float stands for exactly, and the JSON decoder's
//! message for a row that does not fit.

use crate::str::VeltStr;

/// 2^63 as a float: integral floats in `[-2^63, 2^63)` are exact `i64`s.
const I64_LIMIT: f64 = 9_223_372_036_854_775_808.0;

/// A read's result: the value was written, or the value is of another kind.
const MISMATCH: u8 = 0;
const OK: u8 = 1;

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
