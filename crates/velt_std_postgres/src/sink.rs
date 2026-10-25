//! A statement's parameter sink (`velt_text::rows_cursor::ParamSink`, native_abi.md "Parameter
//! sinks"): std's generated code hands it the parameters value by value, and it keeps them as
//! the parameters JSON text would have parsed to (`velt_text::db_json::Params`), which the
//! statement then binds as before: no JSON text is made or parsed.

use std::borrow::Cow;
use velt_text::db_json::{DbValue, Params};
use velt_text::rows_cursor::{Cell, ParamSink, RawSink};

#[derive(Default)]
struct PgSink {
    named: bool,
    name: String,
    values: Vec<DbValue<'static>>,
    members: Vec<(Cow<'static, str>, DbValue<'static>)>,
}

/// What std holds: the sink's ABI form first (its address is the handle), then the sink.
#[repr(C)]
struct Owned {
    raw: RawSink,
    sink: PgSink,
}

impl ParamSink for PgSink {
    fn object(&mut self) {
        self.named = true;
    }

    fn name(&mut self, name: &[u8]) {
        self.name = String::from_utf8_lossy(name).into_owned();
    }

    fn value(&mut self, v: Cell<'_>) {
        let text = |t: &[u8]| DbValue::Text(Cow::Owned(String::from_utf8_lossy(t).into_owned()));
        let v = match v {
            Cell::Null => DbValue::Null,
            Cell::Bool(b) => DbValue::Bool(b),
            Cell::Int(i) => DbValue::Int(i),
            Cell::Float(f) => DbValue::Float(f),
            Cell::Text(t) | Cell::Ascii(t) | Cell::Json(t) => text(t),
            Cell::Blob(b) => DbValue::Bytes(b.to_vec()),
        };
        match self.named {
            true => self
                .members
                .push((Cow::Owned(std::mem::take(&mut self.name)), v)),
            false => self.values.push(v),
        }
    }
}

/// A new sink: its handle for std.
pub fn begin() -> u64 {
    let owned = Box::into_raw(Box::new(Owned {
        // Set below, once the sink has its address.
        raw: RawSink::new(std::ptr::null_mut::<PgSink>()),
        sink: PgSink::default(),
    }));
    // SAFETY: just made; the sink stays in the box until std hands it on.
    unsafe { (*owned).raw = RawSink::new(&mut (*owned).sink) };
    owned as u64
}

/// The parameters the sink of `handle` took (and the sink released).
///
/// # Safety
/// `handle` came from [`begin`] and is not used afterwards.
pub unsafe fn take(handle: u64) -> Params<'static> {
    let sink = Box::from_raw(handle as *mut Owned).sink;
    match sink.named {
        true => Params::Named(sink.members),
        false => Params::Positional(sink.values),
    }
}
