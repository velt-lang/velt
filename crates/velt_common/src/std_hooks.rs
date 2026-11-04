//! The std functions the compiler calls, in one table (docs/internals/design/std-layout.md §10b).
//! CONTRACT FILE — changes need maintainer review (docs/internals/contracts/README.md).
//!
//! Lowering turns some language features into calls of std code written in Velt: `n.toString(16)`
//! calls `f64ToString`, a template over an `any` calls `anyText`, `delete p[k]` of a proxy calls
//! `anyDeleteProxy`. These *hooks* live in `std/lang/hooks/`, as ordinary functions (and a few
//! classes) that no program can name: `lang/` is not importable (`std_modules::importable`), and
//! nothing re-exports them into the prelude or a global. A few live next to what they need (a
//! `Proxy`'s array methods, `this` functions, typed arrays' bytes; outside `lang/`, `Set`'s
//! adapter, not exported). The compiler names a hook only through this table:
//! `std_hook!(f64_to_string)` is its qualified name (`"std/lang/hooks/number::f64ToString"`), the
//! form sema resolves in the code it writes (`Ctx::std_def`: an identifier or type name spelled
//! that way, which no source can write) and lowering finds a definition by. The built-in methods
//! of values held as `any` are found by name prefix instead (`anyStr_<method>`, std/lang/hooks/
//! any_methods.velt; `velt_vir` `any_builtin.rs`).
//!
//! Each entry gives the hook's signature in a small type language (`sig(<A>(&string, i64) ->
//! void)`, or `class`; [`crate::hook_sig`]), which the table turns into a [`Sig`]: `velt_sema`'s
//! test of this table checks every hook module of std whole and walks each entry's definition
//! against it (types, type parameters, `throws` and passing modes), so renaming or
//! changing a hook fails the build, not a program. `velt_vir` generates a typed call builder for
//! each lowered function hook from the same tokens ([`std_hook_table!`]).
//! `lowered` marks the hooks lowering, the optimizer or a backend call: nothing in the checked
//! code refers to them, so lazy std checks them with the prelude (`velt_sema` `lazy_std`), unless
//! the entry lists the constructs whose lowering calls it ([`crate::hook_demand`]).
//!
//! The table is also the port tracker of moving the runtime's work into std (#2196): an entry's
//! `replaces(…)` names the runtime externs (`velt_rt_*`) whose job the hook does, which the
//! compiler must no longer call in the hook's place, and `bench(…)` names the program of
//! `bench/stdport/` that measures the hook against the Rust it replaced. Once nothing calls a
//! replaced extern any more, its Rust is deleted and it moves to [`DELETED`]. `velt_common`'s
//! test `hook_replaces.rs` checks that each named extern is defined by the runtime or listed as
//! deleted (and then named nowhere), that each program exists, and lists every place in the
//! compiler's sources that still names a replaced extern (a fallback, or an emitter with no hook
//! yet: #2197); a new one fails, and so does a listed one that is gone, so the list only shrinks.
//!
//! ```
//! use velt_common::{std_hook, std_hook_name};
//! assert_eq!(std_hook!(f64_to_string), "std/lang/hooks/number::f64ToString");
//! assert_eq!(std_hook_name!(f64_to_string), "f64ToString");
//! ```

pub use crate::hook_demand::{all_demanded, Construct, ConstructSet, HookSet};
pub use crate::hook_sig::{FnSig, Mode, Param, Sig, T};

/// One hook: its entry name, its module's canonical path, its name there, its signature, whether
/// lowering calls it, whether lowering may find it missing and fall back to the runtime
/// (`fallback`), the runtime externs it replaces and the benchmark that measures it.
#[derive(Clone, Copy, Debug)]
pub struct Hook {
    /// The entry's name (`f64_to_string`).
    pub name: &'static str,
    /// The std module table's entry of the module that defines it (`lang_hooks_number`:
    /// [`crate::std_modules`]).
    pub module_entry: &'static str,
    /// Its name in that module (`f64ToString`; `ArrayView` for a class).
    pub item: &'static str,
    /// Its signature ([`crate::hook_sig`]).
    pub sig: Sig,
    /// Lowering, the optimizer or a backend calls it by name.
    pub lowered: bool,
    /// The runtime externs (`velt_rt_*`) whose job this hook does: the compiler must call the
    /// hook, not the extern, for the feature the hook implements (`velt_common`'s test
    /// `hook_replaces.rs` lists every place left that still names one, #2197). An extern the
    /// hook only wraps is not listed: `processWrite` writes through `velt_rt_write_str`, which
    /// stays the runtime's job. A hook may also call an extern it replaces for some of its
    /// inputs (`numberText` hands a float that is not whole to `velt_rt_str_from_f64`): what
    /// counts is that the compiler no longer calls it in the hook's place.
    pub replaces: &'static [&'static str],
    /// Lowering calls the runtime instead when the hook is missing or left unchecked (it is
    /// one of [`ON_DEMAND_FLOAT`]): its `velt_vir` builder returns an `Option`. Every other
    /// lowered hook missing is an internal compiler error.
    pub fallback: bool,
    /// The program of `bench/stdport/` (`int_to_string` for `bench/stdport/int_to_string.velt`)
    /// that measures this hook against the Rust it replaced, if one does.
    pub bench: Option<&'static str>,
    /// The HIR constructs whose lowering calls it ([`crate::hook_demand`]): sema checks it only
    /// where live code has one, and lowering asserts that. None: always needed.
    pub constructs: &'static [Construct],
}

impl Hook {
    /// The canonical path of the module that defines it: `std/lang/hooks/number`.
    pub fn module(&self) -> String {
        let rel = crate::std_modules::rel(self.module_entry).unwrap_or_else(|| {
            panic!(
                "ICE: hook module `{}` is not a std module",
                self.module_entry
            )
        });
        format!("{}{rel}", crate::std_modules::CANONICAL_PREFIX)
    }

    /// The qualified name: `std/lang/hooks/number::f64ToString`.
    pub fn qualified(&self) -> String {
        format!("{}::{}", self.module(), self.item)
    }
}

/// An entry is `name => module, "item", sig(…)` (or `class`), then optional flags after commas,
/// in any order: `lowered`, `fallback`, `replaces("velt_rt_…", …)`, `bench("row")` and
/// `constructs(TextInt, …)` (the fields of [`Hook`]).
macro_rules! hooks {
    // Each flag is one of these (an unknown one fails here).
    (@flag lowered) => {};
    (@flag fallback) => {};
    (@flag replaces) => {};
    (@flag bench) => {};
    (@flag constructs) => {};
    (@lowered) => { false };
    (@lowered lowered $($rest:tt)*) => { true };
    (@lowered $skip:tt $($rest:tt)*) => { hooks!(@lowered $($rest)*) };
    (@fallback) => { false };
    (@fallback fallback $($rest:tt)*) => { true };
    (@fallback $skip:tt $($rest:tt)*) => { hooks!(@fallback $($rest)*) };
    (@replaces) => { &[] };
    (@replaces replaces ($($r:literal),* $(,)?) $($rest:tt)*) => { &[$($r),*] };
    (@replaces $skip:tt $($rest:tt)*) => { hooks!(@replaces $($rest)*) };
    (@bench) => { None };
    (@bench bench ($b:literal) $($rest:tt)*) => { Some($b) };
    (@bench $skip:tt $($rest:tt)*) => { hooks!(@bench $($rest)*) };
    (@constructs) => { &[] };
    (@constructs constructs ($($c:ident),* $(,)?) $($rest:tt)*) => { &[$(Construct::$c),*] };
    (@constructs $skip:tt $($rest:tt)*) => { hooks!(@constructs $($rest)*) };
    ($($(#[doc = $doc:literal])* $name:ident => $module:ident, $item:literal,
        $kind:ident $(($($sig:tt)*))? $(, $flag:ident $(($($arg:tt)*))?)*;)*) => {
        $($(hooks!(@flag $flag);)*)*

        /// Calls macro `cb` (in scope where this is invoked) with every entry of the table, as
        /// `name => "item", sig(…) [flags];` (or `class`), the flags' names only: how `velt_vir`
        /// generates its typed call builders from the signatures written here.
        #[macro_export]
        macro_rules! std_hook_table {
            ($cb:ident) => {
                $cb! { $( $name => $item, $kind $(($($sig)*))? [$($flag)*]; )* }
            };
        }

        /// The qualified name of hook `name`: `std_hook!(f64_to_string)` is
        /// `"std/lang/hooks/number::f64ToString"`. A literal, usable in `const`s and patterns.
        #[macro_export]
        macro_rules! std_hook {
            $( ($name) => { $crate::std_item!($module, $item) }; )*
        }

        /// The name of hook `name` in its module: `std_hook_name!(f64_to_string)` is
        /// `"f64ToString"`.
        #[macro_export]
        macro_rules! std_hook_name {
            $( ($name) => { $item }; )*
        }

        /// Every hook, in table order.
        pub const HOOKS: &[Hook] = &[$(Hook {
            name: stringify!($name),
            module_entry: stringify!($module),
            item: $item,
            sig: $crate::hook_sig!($kind $(($($sig)*))?),
            lowered: hooks!(@lowered $($flag $(($($arg)*))?)*),
            fallback: hooks!(@fallback $($flag $(($($arg)*))?)*),
            replaces: hooks!(@replaces $($flag $(($($arg)*))?)*),
            bench: hooks!(@bench $($flag $(($($arg)*))?)*),
            constructs: hooks!(@constructs $($flag $(($($arg)*))?)*),
        }),*];

        #[allow(non_camel_case_types, dead_code)]
        enum Index { $($name),* }

        /// Each hook's index in [`HOOKS`] (a [`HookSet`]'s bit): `hook_index::int_push`.
        #[allow(non_upper_case_globals)]
        pub mod hook_index {
            $(pub const $name: usize = super::Index::$name as usize;)*
        }
    };
}

hooks! {
    // array.velt: array constructors.
    /// `new Array<T>(n).fill(v)` (`velt_sema` `array_ctor.rs`).
    array_filled => lang_hooks_array, "arrayFilled", sig(<A>(f64, &A) -> [A]);
    /// `new Array<T>(x).fill(v)` with an `x` that is no number.
    array_one => lang_hooks_array, "arrayOne", sig(<A, B>(&A, move B) -> B);
    /// `Array.from({ length: n }, f)`.
    array_from_length => lang_hooks_array, "arrayFromLength", sig(<A, B>(i64, &Fn((i64) -> A throws B)) -> [A] throws B);

    // number.velt: numbers as text, and the conversions of a float index.
    /// `n.toString(radix)` of a float (`velt_sema` `builtins.rs`).
    f64_to_string => lang_hooks_number, "f64ToString", sig((f64, f64) -> string);
    /// `n.toString(radix)` of a signed integer.
    i64_to_string => lang_hooks_number, "i64ToString", sig((i64, f64) -> string);
    /// `n.toString(radix)` of an unsigned integer.
    u64_to_string => lang_hooks_number, "u64ToString", sig((u64, f64) -> string);
    /// `Math.imul`-like `(a * b) | 0` of two int32 values (`velt_sema` `int32.rs`, `velt_opt`
    /// numrep).
    mul_to_int32 => lang_hooks_number, "mulToInt32", sig((f64, f64) -> i32), lowered;
    /// An array index written as a float (`xs[i]` with `i: number`): ts-compat.md.
    float_index => lang_hooks_number, "floatIndex", sig((f64) -> usize), lowered;
    /// A float position argument (`xs.at(i)`, `s.slice(i)`).
    float_position => lang_hooks_number, "floatPosition", sig((f64) -> usize);
    /// `String(v)` of a signed integer: `${v}` alone, `v.toString()` (`velt_vir` `template.rs`).
    /// `IndexCheck`: the message of an out-of-bounds signed index. Sema casts every index it
    /// builds to an unsigned type, so no program reaches that call; only `velt_vir`'s
    /// `hook_demand` test does, with an index made signed by hand. Lowering still handles one.
    int_text => lang_hooks_number_text, "intText", sig((i64) -> string), lowered,
        replaces("velt_rt_str_from_i64"), bench("int_to_string"),
        constructs(TextInt, IndexCheck);
    /// `String(v)` of an unsigned integer.
    uint_text => lang_hooks_number_text, "uintText", sig((u64) -> string), lowered,
        replaces("velt_rt_str_from_u64"), bench("int_to_string"),
        constructs(TextUint, IndexCheck, Dynamic);
    /// A signed integer appended to a builder: a template's `${v}` (`velt_vir` `strbuf.rs`).
    int_push => lang_hooks_number_text, "intPush", sig((&mut string, i64) -> void), lowered,
        replaces("velt_rt_strbuf_push_i64"), bench("int_to_string"),
        constructs(TextInt, Format, Dynamic);
    /// An unsigned integer appended to a builder.
    uint_push => lang_hooks_number_text, "uintPush", sig((&mut string, u64) -> void), lowered,
        replaces("velt_rt_strbuf_push_u64"), bench("int_to_string"),
        constructs(TextUint, Format, Dynamic);
    /// `String(x)` of a float (`velt_vir` `template.rs`): a whole number by std's integer
    /// printing, anything else by the runtime's printer. `velt_opt` numrep turns a call with a
    /// whole `x` into one of `int_text`.
    number_text => lang_hooks_number_text, "numberText", sig((f64) -> string), lowered, fallback,
        replaces("velt_rt_str_from_f64"), bench("float_to_string");
    /// A float appended to a builder, as `number_text`; numrep turns a call with a whole `x` into
    /// `int_push`.
    number_push => lang_hooks_number_text, "numberPush", sig((&mut string, f64) -> void), lowered, fallback,
        replaces("velt_rt_strbuf_push_f64"), bench("float_to_string");
    /// `String(x)` of a float, all of it by std's printing (velt/lang/number/format): lowering
    /// calls it instead of `number_text` where its module is loaded ([`ON_DEMAND_MODULES`], for a
    /// program that converts an operand that may be a float).
    float_text => lang_hooks_number_float, "floatText", sig((f64) -> string), lowered, fallback,
        replaces("velt_rt_str_from_f64"), bench("float_to_string");
    /// A float appended to a builder, as `float_text`.
    float_push => lang_hooks_number_float, "floatPush", sig((&mut string, f64) -> void), lowered, fallback,
        replaces("velt_rt_strbuf_push_f64"), bench("float_to_string");

    // process.velt: the builtin `process` namespace (`velt_sema` `process.rs`).
    /// `process.stdout.write(s)` / `process.stderr.write(s)`.
    process_write => lang_hooks_process, "processWrite", sig((u32, &string) -> bool);
    /// `process.env.NAME` / `process.env[name]`.
    process_env => lang_hooks_process, "processEnv", sig((&string) -> or_undefined(string));
    /// `process.argv`.
    process_argv => lang_hooks_process, "processArgv", sig(() -> [string]);
    /// The mark of the top-level `await`s after it (`velt_sema` `top_level_await.rs`); lowering
    /// replaces its calls by the runtime's (`async_fn/suspend.rs`).
    top_level_await_at => lang_hooks_process, "topLevelAwaitAt", sig((&string) -> void), lowered;

    // syntax.velt: what the parser writes some syntax as (velt_syntax `ast_names.rs`).
    /// `value satisfies T`.
    satisfies => lang_hooks_syntax, "satisfies", sig(<A>(move A) -> A);
    /// A getter in an object literal.
    object_get => lang_hooks_syntax, "objectGet", sig(<A>(&Fn(() -> A)) -> A);
    /// A setter in an object literal.
    object_set => lang_hooks_syntax, "objectSet", sig(<A>(move Fn((A) -> void)) -> Fn((A) -> void));
    /// `{ [key]: value }` with a key known only at run time.
    computed_key => lang_hooks_syntax, "computedKey", sig(<A>(move string, move A) -> Record(string, A));

    // adapt.velt: interface adapters of `unknown[]` and `Map<unknown, unknown>` (`velt_sema`
    // `iface_adapt.rs`).
    /// The `unknown[]` copy of an array an adapter passes for an `unknown[]` parameter.
    unknown_array_in => lang_hooks_adapt, "unknownArrayIn", sig((&unknown) -> [unknown]);
    /// The elements of that copy written back into the array after the call.
    unknown_array_out => lang_hooks_adapt, "unknownArrayOut", sig((&unknown, &[unknown]) -> void);
    /// The `Map<unknown, unknown>` copy of a map an adapter passes for such a parameter.
    unknown_map_in => lang_hooks_adapt, "unknownMapIn", sig((&unknown) -> Map(unknown, unknown));
    /// The entries of that copy written back into the map after the call.
    unknown_map_out => lang_hooks_adapt, "unknownMapOut", sig((&unknown, move Map(unknown, unknown)) -> void);

    // any_text.velt: the text of an `any` (`velt_sema` `any_text.rs`); lowering keeps one instance of
    // each and passes the call site (`velt_vir` `track_caller.rs`).
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
    /// `any_join` with the arrays being joined around it, so a cycle prints as empty text.
    any_join_seen => lang_hooks_any_text, "anyJoinSeen", sig((move any, &string, &mut [any], &string) -> string), lowered;

    // any_object.velt: `Object.*`, spreads, prototypes and `delete` of an `any`.
    /// `{ ...o }` of an `any` in an object literal (`velt_sema` `record_literal.rs`).
    spread_any => lang_hooks_any_object, "spreadAny", sig((&mut Record(string, any), &any) -> void);
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

    // any_record.velt: an `any` holding another object converted to a record (`velt_vir`
    // `any_to_record.rs`), loaded for a program that mentions `Record`
    // (std/prelude/global/record_any.velt).
    /// `x as Record<string, any>` of an `any` holding an object that is no such record: a copy.
    any_to_record_any => lang_hooks_any_record, "anyToRecordAny", sig((&any) -> Record(string, any)), lowered;
    /// `x as Record<string, V>` of such an `any`: `any_to_record_any`, each value a `V`.
    any_to_record => lang_hooks_any_record, "anyToRecord", sig(<A>(&any) -> Record(string, A)), lowered;

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

    // tuple_elements.velt: a tuple's array methods that call back (`velt_sema` `tuple_text.rs`).
    /// The elements of one tuple, read when a method reaches them.
    tuple_elements_class => lang_hooks_tuple_elements, "TupleElements", class;
    /// `TupleElements` of a tuple: an array of its elements for their type, and how to read one.
    tuple_elements => lang_hooks_tuple_elements, "tupleElements", sig(<A>(&[A], move Fn((i64) -> A)) -> TupleElements(A));

    // array_view.velt: an `any` holding an array viewed as `T[]` (`velt_vir` `array_view.rs`).
    /// One view's state: the original array and the mirror of its elements as `T`.
    array_view => lang_hooks_array_view, "ArrayView", class, lowered;
    /// A view of the array an `any` holds, as a `T[]`.
    array_view_new => lang_hooks_array_view, "arrayViewNew", sig(<A>(move any) -> ArrayView(A)), lowered;
    /// The mirror brought up to date with the original before a read of the whole view.
    array_view_sync => lang_hooks_array_view, "arrayViewSync", sig(<A>(&mut ArrayView(A)) -> void), lowered;
    /// The mirror written back to the original after a change through the view.
    array_view_flush => lang_hooks_array_view, "arrayViewFlush", sig(<A>(&ArrayView(A)) -> void), lowered;
    /// `array_view_flush` of the mirror's elements from an index on (after a `push`).
    array_view_flush_from => lang_hooks_array_view, "arrayViewFlushFrom", sig(<A>(&ArrayView(A), usize) -> void), lowered;
    /// `length` of a view: the original's, nothing converted.
    array_view_len => lang_hooks_array_view, "arrayViewLen", sig(<A>(&ArrayView(A)) -> usize), lowered;
    /// Before a read of element `i` of a view: the mirror up to date up to `i`.
    array_view_sync_at => lang_hooks_array_view, "arrayViewSyncAt", sig(<A>(&mut ArrayView(A), usize) -> void), lowered;
    /// Before a change of a view's length (`push`, `pop`): the mirror of the original's length.
    array_view_sync_len => lang_hooks_array_view, "arrayViewSyncLen", sig(<A>(&mut ArrayView(A), bool) -> void), lowered;
    /// After a write of element `i` of a view: that element written back to the original.
    array_view_flush_at => lang_hooks_array_view, "arrayViewFlushAt", sig(<A>(&ArrayView(A), usize) -> void), lowered;
    /// `sort(cmp)` of a view: the mirror sorted by the stable sort and written back.
    array_view_sort => lang_hooks_array_view, "arrayViewSort", sig(<A, B>(&mut ArrayView(A), &Fn((A, A) -> B), &Fn((B) -> bool)) -> void), lowered;


    // record.velt: `k in r` for a key known only at run time, and `===` on prototypes.
    /// `a === b` on prototypes (`velt_sema` `object_prototypes.rs`).
    prototype_same => lang_hooks_record, "prototypeSame", sig((&or_null(ObjectPrototype), &or_null(ObjectPrototype)) -> bool);
    /// `key in p` of a prototype for a key known only at run time (`velt_sema` `in_keys.rs`).
    prototype_in => lang_hooks_record, "prototypeIn", sig((&string, &ObjectPrototype) -> bool);
    /// `k in r` of a record for a key known only at run time (`velt_sema` `in_keys.rs`).
    record_in => lang_hooks_record, "recordIn", sig(<A>(&string, &Record(string, A)) -> bool);
    /// `record_in` in a program that calls `Object.create(null)`.
    record_in_maybe_null_proto => lang_hooks_record, "recordInMaybeNullProto", sig(<A>(&string, &Record(string, A)) -> bool);
    // error.velt: the first line of the report of an uncaught error (`velt_vir` `entry.rs`).
    /// The first line of Node's report of an uncaught error (`E [TypeError]: m`).
    uncaught_header => lang_hooks_error, "uncaughtHeader", sig((&Error, &string) -> string), lowered;

    // string.velt: the characters a `for...of` over a string iterates.
    /// The code points `for (const c of s)` iterates (`velt_sema` `loops.rs`).
    string_code_points => lang_hooks_string, "stringCodePoints", sig((&string) -> [string]);

    // iter.velt, async_iter.velt: iterable object literals.
    /// `{ async *[Symbol.asyncIterator]() { … } }` (`velt_sema` `object_method.rs`).
    async_iterable_object => lang_hooks_async_iter, "AsyncIterableObject", class;
    /// `{ *[Symbol.iterator]() { … } }` (`velt_sema` `object_method.rs`).
    iterable_object => lang_hooks_iter, "IterableObject", class;

    // json.velt: `JSON.parse(text) as T`, loaded with the global `JSON`
    // (std/prelude/global/json.velt).
    /// `JSON.parse(text)` with its type from the context (`velt_sema` `static_call.rs`).
    json_parse_from_context => lang_hooks_json, "jsonParseFromContext", sig(<A>(&string, &or_null(object("{ unknownKeys?: \"ignore\" | \"reject\", maxDepth?: i64, comments?: \"error\" | \"skip\", trailingCommas?: boolean }"))) -> A throws JsonError);

    // proxy.velt: the operations on `any` that ask a proxy, used once a program has loaded `Proxy`
    // (std/prelude/global/proxy.velt).
    /// `delete o.k` / `delete o[k]` of an `any`: a proxy's `deleteProperty` trap (`proxy.rs`).
    any_delete_proxy => lang_hooks_proxy, "anyDeleteProxy", sig((&any, move any) -> bool);
    /// `delete o[KEY]` of an `any` with a symbol constant `KEY` (`velt_sema` `proxy.rs`).
    any_delete_sym => lang_hooks_proxy, "anyDeleteSym", sig((&any, move string, symbol) -> bool);
    /// `Object.entries(o)` of an `any` that may be a proxy.
    any_entries_proxy => lang_hooks_proxy, "anyEntriesProxy", sig((move any) -> [tuple(string, any)]);
    /// Whether `for (const k in o)` of an `any` visits `k` at its turn (`for_in_proxy.rs`).
    any_for_in_has => lang_hooks_proxy, "anyForInHas", sig((&any, move string) -> bool);
    /// The keys `for (const k in o)` of an `any` visits (`velt_sema` `for_in_proxy.rs`).
    any_for_in_keys => lang_hooks_proxy, "anyForInKeys", sig((&any) -> [string]);
    /// `o[k]` of an `any` with a key that may be a symbol (`dynamic_member.rs`, `dynamic_write.rs`).
    any_get_key => lang_hooks_proxy, "anyGetKey", sig((move any, &any) -> any);
    /// `o[KEY]` of an `any` with a symbol constant `KEY` (`dynamic_member.rs`, `dynamic_write.rs`).
    any_get_sym => lang_hooks_proxy, "anyGetSym", sig((move any, &string, symbol) -> any);
    /// `k in o` of an `any` with a key that may be a symbol (`velt_sema` `proxy.rs`).
    any_has_key => lang_hooks_proxy, "anyHasKey", sig((&any, &any) -> bool);
    /// `Object.hasOwn(o, k)` of an `any` that may be a proxy.
    any_has_own_proxy => lang_hooks_proxy, "anyHasOwnProxy", sig((move any, move union(string, symbol)) -> bool);
    /// `Object.hasOwn(o, KEY)` of an `any` with a symbol constant `KEY` (`object_own.rs`).
    any_has_own_sym => lang_hooks_proxy, "anyHasOwnSym", sig((&any, &string, symbol) -> bool);
    /// `KEY in o` of an `any` with a symbol constant `KEY` (`velt_sema` `proxy.rs`).
    any_has_sym => lang_hooks_proxy, "anyHasSym", sig((&any, &string, symbol) -> bool);
    /// `Array.isArray(o)` of an `any` where proxies exist (`object_builtins.rs`, `dynamic*.rs`).
    any_is_array_proxy => lang_hooks_proxy, "anyIsArrayProxy", sig((&any) -> bool);
    /// `o[k] = v` of an `any` with a key that may be a symbol (`velt_sema` `dynamic_write.rs`).
    any_set_key => lang_hooks_proxy, "anySetKey", sig((move any, &any, move any) -> void);
    /// `o[KEY] = v` of an `any` with a symbol constant `KEY` (`velt_sema` `dynamic_write.rs`).
    any_set_sym => lang_hooks_proxy, "anySetSym", sig((move any, &string, symbol, move any) -> void);
    /// `Object.values(o)` of an `any` that may be a proxy.
    any_values_proxy => lang_hooks_proxy, "anyValuesProxy", sig((move any) -> [any]);
    /// `o.key` of a class getter on an instance brand holding a typed proxy (`proxy_getters.rs`).
    brand_getter_read => lang_hooks_proxy, "brandGetterRead", sig((move any, move string) -> any);
    /// `has_own_property` where proxies exist (`velt_sema` `proxy.rs`, `velt_vir` `any_proxy.rs`).
    has_own_property_proxy => lang_hooks_proxy, "HasOwnPropertyProxy", class, lowered;
    /// A typed proxy as the `any` it is (`velt_sema` `for_in_proxy.rs`).
    proxy_any => lang_hooks_proxy, "proxyAny", sig((move any) -> any);
    /// `"k" in x` of a typed proxy: its `has` trap (`velt_sema` `proxy.rs`).
    proxy_has => lang_hooks_proxy, "proxyHas", sig((&any, move string) -> bool);
    /// `Object.hasOwn(x, k)` of a typed proxy: its `getOwnPropertyDescriptor` trap.
    proxy_has_own_of => lang_hooks_proxy, "proxyHasOwnOf", sig((move any, move string) -> bool);
    /// `Object.keys(x)` of a typed proxy: the keys its traps list (`velt_sema` `proxy.rs`).
    proxy_keys_of => lang_hooks_proxy, "proxyKeysOf", sig((&any) -> [string]);
    /// `new Proxy(target, handler)` with a handler typed `any` (`velt_sema` `proxy.rs`).
    proxy_new_any => lang_hooks_proxy, "proxyNewAny", sig((move any, move any) -> any);
    /// `{ ...x }` of an object in a program that makes proxies: a typed proxy is not supported.
    proxy_spread_check => lang_hooks_proxy, "proxySpreadCheck", sig((&any) -> void);
    /// `{ ...o }` of an `any` that may be a proxy: its enumerable own entries.
    spread_any_proxy => lang_hooks_proxy, "spreadAnyProxy", sig((&mut Record(string, any), move any) -> void);

    // Outside lang/hooks, defined where what they need is (none is exported to programs):
    // lang/proxy/array.velt: an array method read through a proxy.
    /// An array method read through a proxy or `Array.prototype` (`velt_vir` `any_proxy.rs`).
    array_method => lang_proxy_array, "ArrayMethod", class, lowered;
    /// `Array.prototype[key]` read through `any` (`velt_vir` `any_get_body`).
    array_prototype_member => lang_proxy_array_apply, "arrayPrototypeMember", sig((move string) -> any), lowered;

    // lang/this_fn: a function whose `this` is bound at the call (a global loaded on demand).
    /// `o.m(...args)` of a member read other than as a slot (`object_call_on.rs`, `this_fn_call.rs`).
    call_on => lang_this_fn, "callOn", sig((&any, move any, move [any]) -> any);
    /// `o.m?.(...args)` (`velt_sema` `this_fn_call.rs`).
    call_opt_on => lang_this_fn, "callOptOn", sig((&any, move any, &Fn(() -> [any])) -> any);
    /// A function value whose `this` is bound at the call (`this_fn_expr.rs`, `velt_vir` `this_fn.rs`).
    this_fn => lang_this_fn, "ThisFn", class, lowered;

    // collections/set.velt: the adapter of `Set<unknown>` (not exported).
    /// The `Set<unknown>` copy of a set an interface adapter passes (`velt_sema` `iface_adapt.rs`).
    unknown_set_in => collections_set, "unknownSetIn", sig((&unknown) -> Set(unknown));
    /// The elements of that copy written back into the set after the call.
    unknown_set_out => collections_set, "unknownSetOut", sig((&unknown, &Set(unknown)) -> void);

    // text/regex/string_methods.velt: the string methods called with a `RegExp` (`velt_sema`
    // `regex_args.rs`), loaded with `RegExp`.
    /// `s.replace(re, replacement)`.
    replace_reg_exp => text_regex_string_methods, "replaceRegExp", sig((&string, &RegExp, &string) -> string);
    /// `s.replace(re, (match, …) => …)`, the function adapted by sema.
    replace_reg_exp_calling => text_regex_string_methods, "replaceRegExpCalling", sig(<A>(&mut string, &mut RegExp, &Fn((RegExpMatch, string) -> string throws A)) -> string throws A);
    /// `s.replaceAll(re, replacement)`.
    replace_all_reg_exp => text_regex_string_methods, "replaceAllRegExp", sig((&string, &RegExp, &string) -> string);
    /// `s.replaceAll(re, (match, …) => …)`.
    replace_all_reg_exp_calling => text_regex_string_methods, "replaceAllRegExpCalling", sig(<A>(&mut string, &mut RegExp, &Fn((RegExpMatch, string) -> string throws A)) -> string throws A);
    /// `s.match(re)`.
    match_reg_exp => text_regex_string_methods, "matchRegExp", sig((&string, &RegExp) -> or_null([string]));
    /// `s.matchAll(re)`.
    match_all_reg_exp => text_regex_string_methods, "matchAllRegExp", sig((&string, &RegExp) -> [RegExpMatch]);
    /// `s.search(re)`.
    search_reg_exp => text_regex_string_methods, "searchRegExp", sig((&string, &RegExp) -> i64);
    /// `s.split(re, limit)`.
    split_reg_exp => text_regex_string_methods, "splitRegExp", sig((&string, &RegExp, or_undefined(f64)) -> [string]);

    // test/registry.velt, test/run.velt: what `velt test`'s generated `main` calls (veltc
    // `commands/test/harness.rs`; the loader lets only the harness import these files).
    /// Starts the registrations of the next test file.
    register_file => test_registry, "registerFile", sig((i64) -> void);
    /// Registers a test function of the current file.
    register_test => test_registry, "registerTest", sig((move string, move Fn(() -> void throws unknown)) -> void);
    /// Registers an `async` test function of the current file.
    register_async_test => test_registry, "registerAsyncTest", sig(<A>(move string, move Fn(() -> Promise(A, unknown))) -> void);
    /// Runs the registered tests: the entry of a test program.
    run_registered => test_run, "runRegistered", sig(() -> void throws union(IoError, JsonError));

    // The record a JSON decoder generated by velt/encoding/json makes of a map
    // (std/encoding/json/generated.velt imports it for the code it generates).
    /// A record of a map's entries in its order (code generated by velt/encoding/json).
    record_from_map => lang_hooks_record, "recordFromMap", sig(<A, B>(move Map(A, B)) -> Record(A, B));
    // The map of a record's entries the comparisons velt/lang/equal generates compare
    // (std/lang/equal/generated.velt imports it for the code it generates).
    /// The map of a record's string-keyed entries (code generated by velt/lang/equal).
    record_entries_of => lang_hooks_record, "recordEntriesOf", sig(<A, B>(&Record(A, B)) -> Map(A, B));

    // lang/typed_array/buffer_view.velt: the bytes of a typed array.
    /// The bytes a typed array or `DataView` passed to a system function views (`system_views.rs`).
    view_bytes => lang_typed_array_buffer_view, "viewBytes", sig((&BufferView) -> usize), lowered;

    // ffi/ptr.velt: a `Ptr` from and to a C function (velt_vir's lower/c_call.rs).
    ffi_ptr_of => ffi_ptr, "ptrOf", sig(<A>(usize) -> Ptr(A)), lowered;
    ffi_ptr_or_null => ffi_ptr, "ptrOrNull", sig(<A>(usize) -> or_null(Ptr(A))), lowered;
    ffi_ptr_address => ffi_ptr, "ptrAddress", sig(<A>(&Ptr(A)) -> usize), lowered;
    ffi_ptr_address_or_zero => ffi_ptr, "ptrAddressOrZero", sig(<A>(&or_null(Ptr(A))) -> usize), lowered;
    // ffi/str_bytes.velt: the string whose bytes a `StrBytes` argument passes.
    ffi_str_bytes_of => ffi_str_bytes, "strBytesOf", sig((move string) -> string), lowered;
    // ffi/c_string.velt: the bytes a `CString` argument passes.
    ffi_c_string_bytes => ffi_c_string, "cStringBytes", sig((&CString) -> [u8]), lowered;

    // host/host_ref.velt: a `HostRef` from and to a host function (velt_vir's lower/host.rs).
    /// The `HostRef` of the slot a host function returned.
    host_ref_of => host_host_ref, "hostRefOf", sig((u32) -> HostRef), lowered;
    /// The `HostRef` of a returned slot, or `null` for 0.
    host_ref_or_null => host_host_ref, "hostRefOrNull", sig((u32) -> or_null(HostRef)), lowered;
    /// The slot of a `HostRef` (0 for `null`) passed to a host function.
    host_ref_handle => host_host_ref, "hostRefHandle", sig((&or_null(HostRef)) -> u32), lowered;
    // host/promise.velt: the promise of an async host function's call (velt_vir's lower/host.rs).
    /// How a host promise settled, as the JS glue writes it.
    host_settled => host_promise, "HostSettled", class, lowered;
    /// The value a host promise fulfills with; an `IoError` when it rejects.
    host_await => host_promise, "hostAwait", sig(<A>(move Promise(HostSettled(A), never)) -> A throws IoError), lowered;
    /// `host_await` for a host function without a result.
    host_await_void => host_promise, "hostAwaitVoid", sig((move Promise(HostSettled(bool), never)) -> void throws IoError), lowered;
    /// `host_await` for a `HostRef` result.
    host_await_ref => host_promise, "hostAwaitRef", sig((move Promise(HostSettled(u32), never)) -> HostRef throws IoError), lowered;
    /// `host_await` for a `HostRef | null` result.
    host_await_ref_or_null => host_promise, "hostAwaitRefOrNull", sig((move Promise(HostSettled(u32), never)) -> or_null(HostRef) throws IoError), lowered;
}

/// One key of a std method the compiler calls: a symbol constant of std/lang/hooks/keys.velt.
#[derive(Clone, Copy, Debug)]
pub struct Key {
    /// The entry's name (`map_cursor`).
    pub name: &'static str,
    /// The constant in std/lang/hooks/keys.velt (`MAP_CURSOR`).
    pub constant: &'static str,
    /// The symbol's description (`mapCursor`), or its registry key for a registered one.
    pub description: &'static str,
    /// `Symbol.for(key)`, which programs reach too (a protocol they implement: `inspect`), not
    /// `Symbol(description)`, which only std holds.
    pub registered: bool,
}

macro_rules! keys {
    (@registered) => { false };
    (@registered registered) => { true };
    ($($(#[doc = $doc:literal])* $name:ident => $constant:literal, $desc:literal $(, $reg:ident)?;)*) => {
        /// The member name of the std method keyed by entry `name` of the key table: what sema
        /// names a member `[KEY]` declared with that symbol (`"\u{FFFF}Symbol(mapCursor)"`;
        /// velt_sema's `property_order::symbol_key_name`, which names std's symbols first).
        #[macro_export]
        macro_rules! std_key {
            $( ($name) => { concat!("\u{FFFF}Symbol(", $desc, ")") }; )*
        }

        /// Every key, in table order.
        pub const KEYS: &[Key] = &[$(Key {
            name: stringify!($name),
            constant: $constant,
            description: $desc,
            registered: keys!(@registered $($reg)?),
        }),*];
    };
}

keys! {
    // What `console.log` and `util.inspect` call, as Node's `util.inspect.custom`: a program's
    // class implements it with `const custom = Symbol.for("nodejs.util.inspect.custom")` and a
    // method `[custom]()` (`velt_sema` `hooks.rs`).
    inspect => "INSPECT", "nodejs.util.inspect.custom", registered;
    // Typed arrays: the kind a class's elements are (a subclass may override it).
    typed_array_kind => "TYPED_ARRAY_KIND", "velt.typedArray.kind", registered;
    // Arrays: `xs[i]` that reads `undefined` past the end, as JS (`velt_sema` `indexers.rs`).
    array_get => "ARRAY_GET", "arrayGet";
    // `xs.reduce(f)` without an initial value (`velt_sema` `method_call.rs`).
    array_reduce_first => "ARRAY_REDUCE_FIRST", "arrayReduceFirst";
    array_reduce_right_first => "ARRAY_REDUCE_RIGHT_FIRST", "arrayReduceRightFirst";
    // JsonValue: what `console.log` prints a value from (`velt_vir` `glue/format.rs`), what
    // appends one's text (velt/encoding/json/append) and what decoders read one with
    // (velt/encoding/json/generate/read_value).
    json_runtime_handle => "JSON_RUNTIME_HANDLE", "jsonRuntimeHandle";
    json_write => "JSON_WRITE", "jsonWrite";
    json_from_reader => "JSON_FROM_READER", "jsonFromReader";
    // Map: what a `for...of` over a map reads (`velt_sema` `for_map.rs`, `velt_vir` `frozen.rs`),
    // what `console.log` calls (`velt_vir` `glue/format_map.rs`).
    map_cursor => "MAP_CURSOR", "mapCursor";
    map_advance => "MAP_ADVANCE", "mapAdvance";
    map_key_at => "MAP_KEY_AT", "mapKeyAt";
    map_value_at => "MAP_VALUE_AT", "mapValueAt";
    map_entry_at => "MAP_ENTRY_AT", "mapEntryAt";
    map_live_at => "MAP_LIVE_AT", "mapLiveAt";
    // ThisFn: a call with a `this` and `bind` (`velt_vir` `this_fn.rs`).
    this_fn_apply => "THIS_FN_APPLY", "thisFnApply";
    this_fn_bind => "THIS_FN_BIND", "thisFnBind";
    // Record: what the record syntax and `Object.*` compile to (`velt_sema` `record*.rs`,
    // `velt_vir` `any_record.rs`, `json/map.rs`).
    record_at => "RECORD_AT", "recordAt";
    record_decoded => "RECORD_DECODED", "recordDecoded";
    record_delete => "RECORD_DELETE", "recordDelete";
    record_entries => "RECORD_ENTRIES", "recordEntries";
    record_extend => "RECORD_EXTEND", "recordExtend";
    record_extend_from => "RECORD_EXTEND_FROM", "recordExtendFrom";
    record_extend_maybe => "RECORD_EXTEND_MAYBE", "recordExtendMaybe";
    record_get => "RECORD_GET", "recordGet";
    record_has => "RECORD_HAS", "recordHas";
    record_in_any => "RECORD_IN_ANY", "recordInAny";
    record_is_null_proto => "RECORD_IS_NULL_PROTO", "recordIsNullProto";
    record_key_names => "RECORD_KEY_NAMES", "recordKeyNames";
    record_mark_null_proto => "RECORD_MARK_NULL_PROTO", "recordMarkNullProto";
    record_perm => "RECORD_PERM", "recordPerm";
    record_positions => "RECORD_POSITIONS", "recordPositions";
    record_set => "RECORD_SET", "recordSet";
    record_size => "RECORD_SIZE", "recordSize";
    record_values => "RECORD_VALUES", "recordValues";
    record_accessor => "RECORD_ACCESSOR", "recordAccessor";
    record_define_accessor => "RECORD_DEFINE_ACCESSOR", "recordDefineAccessor";
    record_define_getter => "RECORD_DEFINE_GETTER", "recordDefineGetter";
    record_define_setter => "RECORD_DEFINE_SETTER", "recordDefineSetter";
    record_delete_keyed => "RECORD_DELETE_KEYED", "recordDeleteKeyed";
    record_delete_sym => "RECORD_DELETE_SYM", "recordDeleteSym";
    record_get_keyed => "RECORD_GET_KEYED", "recordGetKeyed";
    record_get_sym => "RECORD_GET_SYM", "recordGetSym";
    record_has_keyed => "RECORD_HAS_KEYED", "recordHasKeyed";
    record_has_sym => "RECORD_HAS_SYM", "recordHasSym";
    record_in_keyed => "RECORD_IN_KEYED", "recordInKeyed";
    record_own_keys => "RECORD_OWN_KEYS", "recordOwnKeys";
    record_own_symbols => "RECORD_OWN_SYMBOLS", "recordOwnSymbols";
    record_put_accessor => "RECORD_PUT_ACCESSOR", "recordPutAccessor";
    record_set_keyed => "RECORD_SET_KEYED", "recordSetKeyed";
    record_set_sym => "RECORD_SET_SYM", "recordSetSym";
    record_descriptor => "RECORD_DESCRIPTOR", "recordDescriptor";
    record_put_sym => "RECORD_PUT_SYM", "recordPutSym";
    // Proxy: what an operation on `any` asks a proxy (`velt_vir` `any_proxy.rs` & co.).
    proxy_get_getter => "PROXY_GET_GETTER", "proxyGetGetter";
    proxy_reads_getter => "PROXY_READS_GETTER", "proxyReadsGetter";
    proxy_view => "PROXY_VIEW", "proxyView";
    proxy_is_view => "PROXY_IS_VIEW", "proxyIsView";
    proxy_plain => "PROXY_PLAIN", "proxyPlain";
    proxy_has_get => "PROXY_HAS_GET", "proxyHasGet";
    proxy_is_array => "PROXY_IS_ARRAY", "proxyIsArray";
    proxy_self => "PROXY_SELF", "proxySelf";
    proxy_target => "PROXY_TARGET", "proxyTarget";
    proxy_get => "PROXY_GET", "proxyGet";
    proxy_set => "PROXY_SET", "proxySet";
    proxy_has => "PROXY_HAS", "proxyHas";
    proxy_delete_property => "PROXY_DELETE_PROPERTY", "proxyDeleteProperty";
    proxy_delete => "PROXY_DELETE", "proxyDelete";
    proxy_own_keys => "PROXY_OWN_KEYS", "proxyOwnKeys";
    proxy_get_own_property_descriptor => "PROXY_GET_OWN_PROPERTY_DESCRIPTOR", "proxyGetOwnPropertyDescriptor";
    proxy_define_property => "PROXY_DEFINE_PROPERTY", "proxyDefineProperty";
    proxy_keys => "PROXY_KEYS", "proxyKeys";
    proxy_defines => "PROXY_DEFINES", "proxyDefines";
    proxy_receive => "PROXY_RECEIVE", "proxyReceive";
    proxy_json => "PROXY_JSON", "proxyJson";
    proxy_array_call => "PROXY_ARRAY_CALL", "proxyArrayCall";
    proxy_has_own => "PROXY_HAS_OWN", "proxyHasOwn";
}

/// The runtime externs whose job moved to std and whose Rust was deleted (#2196's port tracker: an
/// entry here is a finished port): hooks took over the externs of [`Hook::replaces`], which keep
/// their place there as the record of what the hook replaced, and ordinary std code the ones only
/// std called, which no hook replaces. `velt_common`'s test `hook_replaces.rs` checks that every
/// replaced extern is defined by `velt_rt` unless it is listed here, and that one listed here is
/// defined nowhere and named by no compiler source, runtime or std file.
pub const DELETED: &[&str] = &[
    // #2197: integers as text (std/lang/hooks/number_text, velt/lang/number/integer).
    "velt_rt_str_from_i64",
    "velt_rt_str_from_u64",
    "velt_rt_strbuf_push_i64",
    "velt_rt_strbuf_push_u64",
    // #2196, only std called them: error-code names (std/native/error).
    "velt_rt_err_code_name",
];

/// The hook whose qualified name is `qualified` (`std/lang/hooks/number::f64ToString`).
pub fn find(qualified: &str) -> Option<&'static Hook> {
    let (module, item) = qualified.rsplit_once("::")?;
    let rel = module.strip_prefix(crate::std_modules::CANONICAL_PREFIX)?;
    HOOKS
        .iter()
        .find(|h| h.item == item && crate::std_modules::rel(h.module_entry) == Some(rel))
}

/// Lowered hooks that sema checks only once the program's own code converts a value that may hold
/// a float to text (`velt_sema` `lazy_std`), by their entries' names: the float hooks, whose whole
/// numbers take std's integer printing, and, where the program loaded it, std's float printing
/// (velt/lang/number/format, `shortest`, the table of powers of ten). Lowering calls the runtime's
/// printer where one is unchecked. The integer hooks have no fallback (#2197): sema checks them
/// where live code has one of their `constructs` ([`crate::hook_demand`]).
pub const ON_DEMAND_FLOAT: &[&str] = &["number_text", "number_push", "float_text", "float_push"];

/// The std modules of hooks no program needs by default: not loaded with the prelude. Sema demands
/// each when it finds the hook needed, during the check (#2161 (a1): `velt_sema` `std_load`), so
/// each follows the rule of `velt_loader::std_index`. `VELT_EAGER_STD=1`, and a caller that gives
/// the check no load callback, load them all up front, in this order.
pub const ON_DEMAND_MODULES: &[&str] = &[
    "std/lang/hooks/number_text",
    "std/lang/hooks/number_float",
    "std/lang/hooks/any_record",
];

/// The qualified names of the hooks named in `names` ([`ON_DEMAND_FLOAT`]).
pub fn on_demand(names: &'static [&'static str]) -> impl Iterator<Item = String> {
    HOOKS
        .iter()
        .filter(move |h| names.contains(&h.name))
        .map(Hook::qualified)
}

/// Is the definition with qualified name `qualified` a hook lowering calls by name?
pub fn is_lowered(qualified: &str) -> bool {
    static LOWERED: std::sync::OnceLock<std::collections::HashSet<String>> =
        std::sync::OnceLock::new();
    LOWERED
        .get_or_init(|| {
            HOOKS
                .iter()
                .filter(|h| h.lowered)
                .map(Hook::qualified)
                .collect()
        })
        .contains(qualified)
}

#[cfg(test)]
#[path = "std_hooks_tests.rs"]
mod tests;
