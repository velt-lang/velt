//! The row cursors and parameter sinks native drivers hand to std (native_abi.md "Row cursors"
//! and "Parameter sinks"). A driver registers each one here through the function table
//! ([`db_open`], via `velt_text::rows_cursor::open_cursor` / `open_sink`) and gives Velt code the
//! key, a `u64` handle like every other native handle ([`crate::registry`]); std's
//! `decodeRows` / `bindParams` turn it back into the cursor's or sink's address with
//! [`velt_rt_db_cursor`] / [`velt_rt_db_sink`], and the driver takes its object back with
//! [`db_close`].
//!
//! So no Velt code ever passes an address: a number that is not an open cursor (made up, closed
//! already, or a sink's) finds nothing and std throws `RowError`, instead of the decoder reading
//! whatever memory the number points at.

use std::ffi::c_void;

use velt_text::rows_cursor::{CursorVTable, RawCursor, RawSink, SinkVTable};

use crate::registry::{Key, Registry};

/// [`db_open`]'s `kind`: a row cursor (`vtable` is a `CursorVTable`) or a parameter sink.
pub const DB_CURSOR: u32 = 0;
pub const DB_SINK: u32 = 1;

/// A registered cursor: its ABI form (whose address std's decoder reads) and the driver's type
/// tag, which [`db_close`] checks before handing the data back.
struct Cursor {
    raw: RawCursor,
    tag: u64,
}

struct Sink {
    raw: RawSink,
    tag: u64,
}

// SAFETY: the pointers are the driver's; the registry only stores them, and the driver's object is
// used by one task at a time (the one decoding or binding), as the ABI requires.
unsafe impl Send for Cursor {}
unsafe impl Sync for Cursor {}
unsafe impl Send for Sink {}
unsafe impl Sync for Sink {}

static CURSORS: Registry<Cursor> = Registry::new();
static SINKS: Registry<Sink> = Registry::new();

/// Function table entry `db_open(kind, data, vtable, tag) -> handle`: registers a driver's
/// cursor or sink (its data and `&'static` function table) under the driver's type tag; 0 for
/// an unknown kind or a null pointer.
///
/// # Safety
/// `vtable` is null or a `&'static CursorVTable` (`DB_CURSOR`) or `SinkVTable` (`DB_SINK`);
/// `data` stays valid for it until [`db_close`] returns it.
pub unsafe extern "C" fn db_open(kind: u32, data: *mut c_void, vtable: *const c_void, tag: u64) -> u64 {
    if data.is_null() || vtable.is_null() {
        return 0;
    }
    match kind {
        DB_CURSOR => {
            let vtable = &*(vtable as *const CursorVTable);
            CURSORS.insert(Cursor { raw: RawCursor { data, vtable }, tag }).bits()
        }
        DB_SINK => {
            let vtable = &*(vtable as *const SinkVTable);
            SINKS.insert(Sink { raw: RawSink { data, vtable }, tag }).bits()
        }
        _ => 0,
    }
}

/// Function table entry `db_close(kind, handle, tag) -> data`: unregisters an open cursor or
/// sink registered with the same `tag` and returns its data for the driver to free; null (and
/// nothing changes) for any other handle.
pub extern "C" fn db_close(kind: u32, handle: u64, tag: u64) -> *mut c_void {
    match kind {
        DB_CURSOR => {
            let key = Key::from_bits(handle);
            if CURSORS.get(key).is_none_or(|c| c.tag != tag) {
                return std::ptr::null_mut();
            }
            CURSORS.remove(key).map_or(std::ptr::null_mut(), |c| c.raw.data)
        }
        DB_SINK => {
            let key = Key::from_bits(handle);
            if SINKS.get(key).is_none_or(|s| s.tag != tag) {
                return std::ptr::null_mut();
            }
            SINKS.remove(key).map_or(std::ptr::null_mut(), |s| s.raw.data)
        }
        _ => std::ptr::null_mut(),
    }
}

/// `u64 velt_rt_db_cursor(u64 handle)` (std's velt/db): the address of the open row cursor
/// `handle` names, for `jsonParseCursor`; 0 when it names none. The address stays
/// valid until the driver closes the cursor.
#[no_mangle]
pub extern "C" fn velt_rt_db_cursor(handle: u64) -> u64 {
    CURSORS
        .get(Key::from_bits(handle))
        .map_or(0, |c| &c.raw as *const RawCursor as u64)
}

/// `u64 velt_rt_db_sink(u64 handle)` (std's velt/db): the address of the open parameter sink
/// `handle` names, for `dbBind`; 0 when it names none.
#[no_mangle]
pub extern "C" fn velt_rt_db_sink(handle: u64) -> u64 {
    SINKS
        .get(Key::from_bits(handle))
        .map_or(0, |s| &s.raw as *const RawSink as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_text::rows_cursor::{Cell, RowCursor};

    struct Empty;

    impl RowCursor for Empty {
        fn header(&self) -> &[u8] {
            &[0, 0, 0]
        }
        fn step(&mut self) -> bool {
            false
        }
        fn cell(&self, _: usize) -> Cell<'_> {
            Cell::Null
        }
    }

    #[test]
    fn only_open_handles_reach_a_cursor() {
        let mut c = Empty;
        let raw = RawCursor::new(&mut c as *mut Empty);
        let data = raw.data;
        // SAFETY: `c` outlives its registration.
        let h = unsafe { db_open(DB_CURSOR, data, raw.vtable as *const _ as *const c_void, 7) };
        assert_ne!(h, 0);
        assert_ne!(velt_rt_db_cursor(h), 0);
        // Made-up numbers, a sink's lookup, a wrong tag: nothing.
        assert_eq!(velt_rt_db_cursor(0), 0);
        assert_eq!(velt_rt_db_cursor(0xdead_beef), 0);
        assert_eq!(velt_rt_db_cursor(h ^ (1 << 32)), 0);
        assert_eq!(velt_rt_db_sink(h), 0);
        assert!(db_close(DB_CURSOR, h, 8).is_null());
        assert!(db_close(DB_SINK, h, 7).is_null());
        assert_ne!(velt_rt_db_cursor(h), 0);
        // Closed once: the data comes back, then the handle is dead.
        assert_eq!(db_close(DB_CURSOR, h, 7), data);
        assert_eq!(velt_rt_db_cursor(h), 0);
        assert!(db_close(DB_CURSOR, h, 7).is_null());
    }
}
