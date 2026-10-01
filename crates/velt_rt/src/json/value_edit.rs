//! Building and editing `json.Value`s (`velt_rt_json_value_new_*`, `set`, `delete`, `push`,
//! `set_at`). Values keep value semantics: a handle is a reference to a node that other handles
//! (clones, parents, children handed out by `get`/`at`) may share, so an edit first copies the
//! node unless this handle holds its only reference (copy-on-write, one level: children stay
//! shared). Editors take a pointer to the handle slot, which may get a new node.

use super::value::{Object, Value};
use super::value_abi::ValueHandle;
use crate::handle::Handle;
use crate::str::VeltStr;
use std::sync::Arc;

fn new(v: Value) -> ValueHandle {
    Handle::from_arc(Arc::new(v))
}

/// The node behind `*slot`, ready to change (copied first if shared). `None` for a null handle.
///
/// # Safety
/// `slot` must point to a live handle (or null) that nothing else uses during the edit.
unsafe fn edit<'a>(slot: *mut ValueHandle) -> Option<&'a mut Value> {
    let h = *slot;
    if h.is_null() {
        return None;
    }
    let mut arc = Arc::from_raw(h.ptr());
    if Arc::get_mut(&mut arc).is_none() {
        arc = Arc::new(arc.shallow_clone());
    }
    let p = Arc::into_raw(arc);
    *slot = Handle::from_ptr(p);
    // The slot owns the only reference now.
    Some(&mut *(p as *mut Value))
}

/// `null`.
#[no_mangle]
pub extern "C" fn velt_rt_json_value_new_null() -> ValueHandle {
    new(Value::Null)
}

/// `true` / `false` (`b != 0`).
#[no_mangle]
pub extern "C" fn velt_rt_json_value_new_bool(b: u8) -> ValueHandle {
    new(Value::Bool(b != 0))
}

/// A number.
#[no_mangle]
pub extern "C" fn velt_rt_json_value_new_number(n: f64) -> ValueHandle {
    new(Value::Number(n))
}

/// A string (copied).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_new_string(s: *const VeltStr) -> ValueHandle {
    let text = String::from_utf8_lossy((*s).as_bytes()).into_owned();
    new(Value::String(text.into_boxed_str()))
}

/// An empty array.
#[no_mangle]
pub extern "C" fn velt_rt_json_value_new_array() -> ValueHandle {
    new(Value::Array(Vec::new()))
}

/// An empty object.
#[no_mangle]
pub extern "C" fn velt_rt_json_value_new_object() -> ValueHandle {
    new(Value::Object(Object::default()))
}

/// `obj[key] = value` on the object at `*slot` (an existing key keeps its position): 1 = done,
/// 0 = not an object. `value` is shared, not consumed (null = JSON `null`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_set(
    slot: *mut ValueHandle,
    key: *const VeltStr,
    value: ValueHandle,
) -> u8 {
    if !matches!((*slot).get(), Some(Value::Object(_))) {
        return 0;
    }
    let child = shared(value);
    let Some(Value::Object(obj)) = edit(slot) else {
        return 0;
    };
    let key = String::from_utf8_lossy((*key).as_bytes()).into_owned();
    obj.insert(key.into_boxed_str(), child);
    1
}

/// Remove `key` from the object at `*slot`: 1 = removed, 0 = absent or not an object.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_delete(
    slot: *mut ValueHandle,
    key: *const VeltStr,
) -> u8 {
    let key = String::from_utf8_lossy((*key).as_bytes());
    match (*slot).get() {
        Some(Value::Object(obj)) if obj.find(&key).is_some() => {}
        _ => return 0,
    }
    match edit(slot) {
        Some(Value::Object(obj)) => obj.remove(&key) as u8,
        _ => 0,
    }
}

/// Append `value` to the array at `*slot`: 1 = done, 0 = not an array.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_push(slot: *mut ValueHandle, value: ValueHandle) -> u8 {
    if !matches!((*slot).get(), Some(Value::Array(_))) {
        return 0;
    }
    let child = shared(value);
    match edit(slot) {
        Some(Value::Array(items)) => {
            items.push(child);
            1
        }
        _ => 0,
    }
}

/// `arr[i] = value` for an existing element of the array at `*slot`: 1 = done, 0 = not an
/// array or `i` out of range.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_json_value_set_at(
    slot: *mut ValueHandle,
    i: u64,
    value: ValueHandle,
) -> u8 {
    match (*slot).get() {
        Some(Value::Array(items)) if (i as usize) < items.len() => {}
        _ => return 0,
    }
    let child = shared(value);
    match edit(slot) {
        Some(Value::Array(items)) => {
            items[i as usize] = child;
            1
        }
        _ => 0,
    }
}

/// A new reference to `value`'s node (a null handle becomes JSON `null`).
unsafe fn shared(value: ValueHandle) -> Arc<Value> {
    if value.is_null() {
        Arc::new(Value::Null)
    } else {
        value.clone_arc()
    }
}
