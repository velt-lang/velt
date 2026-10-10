//! Weakly held objects are thread-bound: a release on a thread without their record is an ICE.

use super::*;

/// Releases `obj` on a new thread through the cold path; returns the panic message.
fn release_elsewhere(obj: *mut u8) -> String {
    let addr = obj as usize;
    // SAFETY: the release panics before it touches the object (the point of the test).
    let err = std::thread::spawn(move || unsafe { weak_release(addr as *mut u8) })
        .join()
        .expect_err("a release on another thread must be reported");
    match err.downcast::<String>() {
        Ok(msg) => *msg,
        Err(err) => err
            .downcast_ref::<&str>()
            .map_or_else(String::new, |s| s.to_string()),
    }
}

#[test]
fn a_weakly_held_object_released_on_another_thread_is_an_ice() {
    no_leak(|| {
        let m = obj_map();
        let raw = new_obj(&[]);
        set(m, raw, new_obj(&[]));
        // SAFETY: `raw` is live.
        let r = unsafe { velt_rt_weakref_new(raw) };
        // The last reference (the free path), then a shared one (the decrement path).
        for n in [1, 2] {
            if n == 2 {
                retain(raw);
            }
            let msg = release_elsewhere(raw);
            assert!(msg.starts_with("ICE: weakly held object"), "{msg}");
            assert_eq!(count(raw), n, "the count is untouched");
            assert!(is_marked(raw));
            assert_eq!(weakmap_len(m), 1, "the entry is kept");
        }
        assert_eq!(velt_rt_weakref_deref(r), raw);
        release(raw); // the deref's reference
        release(raw);
        release(raw);
        assert_eq!(weakmap_len(m), 0);
        assert!(velt_rt_weakref_deref(r).is_null());
        // SAFETY: `r` is dropped once.
        unsafe { velt_rt_weakref_drop(r) };
        finish(&[m]);
    });
}
