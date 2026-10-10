//! Tests of the weak-reference core with hand-built counted objects: [`Obj`] stands for any
//! counted type (a raw object, a proxy cell holding its target, a handler), and [`release`] is
//! the release sequence the compiler would emit for a weak-capable type.

mod bench;
mod ephemeron;
mod ops;
mod soak;
mod threads;
mod values;

use super::*;
use crate::mem::{velt_rt_alloc, velt_rt_free};
use std::cell::Cell;

/// A counted test object with three strong reference fields (null: none).
#[repr(C)]
pub(super) struct Obj {
    pub(super) fields: [*mut u8; 3],
}

const SIZE: u64 = 8 + std::mem::size_of::<Obj>() as u64;

thread_local! {
    /// Test objects allocated and not yet freed on this thread.
    static LIVE: Cell<i64> = const { Cell::new(0) };
}

pub(super) fn live() -> i64 {
    LIVE.with(Cell::get)
}

/// A new object (count 1) holding `fields` (whose references it takes over).
pub(super) fn new_obj(fields: &[*mut u8]) -> *mut u8 {
    let mut f = [std::ptr::null_mut(); 3];
    f[..fields.len()].copy_from_slice(fields);
    let block = velt_rt_alloc(SIZE, 8) as *mut u64;
    // SAFETY: a fresh block of `SIZE` bytes.
    unsafe {
        *block = 1;
        let obj = block.add(1) as *mut Obj;
        obj.write(Obj { fields: f });
        LIVE.with(|l| l.set(l.get() + 1));
        obj as *mut u8
    }
}

/// The object's field `i`.
pub(super) fn field(obj: *mut u8, i: usize) -> *mut u8 {
    // SAFETY: tests pass live objects.
    unsafe { (*(obj as *mut Obj)).fields[i] }
}

/// Stores `value` (a reference the caller gives up) in field `i`, releasing the old one.
pub(super) fn set_field(obj: *mut u8, i: usize, value: *mut u8) {
    // SAFETY: tests pass live objects.
    let old = unsafe { std::mem::replace(&mut (*(obj as *mut Obj)).fields[i], value) };
    if !old.is_null() {
        release(old);
    }
}

/// `count += 1`.
pub(super) fn retain(obj: *mut u8) -> *mut u8 {
    // SAFETY: tests pass live objects.
    unsafe { *rc_word(obj) += 1 };
    obj
}

/// The count proper.
pub(super) fn count(obj: *mut u8) -> u64 {
    // SAFETY: tests pass live objects.
    unsafe { *rc_word(obj) & RC_COUNT }
}

pub(super) fn is_marked(obj: *mut u8) -> bool {
    // SAFETY: tests pass live objects.
    unsafe { *rc_word(obj) & RC_WEAK != 0 }
}

/// The release sequence for a weak-capable type (module docs of `weak`), inline as generated
/// code has it: with `RC_WEAK` the sign bit, one signed compare sends both the unique and the
/// weakly held case off the shared path, which stays as short as today's.
#[inline(always)]
pub(super) fn release(obj: *mut u8) {
    // SAFETY: tests own the reference they release.
    unsafe {
        let rc = rc_word(obj);
        let c = *rc;
        if c as i64 > 1 {
            *rc = c - 1;
        } else if c == 1 || velt_rt_weak_release(obj) != 0 {
            destroy(obj);
        }
    }
}

/// Drops the fields and frees the block (drop glue: a call, as in generated code).
#[inline(never)]
unsafe fn destroy(obj: *mut u8) {
    let fields = (*(obj as *mut Obj)).fields;
    for f in fields {
        if !f.is_null() {
            release(f);
        }
    }
    velt_rt_free(rc_word(obj) as *mut u8, SIZE, 8);
    LIVE.with(|l| l.set(l.get() - 1));
}

/// `Obj`'s trace glue.
pub(super) unsafe extern "C" fn trace_obj(obj: *mut u8, visit: VisitFn, ctx: *mut c_void) {
    for f in (*(obj as *mut Obj)).fields {
        if !f.is_null() {
            visit(ctx, f, Some(TraceFn(trace_obj)));
        }
    }
}

/// `RetainFn` for `Obj` values.
pub(super) unsafe extern "C" fn retain_value(v: u64) {
    retain(v as *mut u8);
}

/// `ReleaseFn` for `Obj` values.
pub(super) unsafe extern "C" fn release_value(v: u64) {
    release(v as *mut u8);
}

/// A map from `Obj` keys to `Obj` values (the `raw -> proxy` cache).
pub(super) fn obj_map() -> MapId {
    velt_rt_weakmap_new(
        Some(TraceFn(trace_obj)),
        Some(retain_value),
        Some(release_value),
        Some(TraceFn(trace_obj)),
    )
}

pub(super) fn set(m: MapId, key: *mut u8, value: *mut u8) {
    // SAFETY: tests pass live objects and give up `value`.
    unsafe { velt_rt_weakmap_set(m, key, value as u64) }
}

/// `map.get(key)`: a new reference to the value (for maps of `Obj` values).
pub(super) fn get(m: MapId, key: *mut u8) -> Option<*mut u8> {
    let mut found = 0;
    // SAFETY: `found` is a local.
    let v = unsafe { velt_rt_weakmap_get(m, key, &mut found) };
    (found != 0).then_some(v as *mut u8)
}

/// `signal(raw)` as sigx writes it: the cached proxy, or a new proxy cell holding `raw`
/// (retained) and `handler`, cached. Returns a new reference to the proxy.
pub(super) fn proxy_of(cache: MapId, raw: *mut u8, handler: *mut u8) -> *mut u8 {
    if let Some(p) = get(cache, raw) {
        return p;
    }
    let p = new_obj(&[retain(raw), handler]);
    set(cache, raw, retain(p));
    p
}

/// Drops the map; asserts nothing of this thread's weak state is left behind.
pub(super) fn finish(maps: &[MapId]) {
    for &m in maps {
        // SAFETY: each map is dropped once.
        unsafe { velt_rt_weakmap_drop(m) };
    }
    assert_eq!(tracked_objects(), 0, "side table not empty");
}

/// Runs `f` and asserts it leaves no test object alive.
pub(super) fn no_leak(f: impl FnOnce()) {
    let before = live();
    f();
    assert_eq!(live(), before, "test objects leaked");
}
