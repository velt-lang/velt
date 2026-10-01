//! Opaque object handles at the C ABI (rt_abi_async.md §3.2).
//!
//! The standard library holds runtime objects (sockets, regexes, child processes, HTTP requests,
//! `json.Value` nodes …) as `u64` fields and passes them to the runtime as `u64` arguments: Velt
//! has no pointer type, and a `u64` is the same width everywhere, so the `declare function` in
//! `std/*.vlt` and the definition here have the same signature even on wasm32, where
//! WebAssembly links only exactly matching signatures. A handle carries the object's address
//! (0 = none); [`Handle`] is `repr(transparent)` over that `u64`, so it *is* a `u64` in every
//! `extern "C"` signature while keeping the pointee type on the Rust side.

use std::marker::PhantomData;
use std::sync::Arc;

/// A `u64` holding the address of a `T` (0 = none): an `Arc<T>` turned into a raw pointer
/// (shared objects that in-flight operations keep alive: sockets, regexes, JSON nodes …) or a
/// `Box<T>` (objects with a single owner: HTTP requests and responses …).
#[repr(transparent)]
pub struct Handle<T>(u64, PhantomData<*const T>);

// SAFETY: a handle is a number; every access to the object behind it is `unsafe` and follows the
// object's own rules (the runtime objects behind handles are `Send + Sync`).
unsafe impl<T> Send for Handle<T> {}
// SAFETY: as for `Send`.
unsafe impl<T> Sync for Handle<T> {}

impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Handle<T> {}

impl<T> std::fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Handle({:#x})", self.0)
    }
}

impl<T> Handle<T> {
    /// The null handle (what a failed lookup returns).
    pub const NULL: Self = Handle(0, PhantomData);

    /// The handle of an object address.
    pub fn from_ptr(p: *const T) -> Self {
        Handle(p as usize as u64, PhantomData)
    }

    /// A new handle owning `value`'s reference.
    pub fn from_arc(value: Arc<T>) -> Self {
        Self::from_ptr(Arc::into_raw(value))
    }

    /// A new handle owning `value`.
    pub fn from_box(value: Box<T>) -> Self {
        Self::from_ptr(Box::into_raw(value))
    }

    /// The object's address (null for the null handle).
    pub fn ptr(self) -> *const T {
        self.0 as usize as *const T
    }

    /// The raw `u64`, as Velt code sees it.
    pub fn bits(self) -> u64 {
        self.0
    }

    /// Whether this is the null handle.
    pub fn is_null(self) -> bool {
        self.0 == 0
    }

    /// The object, or `None` for the null handle.
    ///
    /// # Safety
    /// A non-null handle must point to a live `T` for `'a`.
    pub unsafe fn get<'a>(self) -> Option<&'a T> {
        self.ptr().as_ref()
    }

    /// The object behind a handle the ABI requires to be valid (non-null).
    ///
    /// # Safety
    /// The handle must point to a live `T` for `'a`.
    pub unsafe fn obj<'a>(self) -> &'a T {
        &*self.ptr()
    }

    /// The object behind a valid `Box<T>` handle, mutably.
    ///
    /// # Safety
    /// The handle must point to a live `T` that nothing else accesses during `'a`.
    pub unsafe fn obj_mut<'a>(self) -> &'a mut T {
        &mut *(self.ptr() as *mut T)
    }

    /// Take back the object of a `Box<T>` handle (the handle is dead afterwards).
    ///
    /// # Safety
    /// The handle must be a live, non-null `Box<T>` handle, not used afterwards.
    pub unsafe fn into_box(self) -> Box<T> {
        Box::from_raw(self.ptr() as *mut T)
    }

    /// A new `Arc` to the object; the handle keeps its own reference.
    ///
    /// # Safety
    /// The handle must be a live, non-null `Arc<T>` handle.
    pub unsafe fn clone_arc(self) -> Arc<T> {
        Arc::increment_strong_count(self.ptr());
        Arc::from_raw(self.ptr())
    }

    /// Give up the handle's reference (the object is freed with its last one). Null is a no-op.
    ///
    /// # Safety
    /// A non-null handle must be a live `Arc<T>` handle, not used afterwards.
    pub unsafe fn release(self) {
        if !self.is_null() {
            drop(Arc::from_raw(self.ptr()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_handles_round_trip() {
        let h = Handle::from_arc(Arc::new(7u32));
        assert_eq!(std::mem::size_of::<Handle<u32>>(), 8);
        unsafe {
            assert_eq!(*h.obj(), 7);
            let a = h.clone_arc();
            h.release();
            assert_eq!(*a, 7);
            assert_eq!(Arc::strong_count(&a), 1);
            assert!(Handle::<u32>::NULL.get().is_none());
            Handle::<u32>::NULL.release();
            let b = Handle::from_box(Box::new(String::from("a")));
            b.obj_mut().push('b');
            assert_eq!(*b.into_box(), "ab");
        }
    }
}
