//! C accessors for `json.Value` handles (`velt_rt_json_parse_value`, `velt_rt_json_value_*`).
//!
//! A handle is an `Arc<Value>` turned into a raw pointer, passed as a `u64` ([`Handle`]: the
//! prelude's `JsonValue.handle` field and the declarations in `std/prelude/json.vlt`; 0 = null).
//! Every handle returned by the runtime
//! (parse, `get`, `at`, `clone`) is a separate reference that must be released with
//! `velt_rt_json_value_free`; freeing a parent never invalidates handles to its children.
//! All accessors accept a null handle (the result of a failed `get`/`at`).

use super::error::syntax_message;
use super::value::{parse, stringify_into, Value};
use crate::handle::Handle;
use crate::str::VeltStr;
use std::sync::Arc;

/// A `json.Value` handle (0 = null).
pub type ValueHandle = Handle<Value>;

/// `value_kind` results.
pub const KIND_NONE: u32 = 0;
pub const KIND_NULL: u32 = 1;
pub const KIND_BOOL: u32 = 2;
pub const KIND_NUMBER: u32 = 3;
pub const KIND_STRING: u32 = 4;
pub const KIND_ARRAY: u32 = 5;
pub const KIND_OBJECT: u32 = 6;

/// A position given by generated code as `u64`, as a `usize` if it is one. A cast would truncate
/// on 32-bit targets (wasm32), turning `at(4294967296)` into `at(0)`; a position beyond `usize`
/// is out of range of every array. Generic over the target so the 32-bit case can be tested on
/// any host.
pub fn to_index<U: TryFrom<u64>>(i: u64) -> Option<U> {
    U::try_from(i).ok()
}

fn new_handle(v: &Arc<Value>) -> ValueHandle {
    Handle::from_arc(Arc::clone(v))
}

/// `JSON.parseValue(src)` without a depth limit (`JsonValue.from`, whose text the compiler
/// wrote): 1 = ok (`*out_handle` set); 0 = syntax error (`*out_handle` = null, `*out_err` =
/// owned message `invalid JSON at <path>: <detail> (byte <offset>)`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_parse_value(
    src: *const VeltStr,
    out_handle: *mut ValueHandle,
    out_err: *mut VeltStr,
) -> u8 {
    velt_rt_json_parse_value_with(src, 0, out_handle, out_err)
}

/// `JSON.parseValue(src, { maxDepth })`: like `velt_rt_json_parse_value`, failing on arrays and
/// objects nested more than `max_depth` deep (0 = no limit) with
/// `JSON nested deeper than <max_depth> levels at <path> (byte <offset>)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_parse_value_with(
    src: *const VeltStr,
    max_depth: u32,
    out_handle: *mut ValueHandle,
    out_err: *mut VeltStr,
) -> u8 {
    let bytes = (*src).as_bytes();
    let limit = if max_depth == 0 {
        usize::MAX
    } else {
        max_depth as usize
    };
    match parse(bytes, limit) {
        Ok(root) => {
            out_handle.write(Handle::from_arc(root));
            1
        }
        Err((e, path)) => {
            out_handle.write(Handle::NULL);
            out_err.write(VeltStr::from_vec(
                syntax_message(bytes, e, &path, limit).into_bytes(),
            ));
            0
        }
    }
}

/// Kind of the value: 0 none (null handle), 1 null, 2 bool, 3 number, 4 string, 5 array, 6 object.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_kind(h: ValueHandle) -> u32 {
    match h.get() {
        None => KIND_NONE,
        Some(Value::Null) => KIND_NULL,
        Some(Value::Bool(_)) => KIND_BOOL,
        Some(Value::Number(_)) => KIND_NUMBER,
        Some(Value::String(_)) => KIND_STRING,
        Some(Value::Array(_)) => KIND_ARRAY,
        Some(Value::Object(_)) => KIND_OBJECT,
    }
}

/// `v.get(key)`: new handle to the member, or null if `h` is not an object or lacks `key`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_get(
    h: ValueHandle,
    key: *const VeltStr,
) -> ValueHandle {
    let Some(Value::Object(obj)) = h.get() else {
        return Handle::NULL;
    };
    let Ok(key) = std::str::from_utf8((*key).as_bytes()) else {
        return Handle::NULL;
    };
    match obj.find(key) {
        Some(i) => new_handle(&obj.entries[i].1),
        None => Handle::NULL,
    }
}

/// `v.at(i)`: new handle to array element `i` (or the `i`-th member value of an object), or null.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_at(h: ValueHandle, i: u64) -> ValueHandle {
    let Some(i) = to_index::<usize>(i) else {
        return Handle::NULL;
    };
    let child = match h.get() {
        Some(Value::Array(items)) => items.get(i),
        Some(Value::Object(obj)) => obj.entries.get(i).map(|(_, v)| v),
        _ => None,
    };
    child.map_or(Handle::NULL, new_handle)
}

/// Key of the `i`-th object member (owned copy): 1 = ok, 0 = not an object / out of range
/// (`out` untouched).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_key_at(
    h: ValueHandle,
    i: u64,
    out: *mut VeltStr,
) -> u8 {
    let Some(Value::Object(obj)) = h.get() else {
        return 0;
    };
    match to_index::<usize>(i).and_then(|i| obj.entries.get(i)) {
        Some((key, _)) => {
            out.write(VeltStr::from_vec(key.as_bytes().to_vec()));
            1
        }
        None => 0,
    }
}

/// Array length, object member count, or string byte length; 0 for everything else.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_len(h: ValueHandle) -> u64 {
    match h.get() {
        Some(Value::Array(items)) => items.len() as u64,
        Some(Value::Object(obj)) => obj.entries.len() as u64,
        Some(Value::String(s)) => s.len() as u64,
        _ => 0,
    }
}

/// The number, or NaN if the value is not a number.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_as_f64(h: ValueHandle) -> f64 {
    match h.get() {
        Some(Value::Number(n)) => *n,
        _ => f64::NAN,
    }
}

/// 1 for `true`, 0 otherwise (check `value_kind` to tell `false` from non-booleans).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_as_bool(h: ValueHandle) -> u8 {
    matches!(h.get(), Some(Value::Bool(true))) as u8
}

/// Owned copy of a string value: 1 = ok, 0 = not a string (`out` untouched).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_as_str(h: ValueHandle, out: *mut VeltStr) -> u8 {
    match h.get() {
        Some(Value::String(s)) => {
            out.write(VeltStr::from_vec(s.as_bytes().to_vec()));
            1
        }
        _ => 0,
    }
}

/// `JSON.stringify(v)` into a new owned string (`null` for a null handle).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_stringify(h: ValueHandle, out: *mut VeltStr) {
    let mut buf = Vec::new();
    match h.get() {
        Some(v) => stringify_into(&mut buf, v),
        None => buf.extend_from_slice(b"null"),
    }
    out.write(VeltStr::from_vec(buf));
}

/// Another handle to the same (immutable) value: O(1), no deep copy. Null stays null.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_clone(h: ValueHandle) -> ValueHandle {
    if !h.is_null() {
        Arc::increment_strong_count(h.ptr());
    }
    h
}

/// Release a handle (null is a no-op). The tree is freed when its last handle goes.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_free(h: ValueHandle) {
    h.release();
}
