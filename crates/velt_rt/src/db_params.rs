//! A statement's parameters handed to a driver's sink (`velt_text::rows_cursor::ParamSink`) by
//! the code `__intrinsic_db_bind<P>` generates for the parameter type: one call per member name
//! and value, instead of JSON text the driver parses (native_abi.md "Parameter sinks"). Each
//! value binds as its JSON text would: an integral number below 1e21 as an integer (when it
//! fits), NaN and the infinities as null, a string as UTF-8 (a lone surrogate as U+FFFD).
//!
//! # Safety
//! For every function: `sink` is the address of a live `RawSink` that the driver lent std for
//! this statement; strings and arrays are valid for the call.

use crate::bytes::VeltBytes;
use crate::str::VeltStr;
use velt_text::rows_cursor::{Cell, RawSink};

/// 2^63 as a float: integral floats in `[-2^63, 2^63)` are exact `i64`s.
const I64_LIMIT: f64 = 9_223_372_036_854_775_808.0;

/// Below this an integral number's JSON text is its digits (JavaScript's `Number#toString`).
const DIGITS_LIMIT: f64 = 1e21;

/// The parameters are an object's members.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_param_object(sink: *const RawSink) {
    (*sink).object();
}

/// The next value is the object member `name`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_param_name(sink: *const RawSink, name: *const VeltStr) {
    (*sink).name((*name).text_lossy().as_bytes());
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_param_i64(sink: *const RawSink, v: i64) {
    (*sink).value(Cell::Int(v));
}

/// Beyond `i64`, a float (as its JSON digits read back).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_param_u64(sink: *const RawSink, v: u64) {
    (*sink).value(match i64::try_from(v) {
        Ok(i) => Cell::Int(i),
        Err(_) => Cell::Float(v as f64),
    });
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_param_f64(sink: *const RawSink, v: f64) {
    (*sink).value(number(v));
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_param_bool(sink: *const RawSink, v: u8) {
    (*sink).value(Cell::Bool(v != 0));
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_param_null(sink: *const RawSink) {
    (*sink).value(Cell::Null);
}

#[no_mangle]
pub unsafe extern "C" fn velt_rt_param_str(sink: *const RawSink, s: *const VeltStr) {
    (*sink).value(Cell::Text((*s).text_lossy().as_bytes()));
}

/// A `u8[]`: a blob.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_param_bytes(sink: *const RawSink, b: *const VeltBytes) {
    (*sink).value(Cell::Blob((*b).as_bytes()));
}

/// How a number's JSON text reads back as a parameter.
fn number(v: f64) -> Cell<'static> {
    if !v.is_finite() {
        Cell::Null
    } else if v.fract() == 0.0 && v.abs() < DIGITS_LIMIT && (-I64_LIMIT..I64_LIMIT).contains(&v) {
        Cell::Int(v as i64)
    } else {
        Cell::Float(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_text::db_json::{parse_params, DbValue, Params};

    /// What the JSON path binds for `v`.
    fn via_json(v: f64) -> Cell<'static> {
        let mut text = Vec::new();
        match v.is_finite() {
            true => crate::fmt::push_f64(&mut text, v),
            false => text.extend_from_slice(b"null"),
        }
        match parse_params(&text).unwrap() {
            Params::Positional(vals) => match vals[0] {
                DbValue::Int(i) => Cell::Int(i),
                DbValue::Float(f) => Cell::Float(f),
                DbValue::Null => Cell::Null,
                ref other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn numbers_bind_as_their_json_text_does() {
        for v in [
            0.0,
            -0.0,
            5.0,
            -5.0,
            1.5,
            1e20,
            1e21,
            9.3e18,
            -9.3e18,
            2f64.powi(63),
            f64::NAN,
            f64::INFINITY,
            1e-7,
            123456789.0,
        ] {
            assert_eq!(number(v), via_json(v), "{v}");
        }
    }
}
