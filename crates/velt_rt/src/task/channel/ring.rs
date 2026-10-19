//! A channel's queued items: a growable ring of fixed-size slots, one item per slot.
//!
//! Every item of a channel has the same size (a `Channel<T>` only holds `T`s), so a buffer that
//! is a whole number of slots never splits an item at its end: pushing or popping one is a
//! single `copy_nonoverlapping` of the item's bytes, whatever its size (#635; a byte-wise
//! `VecDeque<u8>` cost about 11 instructions per byte under the channel's mutex).

/// Items back to back in slots of `slot` bytes, the oldest at slot `head`.
pub(super) struct Ring {
    /// `slots() * slot` bytes; empty until the first non-empty item.
    buf: Vec<u8>,
    /// The item size, fixed by the first item pushed (0 before it, and for zero-sized items).
    slot: usize,
    /// Slot index of the oldest item.
    head: usize,
    /// Number of items (also for zero-sized items).
    len: usize,
}

impl Ring {
    /// Slots allocated by the first push.
    const FIRST_SLOTS: usize = 4;

    pub(super) const fn new() -> Ring {
        Ring {
            buf: Vec::new(),
            slot: 0,
            head: 0,
            len: 0,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The size of every item (0 before the first non-empty one).
    pub(super) fn item_size(&self) -> usize {
        self.slot
    }

    fn slots(&self) -> usize {
        self.buf.len().checked_div(self.slot).unwrap_or(0)
    }

    fn check_size(&mut self, size: usize) {
        if self.slot != size {
            assert!(
                self.slot == 0 && self.len == 0,
                "ICE: channel item of {size} bytes in a channel of {}-byte items",
                self.slot
            );
            self.slot = size;
        }
    }

    /// Append the `size` bytes at `src`.
    ///
    /// # Safety
    /// `src` must point to `size` readable bytes, not inside this ring.
    pub(super) unsafe fn push(&mut self, src: *const u8, size: usize) {
        if size != 0 {
            self.check_size(size);
            let slots = self.slots();
            if self.len == slots {
                self.grow(slots);
            }
            let slots = self.slots();
            let mut tail = self.head + self.len;
            if tail >= slots {
                tail -= slots;
            }
            // SAFETY: `tail < slots`, so the slot lies inside `buf`.
            std::ptr::copy_nonoverlapping(src, self.buf.as_mut_ptr().add(tail * size), size);
        }
        self.len += 1;
    }

    /// Move the oldest item (`size` bytes) to `dst`; the ring must not be empty.
    ///
    /// # Safety
    /// `dst` must point to `size` writable bytes, not inside this ring.
    pub(super) unsafe fn pop(&mut self, dst: *mut u8, size: usize) {
        assert!(self.len != 0, "ICE: pop from an empty channel ring");
        self.len -= 1;
        if size == 0 {
            return;
        }
        assert!(
            size == self.slot,
            "ICE: channel item of {size} bytes in a channel of {}-byte items",
            self.slot
        );
        // SAFETY: a non-empty ring has `head < slots`, so the slot lies inside `buf`.
        std::ptr::copy_nonoverlapping(self.buf.as_ptr().add(self.head * size), dst, size);
        self.head = if self.len == 0 || self.head + 1 == self.slots() {
            0
        } else {
            self.head + 1
        };
    }

    /// Double the slots (the ring is full), moving the items to the front in order.
    fn grow(&mut self, slots: usize) {
        let size = self.slot;
        let new_slots = (slots * 2).max(Self::FIRST_SLOTS);
        let mut buf = vec![0u8; new_slots * size];
        // Full: the items run from `head` to the end, then from the start up to `head`.
        let split = self.head * size;
        let first = self.buf.len() - split;
        buf[..first].copy_from_slice(&self.buf[split..]);
        buf[first..self.buf.len()].copy_from_slice(&self.buf[..split]);
        self.buf = buf;
        self.head = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::Ring;

    /// A `size`-byte item whose every byte encodes `n` and its position.
    fn item(n: usize, size: usize) -> Vec<u8> {
        (0..size).map(|i| (n * 31 + i) as u8).collect()
    }

    fn push(r: &mut Ring, n: usize, size: usize) {
        let v = item(n, size);
        // SAFETY: `v` holds `size` bytes.
        unsafe { r.push(v.as_ptr(), size) };
    }

    fn pop(r: &mut Ring, size: usize) -> Vec<u8> {
        let mut out = vec![0u8; size];
        // SAFETY: `out` holds `size` bytes.
        unsafe { r.pop(out.as_mut_ptr(), size) };
        out
    }

    #[test]
    fn large_and_odd_sized_items_come_out_whole_and_in_order() {
        for size in [1, 3, 8, 37, 96, 4099] {
            let mut r = Ring::new();
            for n in 0..50 {
                push(&mut r, n, size);
            }
            assert_eq!(r.len(), 50);
            for n in 0..50 {
                assert_eq!(pop(&mut r, size), item(n, size), "size {size}, item {n}");
            }
            assert!(r.is_empty());
        }
    }

    #[test]
    fn items_wrap_around_the_end_and_survive_growing_while_wrapped() {
        let size = 13;
        let mut r = Ring::new();
        let (mut next_in, mut next_out) = (0, 0);
        // Keep 3 of the 4 first slots busy so the tail wraps past the end many times.
        for _ in 0..3 {
            push(&mut r, next_in, size);
            next_in += 1;
        }
        for _ in 0..21 {
            push(&mut r, next_in, size);
            next_in += 1;
            assert_eq!(pop(&mut r, size), item(next_out, size));
            next_out += 1;
        }
        assert_eq!(r.slots(), Ring::FIRST_SLOTS, "steady state never grows");
        assert_ne!(r.head, 0, "the items wrap");
        // Grow while the items wrap: they must keep their order.
        for _ in 0..10 {
            push(&mut r, next_in, size);
            next_in += 1;
        }
        while !r.is_empty() {
            assert_eq!(pop(&mut r, size), item(next_out, size));
            next_out += 1;
        }
        assert_eq!(next_out, next_in);
    }

    #[test]
    fn zero_sized_items_are_counted_without_storage() {
        let mut r = Ring::new();
        for _ in 0..5 {
            // SAFETY: zero bytes are read.
            unsafe { r.push(std::ptr::NonNull::dangling().as_ptr(), 0) };
        }
        assert_eq!((r.len(), r.buf.len()), (5, 0));
        for _ in 0..5 {
            // SAFETY: zero bytes are written.
            unsafe { r.pop(std::ptr::NonNull::dangling().as_ptr(), 0) };
        }
        assert!(r.is_empty());
    }
}
