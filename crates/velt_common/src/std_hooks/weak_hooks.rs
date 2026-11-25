//! The weak core's rows of the hook table (std/lang/hooks/weak*.velt, #3145; docs/internals/
//! design/weak-refs.md, rt_abi.md "Weak references"): the core under `WeakMap`, `WeakSet` and
//! `WeakRef` and the cold path of a weakly held object's release, all lowered for
//! `Construct::Weak` and prebuilt. Kept apart from the rest of the table ([`super`]), which
//! [`with_weak_hooks`] gives to `hooks!` with these rows after it (and the yields' after those,
//! `yield_hooks.rs`).

/// `hooks!` over the rows given, then the weak core's, then the yields'.
macro_rules! with_weak_hooks {
    ($($rows:tt)*) => {
        with_yield_hooks! {
            $($rows)*
            // weak.velt: the weak core (#3145; design/weak-refs.md, rt_abi.md "Weak references").
            /// A weakly held object's cold release: `(obj)`, true when that was the last reference
            /// (the caller drops and frees it).
            weak_release => lang_hooks_weak, "weakRelease", sig((usize) -> bool), lowered, constructs(Weak),
                replaces("velt_rt_weak_release"), prebuilt;
            /// A new weak map: `(key trace, value retain, value release, value trace)`, its handle.
            weak_map_new => lang_hooks_weak, "weakMapNew", sig((usize, usize, usize, usize) -> u32),
                lowered, constructs(Weak), replaces("velt_rt_weakmap_new"), prebuilt;
            /// `map.set(key, value)`: `(map, key, value word)`, the value's reference taken over.
            weak_map_set => lang_hooks_weak, "weakMapSet", sig((u32, usize, u64) -> void), lowered, constructs(Weak),
                replaces("velt_rt_weakmap_set"), prebuilt;
            /// `weak_map_set` with the key's own trace glue (a `WeakMap<object, V>`).
            weak_map_set_traced => lang_hooks_weak, "weakMapSetTraced", sig((u32, usize, u64, usize) -> void),
                lowered, constructs(Weak), replaces("velt_rt_weakmap_set_traced"), prebuilt;
            /// `map.get(key)`: the value word, retained, or 0.
            weak_map_get => lang_hooks_weak, "weakMapGet", sig((u32, usize) -> u64), lowered, constructs(Weak),
                replaces("velt_rt_weakmap_get"), prebuilt;
            /// `map.has(key)`.
            weak_map_has => lang_hooks_weak, "weakMapHas", sig((u32, usize) -> bool), lowered, constructs(Weak),
                replaces("velt_rt_weakmap_has"), prebuilt;
            /// `map.delete(key)`: whether there was an entry.
            weak_map_delete => lang_hooks_weak, "weakMapDelete", sig((u32, usize) -> bool), lowered, constructs(Weak),
                replaces("velt_rt_weakmap_delete"), prebuilt;
            /// A weak map's drop (std's `WeakMap` and `WeakSet`, through `weakMapDrop`'s intrinsic).
            weak_map_drop => lang_hooks_weak, "weakMapDrop", sig((u32) -> void), lowered,
                constructs(Weak), replaces("velt_rt_weakmap_drop"), prebuilt;
            /// `new WeakRef(obj)`: its handle.
            weak_ref_new => lang_hooks_weak, "weakRefNew", sig((usize) -> u32), lowered, constructs(Weak),
                replaces("velt_rt_weakref_new"), prebuilt;
            /// `new WeakRef(obj)` keeping a word for the compiler (an `object` target's descriptor).
            weak_ref_new_with => lang_hooks_weak, "weakRefNewWith", sig((usize, u64) -> u32), lowered, constructs(Weak),
                replaces("velt_rt_weakref_new_with"), prebuilt;
            /// The word `weak_ref_new_with` kept.
            weak_ref_word => lang_hooks_weak, "weakRefWord", sig((u32) -> u64), lowered, constructs(Weak),
                replaces("velt_rt_weakref_word"), prebuilt;
            /// `ref.deref()`: the target's word, retained, or 0.
            weak_ref_deref => lang_hooks_weak, "weakRefDeref", sig((u32) -> usize), lowered, constructs(Weak),
                replaces("velt_rt_weakref_deref"), prebuilt;
            /// A `WeakRef`'s drop (std's `WeakRef`, through `weakRefDrop`'s intrinsic).
            weak_ref_drop => lang_hooks_weak, "weakRefDrop", sig((u32) -> void), lowered,
                constructs(Weak), replaces("velt_rt_weakref_drop"), prebuilt;
            /// The slot of a `weak T` reference to `(obj)`, with one more reference
            /// (weak_slots.velt; cycles.md "`weak T`").
            weak_slot => lang_hooks_weak, "weakSlot", sig((usize) -> usize), lowered, constructs(Weak),
                prebuilt;
            /// A `weak T` field's or variable's reference to `(slot)` given up.
            weak_slot_drop => lang_hooks_weak, "weakSlotDrop", sig((usize) -> void), lowered,
                constructs(Weak), prebuilt;
        }
    };
}
