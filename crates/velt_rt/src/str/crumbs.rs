//! Position translation between UTF-16 code units and WTF-8 bytes (#377,
//! docs/internals/design/strings.md "Breadcrumbs"): [`VeltStr::unit_to_byte`] and
//! [`VeltStr::byte_to_unit`].
//!
//! - ASCII (`units == bytes`): the position is the same number, O(1).
//! - A non-ASCII string of at most [`STRIDE`] units, or one without a heap header (inline, static
//!   or borrowed): a scan, forward from the start or backward from the end, whichever is closer
//!   (`byte_to_unit` counts with the vectorized unit count instead).
//! - A non-ASCII heap string of more than [`STRIDE`] units: a table of the byte offset of every
//!   [`STRIDE`]th unit ("breadcrumbs", one `u32` each, 1/16 of the text at most), built by the
//!   first translation, published in the buffer header's `crumbs` field with a compare-and-swap
//!   (strings cross threads) and freed with the buffer; then a lookup is one table load plus a
//!   forward scan of fewer than [`STRIDE`] units. A translation near the thread's last one of
//!   the same string steps from it instead (`recent.rs`): a sequential index loop decodes one
//!   character per step.
//!
//! A uniquely owned buffer that is appended to keeps its table: the bytes of the prefix never
//! change (a join rewrites only the high surrogate that ends the old text into the start of the
//! pair, at the same offset, and no entry lies past the old text). A table that no longer covers
//! the string is extended when a translation needs it, and translating takes only `&self`: a
//! string whose count is 1 may still be read by two threads at once without a retain (a field of
//! a `shared` object passed by pointer to a runtime call), so a reference count says nothing about
//! who else reads the table. Hence entries are atomics and only ever appended: an extension that
//! fits the table's capacity writes the entries past `len` (concurrent extenders write the same
//! values, the text being immutable while readable) and then publishes the new `len`; one that
//! doesn't publishes a copy twice the size with a compare-and-swap, keeping the old table alive
//! (chained from the new one, freed with the buffer) because another thread may still read it.
//! Capacities double, so the chain holds at most as many entries as the last table.
//!
//! The string methods (`str_ops`), `charCodeAt` on non-ASCII strings and the regex offsets
//! translate their code-unit positions through this API (#377 phase 2b), checked against the
//! reference model in tests/abi/utf16_model.rs.

use std::alloc::{self, Layout};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use super::{heap, recent, wtf8, VeltStr};

/// Units between two breadcrumbs, and the longest string translated by a scan alone.
pub const STRIDE: usize = 64;

/// How far before the last translated unit a translation steps back from it (a loop running
/// backward), rather than starting from a breadcrumb.
const BACK_STEPS: usize = 8;

/// The top bit of an entry: the unit the entry stands for is the second half (low surrogate) of
/// the 4-byte sequence at the entry's offset, whose first half is the unit before. Offsets are
/// below 2 GiB (`MAX_LEN`), so the bit is free.
const LOW_HALF: u32 = 1 << 31;

/// A position in a string as bytes: the code point holding a code unit, and whether the unit is
/// the second half (the low surrogate) of the 4-byte sequence there. A unit index equal to the
/// length is `{ byte: len, low_half: false }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BytePos {
    /// Byte offset of the code point (always at a code point boundary).
    pub byte: usize,
    /// The unit is the low surrogate of the supplementary character at `byte`.
    pub low_half: bool,
}

/// A breadcrumb table: `entries[k]` is the byte offset of unit `k * STRIDE` (with [`LOW_HALF`]),
/// for every `k * STRIDE` below the string's unit count when it was built or last extended.
/// Entries below `len` never change; entries are written before `len` covers them (release) and
/// read after it does (acquire).
#[repr(C)]
struct Table {
    /// Entries in use.
    len: AtomicUsize,
    /// Entries the allocation holds.
    cap: usize,
    /// A table this one replaced while another thread might still read it (freed with this one).
    prev: *mut Table,
}

impl Table {
    fn layout(cap: usize) -> Layout {
        let size = std::mem::size_of::<Table>() + cap * std::mem::size_of::<u32>();
        Layout::from_size_align(size, std::mem::align_of::<Table>())
            .unwrap_or_else(|_| crate::panic::fatal("string too long"))
    }

    /// The entries of the table at `t` that are in use.
    ///
    /// # Safety
    /// `t` must be a live table.
    unsafe fn entries<'a>(t: *mut Table) -> &'a [AtomicU32] {
        std::slice::from_raw_parts(Table::data(t), (*t).len.load(Ordering::Acquire))
    }

    unsafe fn data(t: *mut Table) -> *const AtomicU32 {
        (t as *mut u8).add(std::mem::size_of::<Table>()) as *const AtomicU32
    }

    /// Entry `k` (below `len`).
    ///
    /// # Safety
    /// `t` must be a live table with more than `k` entries in use.
    unsafe fn entry(t: *mut Table, k: usize) -> u32 {
        (*Table::data(t).add(k)).load(Ordering::Relaxed)
    }

    /// A new table with room for `cap` entries holding `old`'s entries.
    fn alloc(cap: usize, old: &[AtomicU32]) -> *mut Table {
        let l = Table::layout(cap);
        // SAFETY: the layout has a non-zero size (the header).
        let t = unsafe { alloc::alloc(l) } as *mut Table;
        if t.is_null() {
            alloc::handle_alloc_error(l);
        }
        // SAFETY: fresh allocation with room for the header and `cap >= old.len()` entries.
        unsafe {
            t.write(Table {
                len: AtomicUsize::new(old.len()),
                cap,
                prev: std::ptr::null_mut(),
            });
            let data = Table::data(t) as *mut AtomicU32;
            for (k, e) in old.iter().enumerate() {
                data.add(k).write(AtomicU32::new(e.load(Ordering::Relaxed)));
            }
        }
        t
    }

    /// Append the entries of `bytes` (WTF-8 with `units` code units) after the last one, up to
    /// every multiple of [`STRIDE`] below `units`, and publish them. Another thread may extend
    /// the same table at the same time: both write the same values (the text is immutable while
    /// two threads can read it), and `len` only grows.
    ///
    /// # Safety
    /// `t` must be a live table with room for them, built for a prefix of `bytes`.
    unsafe fn fill(t: *mut Table, bytes: &[u8], units: usize) {
        let want = units.div_ceil(STRIDE);
        debug_assert!(want <= (*t).cap);
        let data = Table::data(t);
        let mut len = (*t).len.load(Ordering::Acquire);
        if len >= want {
            return;
        }
        // Start at the last entry (or the start of the text).
        let (mut byte, mut unit) = match len {
            0 => (0, 0),
            _ => start_of(Table::entry(t, len - 1), len - 1),
        };
        if len == 0 {
            (*data).store(0, Ordering::Relaxed);
            len = 1;
        }
        while len < want {
            let target = len * STRIDE;
            let pos = scan_forward(bytes, byte, unit, target);
            (*data.add(len)).store(encode(pos), Ordering::Relaxed);
            (byte, unit) = start_of(encode(pos), len);
            len += 1;
        }
        (*t).len.fetch_max(len, Ordering::Release);
    }
}

/// An entry for a position.
fn encode(pos: BytePos) -> u32 {
    pos.byte as u32 | if pos.low_half { LOW_HALF } else { 0 }
}

/// Where a scan may start for entry `k` with value `e`: the code point's byte offset and the
/// index of its first unit.
fn start_of(e: u32, k: usize) -> (usize, usize) {
    let unit = k * STRIDE - (e & LOW_HALF != 0) as usize;
    ((e & !LOW_HALF) as usize, unit)
}

/// Bytes of the sequence a lead byte starts.
#[inline]
fn seq_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0x80..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Code units of the sequence a lead byte starts (two for a supplementary character).
#[inline]
fn seq_units(lead: u8) -> usize {
    1 + (lead >= 0xF0) as usize
}

/// From the code point at `byte`, whose first unit is `unit`, forward to unit `target`
/// (`target >= unit`, at most the string's length).
fn scan_forward(bytes: &[u8], mut byte: usize, mut unit: usize, target: usize) -> BytePos {
    while unit < target {
        let lead = bytes[byte];
        if lead < 0x80 {
            // A run of ASCII: one byte per unit.
            let run = bytes[byte..]
                .iter()
                .take(target - unit)
                .take_while(|b| b.is_ascii())
                .count();
            byte += run;
            unit += run;
            continue;
        }
        let w = seq_units(lead);
        if unit + w > target {
            return BytePos {
                byte,
                low_half: true,
            };
        }
        byte += seq_len(lead);
        unit += w;
    }
    BytePos {
        byte,
        low_half: false,
    }
}

/// From the code point boundary at `byte`, whose first unit is `unit` (the end of the string:
/// `bytes.len()` and the unit count), backward to unit `target` (`target <= unit`).
fn scan_backward(bytes: &[u8], mut byte: usize, mut unit: usize, target: usize) -> BytePos {
    while unit > target {
        let start = wtf8::start_before(bytes, byte);
        let w = seq_units(bytes[start]);
        if unit - w < target {
            return BytePos {
                byte: start,
                low_half: true,
            };
        }
        byte = start;
        unit -= w;
    }
    BytePos {
        byte,
        low_half: false,
    }
}

/// Free a table and the tables it replaced.
///
/// # Safety
/// `t` must be null or a table nobody reads any more.
#[inline]
pub(super) unsafe fn free(t: *mut u8) {
    if !t.is_null() {
        free_chain(t as *mut Table);
    }
}

#[cold]
unsafe fn free_chain(mut t: *mut Table) {
    while !t.is_null() {
        let prev = (*t).prev;
        alloc::dealloc(t as *mut u8, Table::layout((*t).cap));
        t = prev;
    }
}

impl VeltStr {
    /// The byte position of UTF-16 unit `unit` (at most [`Self::units`]; larger is clamped).
    ///
    /// # Safety
    /// `self` must be valid.
    pub unsafe fn unit_to_byte(&self, unit: usize) -> BytePos {
        let units = self.units();
        let unit = unit.min(units);
        if self.is_ascii() || unit == units {
            let byte = if self.is_ascii() { unit } else { self.len() };
            return BytePos {
                byte,
                low_half: false,
            };
        }
        let bytes = self.as_bytes();
        if units > STRIDE && self.is_heap() {
            let (ptr, w1) = (self.w0 as usize, self.w1);
            let pos = match recent::find(ptr, w1) {
                // Near the last translation: step from it.
                Some(e) if unit >= e.unit && unit - e.unit <= STRIDE => {
                    scan_forward(bytes, e.pos.byte, e.unit - e.pos.low_half as usize, unit)
                }
                Some(e) if unit < e.unit && e.unit - unit <= BACK_STEPS => {
                    // From the boundary after the remembered unit's code point.
                    let (byte, from) = match e.pos.low_half {
                        true => (e.pos.byte + 4, e.unit + 1),
                        false => (e.pos.byte, e.unit),
                    };
                    scan_backward(bytes, byte, from, unit)
                }
                _ => {
                    let k = unit / STRIDE;
                    let (byte, from) = start_of(Table::entry(self.crumbs(k), k), k);
                    scan_forward(bytes, byte, from, unit)
                }
            };
            // Only a string with breadcrumbs is remembered: freeing it then forgets it.
            recent::remember(ptr, w1, unit, pos);
            return pos;
        }
        if unit <= units / 2 {
            scan_forward(bytes, 0, 0, unit)
        } else {
            scan_backward(bytes, bytes.len(), units, unit)
        }
    }

    /// The byte positions of units `a` and `b` (`a <= b`; both clamped as by
    /// [`Self::unit_to_byte`]): the second is found by a forward scan from the first when it is
    /// near (a short slice of a long string costs one translation).
    ///
    /// # Safety
    /// `self` must be valid.
    pub unsafe fn unit_range_to_bytes(&self, a: usize, b: usize) -> (BytePos, BytePos) {
        let pa = self.unit_to_byte(a);
        let b = b.min(self.units());
        if self.is_ascii() || b <= a || b - a >= STRIDE {
            return (pa, self.unit_to_byte(b));
        }
        let first = a - pa.low_half as usize;
        (pa, scan_forward(self.as_bytes(), pa.byte, first, b))
    }

    /// The index of the first UTF-16 unit of the code point at byte `byte` (a code point
    /// boundary, at most [`Self::len`]; larger is clamped).
    ///
    /// # Safety
    /// `self` must be valid and `byte` at a code point boundary.
    pub unsafe fn byte_to_unit(&self, byte: usize) -> usize {
        let byte = byte.min(self.len());
        if self.is_ascii() {
            return byte;
        }
        let bytes = self.as_bytes();
        let units = self.units();
        if units > STRIDE && self.is_heap() {
            let (ptr, w1) = (self.w0 as usize, self.w1);
            if let Some(e) = recent::find(ptr, w1) {
                if byte >= e.pos.byte && byte - e.pos.byte <= 4 * STRIDE {
                    let first = e.unit - e.pos.low_half as usize;
                    let unit = first + wtf8::count_units(&bytes[e.pos.byte..byte]);
                    let pos = BytePos {
                        byte,
                        low_half: false,
                    };
                    recent::remember(ptr, w1, unit, pos);
                    return unit;
                }
            }
            let entries = Table::entries(self.crumbs(units.div_ceil(STRIDE) - 1));
            // The last entry at or before `byte`; entry 0 is offset 0.
            let k = entries
                .partition_point(|e| (e.load(Ordering::Relaxed) & !LOW_HALF) as usize <= byte)
                - 1;
            let (from, unit) = start_of(entries[k].load(Ordering::Relaxed), k);
            let unit = unit + wtf8::count_units(&bytes[from..byte]);
            let pos = BytePos {
                byte,
                low_half: false,
            };
            recent::remember(ptr, w1, unit, pos);
            return unit;
        }
        if byte <= bytes.len() / 2 {
            wtf8::count_units(&bytes[..byte])
        } else {
            units - wtf8::count_units(&bytes[byte..])
        }
    }

    /// The breadcrumb table of this non-ASCII heap string, with at least `k + 1` entries (`k`
    /// below the entries a table of the whole string has): built on first use, extended when the
    /// string grew since.
    ///
    /// # Safety
    /// `self` must be a valid non-ASCII heap string of more than [`STRIDE`] units.
    unsafe fn crumbs(&self, k: usize) -> *mut Table {
        let field = heap::crumbs(self.ptr());
        let current = field.load(Ordering::Acquire) as *mut Table;
        if !current.is_null() && k < (*current).len.load(Ordering::Acquire) {
            return current;
        }
        self.build_crumbs(current)
    }

    /// [`Self::crumbs`] when the table is missing or too short: extended in place when it has
    /// room, else replaced by a copy twice its size, published with a compare-and-swap. Never
    /// relies on the buffer's count (see the module docs).
    #[cold]
    #[inline(never)]
    unsafe fn build_crumbs(&self, mut current: *mut Table) -> *mut Table {
        let field = heap::crumbs(self.ptr());
        let (bytes, units) = (self.as_bytes(), self.units());
        let want = units.div_ceil(STRIDE);
        loop {
            if !current.is_null() && (*current).cap >= want {
                Table::fill(current, bytes, units);
                return current;
            }
            let (old, cap) = if current.is_null() {
                (&[][..], want)
            } else {
                (Table::entries(current), want.max((*current).cap * 2))
            };
            let t = Table::alloc(cap, old);
            Table::fill(t, bytes, units);
            (*t).prev = current;
            match field.compare_exchange(
                current as *mut u8,
                t as *mut u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return t,
                Err(won) => {
                    // Another thread published first: drop ours (not its `prev`, which stays
                    // with the published table) and extend theirs.
                    (*t).prev = std::ptr::null_mut();
                    free_chain(t);
                    current = won as *mut Table;
                }
            }
        }
    }
}

/// The order of two strings by UTF-16 code units (JavaScript's `<` and default `sort`), from
/// their canonical WTF-8 (design note "The ordering rule"). Byte order is code point order,
/// which differs from code-unit order between U+E000–U+FFFF and supplementary characters and
/// between lone surrogates and supplementary characters, so: the first differing byte is found
/// (a `memcmp`), the comparison steps back to the start of the code point there (the same offset
/// in both strings, the prefix being shared), and compares the first code unit of each side;
/// when those are equal (a supplementary character against one with the same high surrogate, or
/// against that lone high surrogate) it compares the second units, where the end of a string is
/// below every unit. In canonical form a lone high surrogate is never followed by a low one, so
/// that one extra comparison decides.
/// `velt_rt_str_cmp` (`<`, `sort()`) uses it for every pair of strings that are not both ASCII.
#[inline]
pub fn cmp_utf16(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let n = a.len().min(b.len());
    let Some(i) = mismatch(&a[..n], &b[..n]) else {
        return a.len().cmp(&b.len());
    };
    // SAFETY: `i < n`, within both.
    let (x, y) = unsafe { (*a.get_unchecked(i), *b.get_unchecked(i)) };
    // Byte order is code-unit order unless the first difference is between the lead byte of a
    // supplementary character (F0..F4) and that of a character at U+E000 or above or a surrogate
    // (ED..EF): a difference in a continuation byte means equal leads, so equal lengths.
    if x.max(y) < 0xF0 || x.min(y) < 0xED {
        return x.cmp(&y);
    }
    cmp_at_leads(a, b, i)
}

/// [`cmp_utf16`] when the strings first differ in lead bytes at `i` that may order differently
/// by code units (out of line: rare).
#[cold]
#[inline(never)]
fn cmp_at_leads(a: &[u8], b: &[u8], i: usize) -> std::cmp::Ordering {
    let (ca, la) = wtf8::decode_at(a, i);
    let (cb, lb) = wtf8::decode_at(b, i);
    first_unit(ca)
        .cmp(&first_unit(cb))
        .then_with(|| second_unit(a, i, ca, la).cmp(&second_unit(b, i, cb, lb)))
}

/// The first byte where `a` and `b` (of equal length) differ, a word at a time (the last word
/// overlapping the one before when the length is not a multiple of 8).
#[inline]
fn mismatch(a: &[u8], b: &[u8]) -> Option<usize> {
    let n = a.len();
    // SAFETY (both reads): every word read lies within `0..n`, the length of both slices.
    let word = |at: usize| unsafe {
        (
            a.as_ptr().add(at).cast::<u64>().read_unaligned(),
            b.as_ptr().add(at).cast::<u64>().read_unaligned(),
        )
    };
    let differ = |at: usize, (x, y): (u64, u64)| {
        (x != y).then(|| at + (u64::from_le(x ^ y).trailing_zeros() / 8) as usize)
    };
    if n < 8 {
        return (0..n).find(|&j| a[j] != b[j]);
    }
    let mut i = 0;
    while i + 8 <= n {
        if let Some(k) = differ(i, word(i)) {
            return Some(k);
        }
        i += 8;
    }
    if i < n {
        return differ(n - 8, word(n - 8));
    }
    None
}

/// The first UTF-16 unit of a code point (or lone surrogate).
#[inline]
fn first_unit(cp: u32) -> u32 {
    if cp < 0x10000 {
        cp
    } else {
        0xD800 + ((cp - 0x10000) >> 10)
    }
}

/// The unit after the first one of the code point `cp` (`len` bytes at `k` of `s`): its low
/// surrogate, else the first unit of the next code point, else 0 for the end of the string
/// (below every unit, so the shorter string is less).
#[inline]
fn second_unit(s: &[u8], k: usize, cp: u32, len: usize) -> u32 {
    if cp >= 0x10000 {
        0xDC00 + ((cp - 0x10000) & 0x3FF) + 1
    } else if k + len < s.len() {
        first_unit(wtf8::decode_at(s, k + len).0) + 1
    } else {
        0
    }
}
