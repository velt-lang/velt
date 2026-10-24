//! The live view of each object (rt_abi.md "Views", #1485; velt_vir `lower/any_view.rs`): a
//! view is the `Proxy` an `any` holding an object of another type becomes when it is converted
//! to an object type, and an object has at most one at a time, so a view's identity is its
//! pointer. The table maps the object's data pointer to its view. It holds no reference to
//! either: the view holds the object, and the view's drop removes its entry, so an entry lives
//! exactly as long as its view and an address is never reused while it is in the table.
//!
//! One table for the process, behind a lock: a task moves between worker threads at an `await`,
//! so a view made on one thread may be converted again, or dropped, on another.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::{Mutex, PoisonError};

/// Hashes object addresses: one multiply (Fibonacci hashing).
#[derive(Default)]
struct AddrHasher(u64);

impl Hasher for AddrHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(8) ^ u64::from(b)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }
    fn write_usize(&mut self, n: usize) {
        self.0 = (n as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .rotate_left(29);
    }
}

type Table = HashMap<usize, usize, BuildHasherDefault<AddrHasher>>;

static VIEWS: Mutex<Table> = Mutex::new(HashMap::with_hasher(BuildHasherDefault::new()));

fn with<R>(f: impl FnOnce(&mut Table) -> R) -> R {
    f(&mut VIEWS.lock().unwrap_or_else(PoisonError::into_inner))
}

/// The live view of the object at `obj`, not counted (the caller retains it), or null.
#[no_mangle]
pub extern "C" fn velt_rt_view_get(obj: *const u8) -> *mut u8 {
    with(|t| t.get(&(obj as usize)).copied().unwrap_or(0) as *mut u8)
}

/// `view` (just made, untagged) is the live view of the object at `obj`.
#[no_mangle]
pub extern "C" fn velt_rt_view_put(obj: *const u8, view: *const u8) {
    with(|t| t.insert(obj as usize, view as usize));
}

/// The drop of `view`, a view of the object at `obj`: its entry goes (if it is still the
/// object's entry).
#[no_mangle]
pub extern "C" fn velt_rt_view_remove(obj: *const u8, view: *const u8) {
    with(|t| {
        if t.get(&(obj as usize)) == Some(&(view as usize)) {
            t.remove(&(obj as usize));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_view_is_found_until_it_is_removed() {
        let (obj, view, other) = (
            0x1000 as *const u8,
            0x2000 as *const u8,
            0x3000 as *const u8,
        );
        assert!(velt_rt_view_get(obj).is_null());
        velt_rt_view_put(obj, view);
        assert_eq!(velt_rt_view_get(obj) as usize, 0x2000);
        // Another view's drop leaves the entry.
        velt_rt_view_remove(obj, other);
        assert_eq!(velt_rt_view_get(obj) as usize, 0x2000);
        velt_rt_view_remove(obj, view);
        assert!(velt_rt_view_get(obj).is_null());
        // The address may hold a new object, with a view of its own.
        velt_rt_view_put(obj, other);
        assert_eq!(velt_rt_view_get(obj) as usize, 0x3000);
        velt_rt_view_remove(obj, other);
        assert!(velt_rt_view_get(obj).is_null());
    }

    #[test]
    fn views_of_objects_are_independent() {
        let objs: Vec<usize> = (1..=100).map(|i| 0x10_0000 + i * 64).collect();
        for &o in &objs {
            velt_rt_view_put(o as *const u8, (o + 8) as *const u8);
        }
        for &o in &objs {
            assert_eq!(velt_rt_view_get(o as *const u8) as usize, o + 8);
        }
        for &o in objs.iter().step_by(2) {
            velt_rt_view_remove(o as *const u8, (o + 8) as *const u8);
        }
        for (i, &o) in objs.iter().enumerate() {
            let found = velt_rt_view_get(o as *const u8) as usize;
            assert_eq!(found, if i % 2 == 0 { 0 } else { o + 8 });
            velt_rt_view_remove(o as *const u8, (o + 8) as *const u8);
        }
    }
}
