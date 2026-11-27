//! The rows of the hook table for operations on `any` values holding objects: their text
//! (any_text.velt), a record view (any_record.velt), operators (any_ops.velt) and classes as
//! values (class_object.velt); `Object.*` and spreads are object_hooks.rs's. Kept apart from
//! the rest of the table ([`super`]), which [`with_any_hooks`] gives on, with these rows after it,
//! to `with_object_hooks!`.

/// `with_object_hooks!` over the rows given, then the `any` operations'.
macro_rules! with_any_hooks {
    ($($rows:tt)*) => {
        with_object_hooks! {
            $($rows)*
            // any_text.velt: the text of an `any` (`velt_sema` `any_text.rs`); lowering keeps one
            // instance of each and passes the call site (`velt_vir` `track_caller.rs`).
            /// `String(v)` and `${v}` of an `any`: sema writes the dispatch over the program's classes.
            any_string => lang_hooks_any_text, "anyString", sig((&any, &string) -> string), lowered;
            /// `v + ""` / `s += v` of an `any` (hint `"default"`): sema writes the class dispatch.
            any_concat_text => lang_hooks_any_text, "anyConcatText", sig((&any, &string) -> string), lowered;
            /// `any_concat_text` of a value no class method of the program writes.
            any_concat_plain => lang_hooks_any_text, "anyConcatPlain", sig((move any, &string) -> string), lowered;
            /// The text of what a `toString()`, `valueOf()` or `[Symbol.toPrimitive]` returned.
            any_prim_text => lang_hooks_any_text, "anyPrimText", sig((move any, &string) -> string), lowered;
            /// `any_prim_text` of a primitive, `null` for an object (`ToPrimitive` tries the next method).
            any_prim_try => lang_hooks_any_text, "anyPrimTry", sig((move any, &string) -> or_null(string)), lowered;
            /// `String(p)` of a proxy through `any`, which reports that it is not supported yet.
            any_text_unsupported => lang_hooks_any_text, "anyTextUnsupported", sig((&string, &string) -> string), lowered;
            /// `String(v)` of an `any` holding no class instance with `toString()` or `[Symbol.toPrimitive]`.
            any_text => lang_hooks_any_text, "anyText", sig((move any, &string) -> string), lowered;
            /// `String(r)` of an `any` holding a record, where `Object.create(null)` is called.
            any_record_text => lang_hooks_any_text, "anyRecordText", sig((move any, &string) -> string);
            /// `r + ""` of an `any` holding a record, where `Object.create(null)` is called.
            any_record_concat => lang_hooks_any_text, "anyRecordConcat", sig((move any, &string) -> string);
            /// `xs.join(sep)` and `String(xs)` of an array of `any`, `unknown` or `object` values.
            any_join => lang_hooks_any_text, "anyJoin", sig((move any, &string, &string) -> string), lowered;
            /// `xs.join(sep)` of an `any` `xs` (`velt_sema` `dynamic_member.rs`).
            any_join_method => lang_hooks_any_text, "anyJoinMethod", sig((move any, move any, &string) -> any);
            /// `xs.join()` of an `any` `xs`, with no argument written.
            any_join_none => lang_hooks_any_text, "anyJoinNone", sig((move any, &string) -> any);
            /// The list of `f.apply(self, list)` read at the call (`velt_sema` `dynamic_member.rs`).
            any_apply_list => lang_hooks_any_ops, "anyApplyList", sig((&any, move any) -> any throws union(TypeError, RangeError));
            /// A typed array as that list, in a program that has them.
            typed_apply_list => lang_typed_array_apply_list, "typedApplyList", sig((&any, move any) -> any);
            /// A typed array behind a proxy as that list, in a program that has both.
            typed_proxy_list => lang_typed_array_apply_list, "typedProxyList", sig((&any, &any, move any) -> any throws TypeError);
            /// The same outside a `try`: the error ends the program, with no `throws` for the caller.
            typed_proxy_stop => lang_typed_array_apply_list, "typedProxyStop", sig((&any, &any, move any) -> any);
            proxy_innermost => lang_proxy_traps, "proxyInnermost", sig((&any) -> any);
            /// The same in a program with `Proxy`, where reading the list can run traps.
            proxy_apply_list => lang_proxy_traps, "proxyApplyList", sig((&any, move any) -> any throws union(TypeError, RangeError));
            /// `any_join` with the arrays being joined around it, so a cycle prints as empty text.
            any_join_seen => lang_hooks_any_text, "anyJoinSeen", sig((move any, &string, &mut [any], &string) -> string), lowered;

            // any_record.velt: an `any` holding another object converted to a record (`velt_vir`
            // `any_to_record.rs`), loaded for a program that mentions `Record`
            // (std/prelude/global/record_any.velt).
            /// `x as Record<string, any>` of an `any` holding an object that is no such record: a copy.
            any_to_record_any => lang_hooks_any_record, "anyToRecordAny", sig((&any) -> Record(string, any)), lowered;
            /// `x as Record<string, V>` of such an `any`: `any_to_record_any`, each value a `V`.
            any_to_record => lang_hooks_any_record, "anyToRecord", sig(<A>(&any) -> Record(string, A)), lowered;
            /// The source of a record view (`velt_vir` `record_view.rs`): the object an `any` held.
            record_view_source => lang_hooks_any_record, "RecordViewSource", class, lowered;
            /// The source of a view of the object an `any` holds.
            record_view_new => lang_hooks_any_record, "recordViewNew", sig((move any) -> RecordViewSource), lowered;
            /// A view's record, its mirror filled from the source.
            record_view_make => lang_hooks_any_record, "recordViewMake", sig((&RecordViewSource) -> Record(string, any)), lowered;
            /// The source's own entry for one key of a view, or `undefined`.
            record_view_get_one => lang_hooks_any_record, "recordViewGetOne", sig((&RecordViewSource, &string) -> any), lowered;
            /// Whether a view's source has one own key.
            record_view_has_one => lang_hooks_any_record, "recordViewHasOne", sig((&RecordViewSource, &string) -> bool), lowered;
            /// A value written to a view's source under one key.
            record_view_set_one => lang_hooks_any_record, "recordViewSetOne", sig((&RecordViewSource, &string, move any) -> void), lowered;
            /// A key deleted from a view's source.
            record_view_delete_one => lang_hooks_any_record, "recordViewDeleteOne", sig((&RecordViewSource, move string) -> void), lowered;
            /// A view's mirror rebuilt from its source before a read of the whole record.
            record_view_sync_all => lang_hooks_any_record, "recordViewSyncAll", sig((&RecordViewSource) -> Map(string, any)), lowered;
            /// A view's mirror entries for another record's keys written to its source after a spread.
            record_view_write_all => lang_hooks_any_record, "recordViewWriteAll", sig((&RecordViewSource, &Map(string, any), move Map(string, any)) -> void), lowered;

            // any_ops.velt: operators on `any` values holding objects (`ToPrimitive` first).
            /// `ToPrimitive(v, hint)` of an `any` holding an object or a function.
            any_primitive => lang_hooks_any_ops, "anyPrimitive", sig((&any, &string, &string) -> any), lowered;
            /// `ToPrimitive(v, hint)` of an object no class method of the program answers (`any_prim.rs`).
            any_primitive_plain => lang_hooks_any_ops, "anyPrimitivePlain", sig((move any, &string, &string) -> any), lowered;
            /// What a `valueOf()`, `toString()` or `[Symbol.toPrimitive]` returned, checked a primitive.
            any_prim_keep => lang_hooks_any_ops, "anyPrimKeep", sig((move any, &string) -> any), lowered;
            /// A method's result when it is a primitive, else the next method's (`velt_sema` `any_prim.rs`).
            any_prim_then => lang_hooks_any_ops, "anyPrimThen", sig((move any, &Fn(() -> any)) -> any);
            /// `ToNumber` of an object an `any` holds, for `-`, `*`, `<`, `+x` (`dynamic_arith.rs`).
            any_object_number => lang_hooks_any_ops, "anyObjectNumber", sig((&any, &string) -> f64);
            /// `a + b` of dynamic values one of which holds a string or an object (`dynamic_arith.rs`).
            any_add => lang_hooks_any_ops, "anyAdd", sig((move any, move any, &string) -> any);
            /// `<`, `>`, `<=`, `>=` of dynamic values one of which holds an object (`dynamic_relational.rs`).
            any_relational => lang_hooks_any_ops, "anyRelational", sig((move any, move any, i64, &string) -> bool);
            /// `a == b` of an object and a primitive (`velt_vir` `any_eq.rs`).
            any_loose_eq => lang_hooks_any_ops, "anyLooseEq", sig((&any, &any, &string) -> bool), lowered;

            // class_object.velt: classes as values, and `x.constructor` of an `any`.
            /// The rest element type of a class object's function type, read by `velt_vir` `glue/format.rs`.
            class_marker => lang_hooks_class_object, "ClassMarker", class, lowered;
            /// A class used as a value (`velt_sema` `ctor_objects.rs`; printed by `glue/format.rs`).
            class_object => lang_hooks_class_object, "classObject", sig((&string, &string) -> FnRest(([ClassMarker(string)]) -> any)), lowered;
            /// `class_object` of a class with static fields or accessors.
            class_object_statics => lang_hooks_class_object, "classObjectStatics", sig((&string, &string) -> FnRest(([ClassMarker(bool)]) -> any));
            /// `v.constructor` of an `any` holding no class instance (`velt_sema` `ctor_objects.rs`).
            any_ctor => lang_hooks_class_object, "anyCtor", sig((&any, move any, move any, move any, move any, move any, move any) -> any);
            /// `r.constructor` of an `any` holding a record.
            any_record_ctor => lang_hooks_class_object, "anyRecordCtor", sig((move any, move any) -> any);
            /// `v.constructor` of an `any` without an own `constructor`: sema writes the class dispatch.
            any_constructor => lang_hooks_class_object, "anyConstructor", sig((&any) -> any);
            /// `v.constructor` of an instance of a class declared in a function.
            any_ctor_local => lang_hooks_class_object, "anyCtorLocal", sig((&string) -> any);
            /// `v.constructor` of an instance of a std class JavaScript doesn't name: an error.
            any_ctor_unsupported => lang_hooks_class_object, "anyCtorUnsupported", sig((&string) -> any);
        }
    };
}
