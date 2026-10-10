//! Weak references without a collector (#823, #11): the runtime core under `WeakMap`, `WeakSet`,
//! `WeakRef` and `weak T`. Design: docs/internals/design/weak-refs.md; proposed ABI:
//! docs/internals/contracts/rt_abi.md "Weak references (proposed)".
//!
//! A counted object (`[count: u64][value]`, the value pointer is the block + 8) that is weakly
//! held has [`RC_WEAK`] set in its count word and a record in this thread's side table
//! (`table`). Retain stays `count += 1`. [`RC_WEAK`] is the sign bit, so release, for the types
//! the compiler marks weak-capable, keeps today's single compare on the shared path (signed
//! instead of `== 1`) and tells the unique and the weakly held case apart off it:
//!
//! ```text
//! c = *rc
//! if c as i64 > 1 { *rc = c - 1 }                                      // shared path
//! else if c == 1  { drop the fields; free }                            // unique path
//! else            { if weak_release(obj) { drop the fields; free } }   // RC_WEAK set: cold
//! ```
//!
//! Types that are never weakly held keep today's two-way release. `weak_release` removes a dying
//! object from every map and `WeakRef`, and, on a decrement that may leave the object referenced
//! only by its own entries' values (the `raw -> proxy` ephemeron), runs a bounded trial deletion
//! (`trial`) that frees the cycle when nothing outside refers to it.
//!
//! Nothing here is called by generated code yet: the functions are exported with the proposed
//! ABI, for the compiler to lower `WeakMap`/`WeakSet`/`WeakRef` to.

mod table;
mod trial;

#[cfg(test)]
mod tests;

use std::ffi::c_void;

pub use table::{MapId, RefId};

/// Count word: the object is weakly held (a weak key, a `WeakRef` target or a member of an
/// ephemeron cycle) and has a record in the side table. Counts never reach 2^63.
pub const RC_WEAK: u64 = 1 << 63;

/// Count word: the count proper.
pub const RC_COUNT: u64 = RC_WEAK - 1;

/// Trace glue of a counted type: calls `visit(ctx, child, child_trace)` once for every strong
/// reference `obj` holds to a counted object (through inline structs, arrays and closures'
/// captures), with the child's own trace glue (`None`: the child is opaque). A plain function
/// pointer in the ABI (the wrapper only breaks the type recursion with [`VisitFn`]).
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct TraceFn(pub unsafe extern "C" fn(obj: *mut u8, visit: VisitFn, ctx: *mut c_void));

/// The callback trace glue calls for each strong reference.
pub type VisitFn = unsafe extern "C" fn(ctx: *mut c_void, child: *mut u8, trace: Option<TraceFn>);

/// Releases one reference to a map value (the value's full release sequence).
pub type ReleaseFn = unsafe extern "C" fn(value: u64);

/// Retains a map value (the value's retain: `count += 1`, or its type's own sequence).
pub type RetainFn = unsafe extern "C" fn(value: u64);

/// The count word of the counted object `obj`.
///
/// # Safety
/// `obj` is the value pointer of a live counted object.
#[inline]
pub(crate) unsafe fn rc_word(obj: *mut u8) -> *mut u64 {
    (obj as *mut u64).sub(1)
}

/// A new, empty weak map: `key_trace` is the keys' trace glue, `value_retain` and
/// `value_release` retain and release a value (both `None`: values are plain words, as for
/// `WeakMap<K, number>` and `WeakSet`), `value_trace` traces a value (`None`: values never refer
/// back to keys, so entries are never ephemerons). A value word of a map with `value_release` is
/// 0 (`null`, `undefined`) or a counted object pointer: the compiler boxes strings and unions.
#[no_mangle]
pub extern "C" fn velt_rt_weakmap_new(
    key_trace: Option<TraceFn>,
    value_retain: Option<RetainFn>,
    value_release: Option<ReleaseFn>,
    value_trace: Option<TraceFn>,
) -> MapId {
    assert!(
        value_retain.is_some() == value_release.is_some()
            && (value_trace.is_none() || value_release.is_some()),
        "ICE: weak map values need both retain and release glue, and trace glue only with them"
    );
    table::with(|s| s.new_map(key_trace, value_retain, value_release, value_trace))
}

/// `map.set(key, value)`: takes over the caller's reference to `value`; the key is not counted.
/// A previous value for `key` is released.
///
/// # Safety
/// `map` is live; `key` is a live counted object; `value` is owned by the caller.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_weakmap_set(map: MapId, key: *mut u8, value: u64) {
    let old = table::with(|s| s.set(map, key, value));
    table::release_values(old);
}

/// `map.get(key)`: the value, counted (retained with the map's `value_retain`, like
/// [`velt_rt_weakref_deref`]; the caller releases it), and whether there was one in `*found`.
/// Counted because any later release of a weakly held object may run a trial that deletes the
/// entry and frees its value.
///
/// # Safety
/// `map` is live; `found` is writable.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_weakmap_get(map: MapId, key: *mut u8, found: *mut u8) -> u64 {
    let (v, retain) = table::with(|s| (s.get(map, key), s.map(map).value_retain));
    *found = u8::from(v.is_some());
    let v = v.unwrap_or(0);
    if let (Some(retain), true) = (retain, v != 0) {
        retain(v);
    }
    v
}

/// `map.has(key)`.
#[no_mangle]
pub extern "C" fn velt_rt_weakmap_has(map: MapId, key: *mut u8) -> u8 {
    u8::from(table::with(|s| s.get(map, key)).is_some())
}

/// `map.delete(key)`: whether there was an entry; its value is released.
///
/// # Safety
/// `map` is live.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_weakmap_delete(map: MapId, key: *mut u8) -> u8 {
    let old = table::with(|s| s.delete(map, key));
    let found = u8::from(old.is_some());
    table::release_values(old.unwrap_or_default());
    found
}

/// Drops `map`: every value is released.
///
/// # Safety
/// `map` is live and not used again.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_weakmap_drop(map: MapId) {
    let old = table::with(|s| s.drop_map(map));
    table::release_values(old);
}

/// The number of entries in `map` (tests and `--inspect`; JavaScript has no `WeakMap.size`).
pub fn weakmap_len(map: MapId) -> u64 {
    table::with(|s| s.map_len(map)) as u64
}

/// `new WeakRef(obj)`: does not count `obj`.
///
/// # Safety
/// `obj` is a live counted object.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_weakref_new(obj: *mut u8) -> RefId {
    table::with(|s| s.new_ref(obj))
}

/// `ref.deref()`: the target with its count raised by one, or null once it was freed.
#[no_mangle]
pub extern "C" fn velt_rt_weakref_deref(r: RefId) -> *mut u8 {
    table::with(|s| s.deref(r))
}

/// Drops `r`.
///
/// # Safety
/// `r` is live and not used again.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_weakref_drop(r: RefId) {
    table::with(|s| s.drop_ref(r))
}

/// The cold release path of a weakly held object (`*rc_word(obj) & RC_WEAK` set), called with
/// the caller's reference. Returns 1 when that was the last reference: the object has left every
/// map and `WeakRef`, and the caller drops its fields and frees it as on the unique path.
/// Returns 0 when the count was decremented instead (which may have freed an ephemeron cycle
/// the object belonged to, but never the object while the caller's code still runs on it: a
/// count above 1 means another reference, and a cycle's last outside reference is never the
/// one being released by the code that holds it).
///
/// Weakly held objects are thread-bound: one released on a thread whose side table has no
/// record of it is an internal compiler error (the compiler keeps weak-capable types off other
/// threads), reported before the count word changes.
///
/// # Safety
/// `obj` is a live counted object with [`RC_WEAK`] set, and the caller owns one reference.
#[cold]
#[no_mangle]
pub unsafe extern "C" fn velt_rt_weak_release(obj: *mut u8) -> u8 {
    u8::from(weak_release(obj))
}

/// [`velt_rt_weak_release`], unwinding on an internal error (for tests).
///
/// # Safety
/// As for [`velt_rt_weak_release`].
#[inline]
pub(crate) unsafe fn weak_release(obj: *mut u8) -> bool {
    let rc = rc_word(obj);
    debug_assert!(
        *rc & RC_COUNT >= 1,
        "ICE: release of a weakly held object with count 0"
    );
    if *rc & RC_COUNT == 1 {
        let values = table::with(|s| s.forget(obj)).unwrap_or_else(|| not_held_here(obj));
        *rc = 1;
        table::release_values(values);
        return true;
    }
    table::decrement(obj);
    false
}

/// A weakly held object released on a thread with no record of it.
#[cold]
pub(crate) fn not_held_here(obj: *mut u8) -> ! {
    panic!(
        "ICE: weakly held object {obj:p} released on a thread that does not hold it          (weak-capable objects are thread-bound)"
    )
}

/// The number of objects in this thread's side table (tests and leak checks).
pub fn tracked_objects() -> usize {
    table::with(|s| s.tracked())
}
