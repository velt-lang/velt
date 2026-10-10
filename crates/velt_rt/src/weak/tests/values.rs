//! Map values that are null or not traced, and `get` handing out a counted reference.

use super::*;

/// A map of `Obj` values without trace glue (values never refer back to keys).
fn untraced_map() -> MapId {
    velt_rt_weakmap_new(
        Some(TraceFn(trace_obj)),
        Some(retain_value),
        Some(release_value),
        None,
    )
}

#[test]
fn a_null_value_in_a_traced_map() {
    // `WeakMap<object, Proxy | undefined>`: insert must not trace the 0 word.
    no_leak(|| {
        let m = obj_map();
        let k = new_obj(&[]);
        set(m, k, std::ptr::null_mut());
        assert_eq!(get(m, k), Some(std::ptr::null_mut()));
        // SAFETY: `m` is live.
        assert_eq!(unsafe { velt_rt_weakmap_delete(m, k) }, 1);
        set(m, k, std::ptr::null_mut());
        release(k);
        assert_eq!(weakmap_len(m), 0);
        finish(&[m]);
    });
}

#[test]
fn null_and_untraced_values_of_a_key_in_a_cycle() {
    // A trial through `raw` meets its entries in the other maps: a null value (traced map and
    // untraced map) and an object value of an untraced map. None joins the trial graph, and the
    // cycle is still freed in either order.
    for raw_first in [true, false] {
        no_leak(|| {
            let (cache, traced, untraced) = (obj_map(), obj_map(), untraced_map());
            let raw = new_obj(&[]);
            let p = proxy_of(cache, raw, std::ptr::null_mut());
            set(traced, raw, std::ptr::null_mut());
            set(untraced, raw, std::ptr::null_mut());
            let other = new_obj(&[]);
            set(untraced, other, new_obj(&[]));
            let side = untraced_map();
            set(side, raw, new_obj(&[]));
            let refs = if raw_first { [raw, p] } else { [p, raw] };
            for r in refs {
                release(r);
            }
            assert_eq!(weakmap_len(cache) + weakmap_len(traced), 0);
            assert_eq!(weakmap_len(untraced), 1, "only `other`'s entry is left");
            assert_eq!(weakmap_len(side), 0);
            release(other);
            finish(&[cache, traced, untraced, side]);
        });
    }
}

#[test]
fn a_got_value_survives_an_unrelated_release() {
    // `const p = cache.get(raw); raw = null; use(p)`: releasing `raw` runs a trial, which must
    // see the reference `get` returned and keep the proxy.
    no_leak(|| {
        let m = obj_map();
        let raw = new_obj(&[]);
        release(proxy_of(m, raw, std::ptr::null_mut()));
        let p = get(m, raw).expect("cached");
        let before = live();
        release(raw);
        assert_eq!(live(), before, "the proxy and its target are alive");
        assert_eq!(field(p, 0), raw);
        assert_eq!(weakmap_len(m), 1);
        release(p);
        assert_eq!(live(), before - 2);
        assert_eq!(weakmap_len(m), 0);
        finish(&[m]);
    });
}
