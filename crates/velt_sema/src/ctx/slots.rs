//! EXPERIMENT (#2943 M2): a vector whose slots may be empty until first read ([`Slots::set`]
//! through a shared reference), for `DefTable`'s entries decoded on demand. Unlike a
//! `Vec<OnceCell<T>>` it is covariant in `T` (a `DefTable<DefInfo<'m>>` is one for a shorter
//! `'m`, as a `Vec` is): the slots are behind a raw pointer, not an `UnsafeCell`.
//!
//! Sound because a slot is written once, while empty, and no reference to an empty slot is
//! ever made; a filled slot is only read through `&self` and written through `&mut self`. Not
//! `Send` or `Sync` (the raw pointer), like the checker's context that holds it.

use std::cell::Cell;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::ptr::NonNull;

pub(crate) struct Slots<T> {
    ptr: NonNull<MaybeUninit<T>>,
    len: usize,
    cap: usize,
    /// Frees the slots: a field of a type without `T`, so that, as with a `Vec`, a `T` that
    /// borrows (`DefInfo<'m>`) need not outlive the table (a `Drop` for `Slots<T>` would need
    /// `may_dangle`, which stable Rust doesn't have). Declared before `ready`, which it reads:
    /// fields are dropped in order.
    free: Free,
    /// Whether each slot is filled.
    ready: Vec<Cell<bool>>,
    _owns: PhantomData<T>,
}

/// Drops the filled slots of a [`Slots`] and frees its allocation.
struct Free {
    ptr: *mut u8,
    cap: usize,
    /// The table's `len` and `ready`, kept by the table itself ([`Slots::sync`]).
    len: usize,
    ready: *const Cell<bool>,
    drop: unsafe fn(*mut u8, usize, usize, *const Cell<bool>),
}

impl Drop for Free {
    fn drop(&mut self) {
        // SAFETY: `drop` is `drop_slots::<T>` for the table's `T`, over its own parts.
        unsafe { (self.drop)(self.ptr, self.len, self.cap, self.ready) }
    }
}

/// Drop the filled ones of `len` slots of `T` at `ptr` (`ready` says which), then free the
/// allocation of `cap`.
unsafe fn drop_slots<T>(ptr: *mut u8, len: usize, cap: usize, ready: *const Cell<bool>) {
    let ptr = ptr.cast::<MaybeUninit<T>>();
    for i in 0..len {
        // SAFETY: the caller's parts: `ready` has `len` flags, and a filled slot is initialized.
        unsafe {
            if (*ready.add(i)).get() {
                (*ptr.add(i)).assume_init_drop();
            }
        }
    }
    // SAFETY: the allocation's own parts (its slots are dropped above).
    drop(unsafe { Vec::from_raw_parts(ptr, 0, cap) });
}

impl<T> Slots<T> {
    fn from_vec(v: Vec<MaybeUninit<T>>, ready: Vec<Cell<bool>>) -> Self {
        let mut v = std::mem::ManuallyDrop::new(v);
        let (len, cap) = (v.len(), v.capacity());
        let ptr = NonNull::new(v.as_mut_ptr()).unwrap_or(NonNull::dangling());
        let free = Free {
            ptr: ptr.as_ptr().cast(),
            cap,
            len,
            ready: ready.as_ptr(),
            drop: drop_slots::<T>,
        };
        Slots {
            ptr,
            len,
            cap,
            ready,
            free,
            _owns: PhantomData,
        }
    }

    /// Point [`Free`] at the parts as they are now.
    fn sync(&mut self) {
        self.free.ptr = self.ptr.as_ptr().cast();
        self.free.cap = self.cap;
        self.free.len = self.len;
        self.free.ready = self.ready.as_ptr();
    }


    /// `n` empty slots.
    pub(crate) fn empty(n: usize) -> Self {
        let v = (0..n).map(|_| MaybeUninit::uninit()).collect();
        Slots::from_vec(v, (0..n).map(|_| Cell::new(false)).collect())
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Slot `i`, when it is filled (panics past the end).
    #[inline]
    pub(crate) fn get(&self, i: usize) -> Option<&T> {
        assert!(i < self.len, "ICE: definition {i} past the table's end");
        // SAFETY: in bounds; a filled slot is initialized and only written through `&mut self`.
        self.ready[i]
            .get()
            .then(|| unsafe { (*self.ptr.as_ptr().add(i)).assume_init_ref() })
    }

    /// Fill empty slot `i` (a filled one is left as it is).
    pub(crate) fn set(&self, i: usize, x: T) {
        assert!(i < self.len, "ICE: definition {i} past the table's end");
        if self.ready[i].get() {
            return;
        }
        // SAFETY: in bounds, and nothing refers to an empty slot; the pointer is the
        // allocation's own (not derived from a shared reference).
        unsafe { self.ptr.as_ptr().add(i).write(MaybeUninit::new(x)) };
        self.ready[i].set(true);
    }

    /// Slot `i` to write, when it is filled.
    pub(crate) fn get_mut(&mut self, i: usize) -> Option<&mut T> {
        assert!(i < self.len, "ICE: definition {i} past the table's end");
        // SAFETY: as in `get`, with `&mut self`.
        self.ready[i]
            .get()
            .then(|| unsafe { (*self.ptr.as_ptr().add(i)).assume_init_mut() })
    }

    pub(crate) fn push(&mut self, x: T) {
        // SAFETY: the parts are this vector's own; it is taken apart again at once.
        let mut v = std::mem::ManuallyDrop::new(unsafe {
            Vec::from_raw_parts(self.ptr.as_ptr(), self.len, self.cap)
        });
        v.push(MaybeUninit::new(x));
        self.ptr = NonNull::new(v.as_mut_ptr()).unwrap_or(NonNull::dangling());
        (self.len, self.cap) = (v.len(), v.capacity());
        self.ready.push(Cell::new(true));
        self.sync();
    }

    /// Every slot's value, the empty ones filled with `fill(i)` first.
    pub(crate) fn into_vec_filling(self, fill: impl Fn(usize) -> T) -> Vec<T> {
        for i in 0..self.len {
            if !self.ready[i].get() {
                self.set(i, fill(i));
            }
        }
        self.into_vec()
    }

    /// Every slot's value, when all are filled (panics otherwise).
    pub(crate) fn into_vec(self) -> Vec<T> {
        assert!(
            self.ready.iter().all(Cell::get),
            "ICE: a definition table taken with entries not decoded"
        );
        let (ptr, len, cap) = (self.ptr.as_ptr(), self.len, self.cap);
        let mut me = std::mem::ManuallyDrop::new(self);
        // The flags are freed here; the slots go to the vector.
        drop(std::mem::take(&mut me.ready));
        // SAFETY: every slot is initialized, and `MaybeUninit<T>` has `T`'s layout.
        unsafe { Vec::from_raw_parts(ptr.cast::<T>(), len, cap) }
    }
}

impl<T: Clone> Clone for Slots<T> {
    fn clone(&self) -> Self {
        let mut v = Vec::with_capacity(self.len);
        for i in 0..self.len {
            v.push(match self.get(i) {
                Some(x) => MaybeUninit::new(x.clone()),
                None => MaybeUninit::uninit(),
            });
        }
        Slots::from_vec(v, self.ready.clone())
    }
}

impl<T> FromIterator<T> for Slots<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let v: Vec<MaybeUninit<T>> = iter.into_iter().map(MaybeUninit::new).collect();
        let ready = (0..v.len()).map(|_| Cell::new(true)).collect();
        Slots::from_vec(v, ready)
    }
}

impl<T> Default for Slots<T> {
    fn default() -> Self {
        Slots::empty(0)
    }
}

#[cfg(test)]
mod tests {
    use super::Slots;

    #[test]
    fn slots_fill_once_and_drop_only_what_was_filled() {
        let s: Slots<String> = Slots::empty(3);
        assert!(s.get(1).is_none());
        s.set(1, "b".into());
        s.set(1, "ignored".into());
        assert_eq!(s.get(1).map(String::as_str), Some("b"));
        let mut t = s.clone();
        t.push("d".into());
        assert_eq!(t.len(), 4);
        assert_eq!(t.get(3).map(String::as_str), Some("d"));
        *t.get_mut(1).unwrap() = "B".into();
        assert_eq!(s.get(1).map(String::as_str), Some("b"));
        t.set(0, "a".into());
        t.set(2, "c".into());
        assert_eq!(t.into_vec(), ["a", "B", "c", "d"]);
    }
}
