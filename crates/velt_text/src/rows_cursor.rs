//! Result rows read where the driver has them (native_abi.md "Row cursors"): instead of writing
//! every row into a binary buffer ([`crate::rows`]) for std's decoder to read back, a driver
//! hands the decoder a cursor over its live result, and the decoder takes each value straight
//! from the driver's memory into the Velt value it builds (one copy, of the text it keeps).
//!
//! Any native driver can offer one: it implements [`RowCursor`] and hands std the address of a
//! [`RawCursor`] ([`RawCursor::new`]); std decodes with `__intrinsic_json_parse_cursor<T>`
//! (flag `FLAG_CURSOR` of `velt_rt_json_reader_new_with`) and gives the cursor back to the
//! driver afterwards, which then releases what it held and reports a failure the rows met.
//!
//! The cursor answers like the binary rows it replaces: the same header (`single`, the column
//! names), then the rows, each read cell by cell, in any order, between two steps.

use std::ffi::c_void;

/// One value of the current row, borrowed from the driver until its next step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cell<'a> {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Text, UTF-8 or not (the reader replaces what is not with U+FFFD).
    Text(&'a [u8]),
    /// Text the driver knows is all ASCII.
    Ascii(&'a [u8]),
    Blob(&'a [u8]),
}

/// What a driver implements to offer its rows as a cursor.
pub trait RowCursor {
    /// The header as [`crate::rows`] lays it out: `single: u8, columns: u16, names`
    /// ([`crate::rows::encode_names`] makes the names part). It stays valid, unchanged, until
    /// the cursor is dropped.
    fn header(&self) -> &[u8];
    /// Moves to the next row (the first on the first call): false when there is none, also
    /// after a failure, which the driver keeps to report when the cursor is given back.
    fn step(&mut self) -> bool;
    /// Cell `col` (below the header's column count) of the current row.
    fn cell(&self, col: usize) -> Cell<'_>;
}

/// [`Cell`] across the C ABI.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawCell {
    /// [`crate::rows`]' tags: `NULL`, `FALSE`, `TRUE`, `INT`, `FLOAT`, `TEXT`, `BLOB`, `ASCII`.
    pub tag: u8,
    pub int: i64,
    pub float: f64,
    pub ptr: *const u8,
    pub len: usize,
}

/// The functions of a [`RawCursor`].
#[repr(C)]
pub struct CursorVTable {
    /// [`RowCursor::header`]: its address, and its length in `*len`.
    pub header: unsafe extern "C" fn(data: *mut c_void, len: *mut usize) -> *const u8,
    /// [`RowCursor::step`]: 1 for a row, 0 for none.
    pub step: unsafe extern "C" fn(data: *mut c_void) -> u8,
    /// [`RowCursor::cell`] into `*out`.
    pub cell: unsafe extern "C" fn(data: *mut c_void, col: usize, out: *mut RawCell),
}

/// A driver's cursor as std and the runtime see it: its data and functions. std receives its
/// address (a `u64`) from the driver and gives that back when it has decoded the rows.
#[repr(C)]
pub struct RawCursor {
    pub data: *mut c_void,
    pub vtable: &'static CursorVTable,
}

impl RawCursor {
    /// The cursor of `c`, which must stay at its address (and not be used otherwise) while the
    /// cursor is.
    pub fn new<C: RowCursor>(c: *mut C) -> RawCursor {
        RawCursor {
            data: c as *mut c_void,
            vtable: &Vtable::<C>::VTABLE,
        }
    }

    /// # Safety
    /// The cursor's data is alive; the header is valid until it is dropped.
    pub unsafe fn header<'a>(&self) -> &'a [u8] {
        let mut len = 0;
        let p = (self.vtable.header)(self.data, &mut len);
        std::slice::from_raw_parts(p, len)
    }

    /// # Safety
    /// As [`RawCursor::header`].
    pub unsafe fn step(&self) -> bool {
        (self.vtable.step)(self.data) != 0
    }

    /// Cell `col`, valid until the next step.
    ///
    /// # Safety
    /// As [`RawCursor::header`]; `col` is below the header's column count.
    pub unsafe fn cell<'a>(&self, col: usize) -> Cell<'a> {
        use crate::rows::*;
        let mut c = RawCell {
            tag: NULL,
            int: 0,
            float: 0.0,
            ptr: std::ptr::null(),
            len: 0,
        };
        (self.vtable.cell)(self.data, col, &mut c);
        let bytes = || match c.len {
            0 => &[][..],
            n => std::slice::from_raw_parts(c.ptr, n),
        };
        match c.tag {
            FALSE => Cell::Bool(false),
            TRUE => Cell::Bool(true),
            INT => Cell::Int(c.int),
            FLOAT => Cell::Float(c.float),
            TEXT => Cell::Text(bytes()),
            ASCII => Cell::Ascii(bytes()),
            BLOB => Cell::Blob(bytes()),
            _ => Cell::Null,
        }
    }
}

struct Vtable<C>(std::marker::PhantomData<C>);

impl<C: RowCursor> Vtable<C> {
    const VTABLE: CursorVTable = CursorVTable {
        header: Self::header,
        step: Self::step,
        cell: Self::cell,
    };

    unsafe extern "C" fn header(data: *mut c_void, len: *mut usize) -> *const u8 {
        let h = (*(data as *const C)).header();
        len.write(h.len());
        h.as_ptr()
    }

    unsafe extern "C" fn step(data: *mut c_void) -> u8 {
        (*(data as *mut C)).step() as u8
    }

    unsafe extern "C" fn cell(data: *mut c_void, col: usize, out: *mut RawCell) {
        use crate::rows::*;
        let (tag, int, float, bytes): (u8, i64, f64, &[u8]) = match (*(data as *const C)).cell(col)
        {
            Cell::Null => (NULL, 0, 0.0, &[]),
            Cell::Bool(b) => (if b { TRUE } else { FALSE }, 0, 0.0, &[]),
            Cell::Int(v) => (INT, v, 0.0, &[]),
            Cell::Float(v) => (FLOAT, 0, v, &[]),
            Cell::Text(t) => (TEXT, 0, 0.0, t),
            Cell::Ascii(t) => (ASCII, 0, 0.0, t),
            Cell::Blob(b) => (BLOB, 0, 0.0, b),
        };
        out.write(RawCell {
            tag,
            int,
            float,
            ptr: bytes.as_ptr(),
            len: bytes.len(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Two {
        header: Vec<u8>,
        row: usize,
        text: String,
    }

    impl RowCursor for Two {
        fn header(&self) -> &[u8] {
            &self.header
        }
        fn step(&mut self) -> bool {
            self.row += 1;
            self.text = format!("row {}", self.row);
            self.row <= 2
        }
        fn cell(&self, col: usize) -> Cell<'_> {
            match col {
                0 => Cell::Int(self.row as i64),
                _ => Cell::Text(self.text.as_bytes()),
            }
        }
    }

    #[test]
    fn a_cursor_reads_through_its_raw_form() {
        let mut header = vec![0];
        header.extend(crate::rows::encode_names(["id", "name"]));
        let mut two = Two {
            header: header.clone(),
            row: 0,
            text: String::new(),
        };
        let raw = RawCursor::new(&mut two);
        // SAFETY: `two` outlives `raw` and stays put.
        unsafe {
            assert_eq!(raw.header(), &header[..]);
            assert!(raw.step());
            assert_eq!(raw.cell(0), Cell::Int(1));
            assert_eq!(raw.cell(1), Cell::Text(b"row 1"));
            assert!(raw.step());
            assert_eq!(raw.cell(1), Cell::Text(b"row 2"));
            assert!(!raw.step());
        }
    }
}
