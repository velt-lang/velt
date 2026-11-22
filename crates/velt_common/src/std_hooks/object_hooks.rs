//! The rows of the hook table for `Object.*` and the other object operations of std code in
//! Velt: those of an `any` (std/lang/hooks/any_object.velt) and the errors of a write to a frozen
//! object (std/lang/hooks/frozen.velt, #2623; `velt_sema` `freezable.rs`: a field write of a
//! freezable type tests the object's frozen mark and calls one, an ordinary throw). Kept apart
//! from the rest of the table ([`super`]), which [`with_object_hooks`] gives on, with these rows
//! after it, to `with_decorator_hooks!`.

/// `with_decorator_hooks!` over the rows given, then these.
macro_rules! with_object_hooks {
    ($($rows:tt)*) => {
        with_decorator_hooks! {
            $($rows)*
            // any_object.velt: `Object.*`, spreads, prototypes and `delete` of an `any`.
            /// `{ ...o }` of an `any` in an object literal (`velt_sema` `record_literal.rs`).
            spread_any => lang_hooks_any_object, "spreadAny", sig((&mut Record(string, any), &any) -> void);
            /// `const { a, ...rest } = o` of an `any` (`velt_sema` `literal_patterns.rs`).
            rest_any => lang_hooks_any_object, "restAny", sig((&any, &any) -> any);
            /// `Object.hasOwn(o, k)` of an `any` (`velt_sema` `object_own.rs`).
            any_has_own => lang_hooks_any_object, "anyHasOwn", sig((&any, &union(string, symbol)) -> bool);
            /// Member `k` (one of `Object.keys(o)`) of an `any`: std's `spreadAny` & co. call it, not the compiler.
            any_member => lang_hooks_any_object, "anyMember", sig((&any, &string) -> any);
            /// `Object.values(o)` of an `any` (`velt_sema` `object_keys.rs`).
            any_values => lang_hooks_any_object, "anyValues", sig((&any) -> [any]);
            /// `Object.entries(o)` of an `any` (`velt_sema` `object_keys.rs`).
            any_entries => lang_hooks_any_object, "anyEntries", sig((&any) -> [tuple(string, any)]);
            /// `Object.getPrototypeOf(v)` of an `any` holding no class instance (`object_prototypes.rs`).
            any_prototype => lang_hooks_any_object, "anyPrototype", sig((&any) -> or_null(ObjectPrototype));
            /// `Object.getPrototypeOf(r)` of an `any` holding a record.
            any_record_prototype => lang_hooks_any_object, "anyRecordPrototype", sig((&any) -> or_null(ObjectPrototype));
            /// `Object.getPrototypeOf(v)` of an instance of a class Velt doesn't name: an error.
            any_prototype_unsupported => lang_hooks_any_object, "anyPrototypeUnsupported", sig((&string) -> or_null(ObjectPrototype));
            /// `Object.prototype.toString.call(v)` of an `any` holding no class instance (`object_own.rs`).
            any_object_tag => lang_hooks_any_object, "anyObjectTag", sig((&any) -> string);
            /// `Object.prototype.toString.call(v)` of an instance whose tag Velt doesn't know: an error.
            any_tag_unsupported => lang_hooks_any_object, "anyTagUnsupported", sig((&string) -> string);
            /// `v.constructor.name` of an `any` holding no class instance (`velt_sema` `ctor_name.rs`).
            any_ctor_name => lang_hooks_any_object, "anyCtorName", sig((&any) -> string);
            /// `v.constructor.name` of an instance whose constructor Velt doesn't name: an error.
            any_ctor_name_unsupported => lang_hooks_any_object, "anyCtorNameUnsupported", sig((&string) -> string);
            /// `delete o.k` / `delete o[k]` of an `any` that is no proxy (`velt_sema` `proxy.rs`).
            any_delete_member => lang_hooks_any_object, "anyDeleteMember", sig((&any, &any) -> bool);
            /// `Object.prototype.hasOwnProperty` as a value (`object_prototypes.rs`, `velt_vir` `any_proxy.rs`).
            has_own_property => lang_hooks_any_object, "HasOwnProperty", class, lowered;

            // frozen.velt: a write to a field of a frozen object (`Object.freeze`), on demand.
            /// A write to a field of a frozen object: Node's `TypeError` of a strict-mode write.
            frozen_write_error => lang_hooks_frozen, "frozenWriteError", sig((&string) -> void throws TypeError);
            /// `Object.defineProperty` of a frozen object's field: Node's `TypeError`.
            frozen_define_error => lang_hooks_frozen, "frozenDefineError", sig((&string) -> void throws TypeError);
            /// A write adding an absent optional field to a frozen object: Node's `TypeError`.
            frozen_add_error => lang_hooks_frozen, "frozenAddError", sig((&string) -> void throws TypeError);
            /// `Object.defineProperty` of an absent optional field on a frozen object.
            frozen_define_add_error => lang_hooks_frozen, "frozenDefineAddError", sig((&string) -> void throws TypeError);
        }
    };
}
