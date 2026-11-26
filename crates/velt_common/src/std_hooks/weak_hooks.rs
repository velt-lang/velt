//! The weak core's rows of the hook table (std/lang/hooks/weak*.velt, #3145; docs/internals/
//! design/weak-refs.md, rt_abi.md "Weak references"): the core under `WeakMap`, `WeakSet` and
//! `WeakRef` and the cold path of a weakly held object's release, all lowered for
//! `Construct::Weak` and prebuilt; and the table of function properties (fn_props.velt, #2196),
//! another process-wide table in std, lowered for `Construct::FnProps`. Kept apart from the rest of the table ([`super`]), which
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
            // fn_props.velt: the properties of function values (#924, #2196; rt_abi.md
            // "Function properties"), keyed by the function's environment (`env`), in a
            // program that writes through `any` (velt_vir `any_fn_props.rs`).
            /// The identity environment of a declared function's code, made on first use.
            fn_prop_identity => lang_hooks_fn_props, "fnPropIdentity", sig((u64) -> u64), lowered,
                constructs(FnProps), replaces("velt_rt_fnprop_identity");
            /// That identity if it was made, else 0.
            fn_prop_identity_of => lang_hooks_fn_props, "fnPropIdentityOf", sig((u64) -> u64),
                lowered, constructs(FnProps), replaces("velt_rt_fnprop_identity_of");
            /// `(env, key)`: the property's index, or -1; `fn_prop_at` gives a share of its value.
            fn_prop_index => lang_hooks_fn_props, "fnPropIndex", sig((u64, &string) -> i64), lowered,
                constructs(FnProps), replaces("velt_rt_fnprop_get");
            fn_prop_at => lang_hooks_fn_props, "fnPropAt", sig((u64, i64) -> any), lowered,
                constructs(FnProps);
            /// `(env, key, value)`: the property set, the value taken over.
            fn_prop_set => lang_hooks_fn_props, "fnPropSet", sig((u64, &string, move any) -> void),
                lowered, constructs(FnProps), replaces("velt_rt_fnprop_set");
            /// `(env, key)`: the property removed; whether there was one.
            fn_prop_delete => lang_hooks_fn_props, "fnPropDelete", sig((u64, &string) -> bool),
                lowered, constructs(FnProps), replaces("velt_rt_fnprop_delete");
            /// `(env)`: how many properties; `(env, i)`: the name of property `i`.
            fn_prop_len => lang_hooks_fn_props, "fnPropLen", sig((u64) -> u64), lowered,
                constructs(FnProps), replaces("velt_rt_fnprop_len");
            fn_prop_key => lang_hooks_fn_props, "fnPropKey", sig((u64, u64) -> string), lowered,
                constructs(FnProps), replaces("velt_rt_fnprop_key");
            /// The program's normal end: the identities released for the leak counters.
            fn_props_exit => lang_hooks_fn_props, "fnPropsExit", sig(() -> void), lowered,
                constructs(FnProps);
        }
    };
}
