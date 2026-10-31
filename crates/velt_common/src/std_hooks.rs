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
//! any_methods.vlt; `velt_vir` `any_builtin.rs`).
//!
//! Each entry gives the hook's signature as `velt_sema`'s test of this table prints it (type
//! parameters `A`, `B`, … in brackets, then the parameter types and the result; a class is
//! `class`); the test loads every hook module of std and checks that each entry resolves to a
//! definition of that signature, so renaming or changing a hook fails the build, not a program.
//! `lowered` marks the hooks lowering, the optimizer or a backend call: nothing in the checked
//! code refers to them, so lazy std checks them with the prelude (`velt_sema` `lazy_std`).
//!
//! ```
//! use velt_common::{std_hook, std_hook_name};
//! assert_eq!(std_hook!(f64_to_string), "std/lang/hooks/number::f64ToString");
//! assert_eq!(std_hook_name!(f64_to_string), "f64ToString");
//! ```

/// One hook: its entry name, its module's canonical path, its name there, its signature and
/// whether lowering calls it.
#[derive(Clone, Copy, Debug)]
pub struct Hook {
    /// The entry's name (`f64_to_string`).
    pub name: &'static str,
    /// The std module table's entry of the module that defines it (`lang_hooks_number`:
    /// [`crate::std_modules`]).
    pub module_entry: &'static str,
    /// Its name in that module (`f64ToString`; `ArrayView` for a class).
    pub item: &'static str,
    /// Its signature as the test prints it (module docs).
    pub sig: &'static str,
    /// Lowering, the optimizer or a backend calls it by name.
    pub lowered: bool,
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

macro_rules! hooks {
    (@lowered) => { false };
    (@lowered lowered) => { true };
    ($($(#[doc = $doc:literal])* $name:ident => $module:ident, $item:literal, $sig:literal $(, $low:ident)?;)*) => {
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
            sig: $sig,
            lowered: hooks!(@lowered $($low)?),
        }),*];
    };
}

hooks! {
    // array.vlt: array constructors.
    /// `new Array<T>(n).fill(v)` (`velt_sema` `array_ctor.rs`).
    array_filled => lang_hooks_array, "arrayFilled", "<A>(f64, A) -> A[]";
    /// `new Array<T>(x).fill(v)` with an `x` that is no number.
    array_one => lang_hooks_array, "arrayOne", "<A, B>(A, B) -> B";
    /// `Array.from({ length: n }, f)`.
    array_from_length => lang_hooks_array, "arrayFromLength", "<A, B>(i64, (i64) => A throws B) -> A[]";

    // number.vlt: numbers as text, and the conversions of a float index.
    /// `n.toString(radix)` of a float (`velt_sema` `builtins.rs`).
    f64_to_string => lang_hooks_number, "f64ToString", "(f64, f64) -> string";
    /// `n.toString(radix)` of a signed integer.
    i64_to_string => lang_hooks_number, "i64ToString", "(i64, f64) -> string";
    /// `n.toString(radix)` of an unsigned integer.
    u64_to_string => lang_hooks_number, "u64ToString", "(u64, f64) -> string";
    /// `Math.imul`-like `(a * b) | 0` of two int32 values (`velt_sema` `int32.rs`, `velt_opt`
    /// numrep).
    mul_to_int32 => lang_hooks_number, "mulToInt32", "(f64, f64) -> i32", lowered;
    /// An array index written as a float (`xs[i]` with `i: number`): ts-compat.md.
    float_index => lang_hooks_number, "floatIndex", "(f64) -> usize", lowered;
    /// A float position argument (`xs.at(i)`, `s.slice(i)`).
    float_position => lang_hooks_number, "floatPosition", "(f64) -> usize";

    // process.vlt: the builtin `process` namespace (`velt_sema` `process.rs`).
    /// `process.stdout.write(s)` / `process.stderr.write(s)`.
    process_write => lang_hooks_process, "processWrite", "(u32, string) -> bool";
    /// `process.env.NAME` / `process.env[name]`.
    process_env => lang_hooks_process, "processEnv", "(string) -> string | undefined";
    /// `process.argv`.
    process_argv => lang_hooks_process, "processArgv", "() -> string[]";
    /// The mark of the top-level `await`s after it (`velt_sema` `top_level_await.rs`); lowering
    /// replaces its calls by the runtime's (`async_fn/suspend.rs`).
    top_level_await_at => lang_hooks_process, "topLevelAwaitAt", "(string) -> void", lowered;

    // syntax.vlt: what the parser writes some syntax as (velt_syntax `ast_names.rs`).
    /// `value satisfies T`.
    satisfies => lang_hooks_syntax, "satisfies", "<A>(A) -> A";
    /// A getter in an object literal.
    object_get => lang_hooks_syntax, "objectGet", "<A>(() => A) -> A";
    /// A setter in an object literal.
    object_set => lang_hooks_syntax, "objectSet", "<A>((A) => void) -> (A) => void";
    /// `{ [key]: value }` with a key known only at run time.
    computed_key => lang_hooks_syntax, "computedKey", "<A>(string, A) -> Record<string, A>";

    // adapt.vlt: interface adapters of `unknown[]` and `Map<unknown, unknown>` (`velt_sema`
    // `iface_adapt.rs`).
    unknown_array_in => lang_hooks_adapt, "unknownArrayIn", "(unknown) -> unknown[]";
    unknown_array_out => lang_hooks_adapt, "unknownArrayOut", "(unknown, unknown[]) -> void";
    unknown_map_in => lang_hooks_adapt, "unknownMapIn", "(unknown) -> Map<unknown, unknown>";
    unknown_map_out => lang_hooks_adapt, "unknownMapOut", "(unknown, Map<unknown, unknown>) -> void";

    // any_text.vlt: the text of an `any` (`velt_sema` `any_text.rs`); lowering keeps one instance of
    // each and passes the call site (`velt_vir` `track_caller.rs`).
    any_string => lang_hooks_any_text, "anyString", "(any, string) -> string", lowered;
    any_concat_text => lang_hooks_any_text, "anyConcatText", "(any, string) -> string", lowered;
    any_concat_plain => lang_hooks_any_text, "anyConcatPlain", "(any, string) -> string", lowered;
    any_prim_text => lang_hooks_any_text, "anyPrimText", "(any, string) -> string", lowered;
    any_prim_try => lang_hooks_any_text, "anyPrimTry", "(any, string) -> string | null", lowered;
    any_text_unsupported => lang_hooks_any_text, "anyTextUnsupported", "(string, string) -> string", lowered;
    any_text => lang_hooks_any_text, "anyText", "(any, string) -> string", lowered;
    any_record_text => lang_hooks_any_text, "anyRecordText", "(any, string) -> string";
    any_record_concat => lang_hooks_any_text, "anyRecordConcat", "(any, string) -> string";
    any_join => lang_hooks_any_text, "anyJoin", "(any, string, string) -> string", lowered;
    any_join_method => lang_hooks_any_text, "anyJoinMethod", "(any, any, string) -> any";
    any_join_seen => lang_hooks_any_text, "anyJoinSeen", "(any, string, any[], string) -> string", lowered;

    // any_object.vlt: `Object.*`, spreads, prototypes and `delete` of an `any`.
    spread_any => lang_hooks_any_object, "spreadAny", "(Record<string, any>, any) -> void";
    any_has_own => lang_hooks_any_object, "anyHasOwn", "(any, string | symbol) -> bool";
    any_member => lang_hooks_any_object, "anyMember", "(any, string) -> any";
    any_values => lang_hooks_any_object, "anyValues", "(any) -> any[]";
    any_entries => lang_hooks_any_object, "anyEntries", "(any) -> [string, any][]";
    any_prototype => lang_hooks_any_object, "anyPrototype", "(any) -> ObjectPrototype | null";
    any_record_prototype => lang_hooks_any_object, "anyRecordPrototype", "(any) -> ObjectPrototype | null";
    any_prototype_unsupported => lang_hooks_any_object, "anyPrototypeUnsupported", "(string) -> ObjectPrototype | null";
    any_object_tag => lang_hooks_any_object, "anyObjectTag", "(any) -> string";
    any_tag_unsupported => lang_hooks_any_object, "anyTagUnsupported", "(string) -> string";
    any_ctor_name => lang_hooks_any_object, "anyCtorName", "(any) -> string";
    any_ctor_name_unsupported => lang_hooks_any_object, "anyCtorNameUnsupported", "(string) -> string";
    any_delete_member => lang_hooks_any_object, "anyDeleteMember", "(any, any) -> bool";
    has_own_property => lang_hooks_any_object, "HasOwnProperty", "class", lowered;

    // any_ops.vlt: operators on `any` values holding objects (`ToPrimitive` first).
    any_primitive => lang_hooks_any_ops, "anyPrimitive", "(any, string, string) -> any", lowered;
    any_primitive_plain => lang_hooks_any_ops, "anyPrimitivePlain", "(any, string, string) -> any", lowered;
    any_prim_keep => lang_hooks_any_ops, "anyPrimKeep", "(any, string) -> any", lowered;
    any_prim_then => lang_hooks_any_ops, "anyPrimThen", "(any, () => any) -> any";
    any_object_number => lang_hooks_any_ops, "anyObjectNumber", "(any, string) -> f64";
    any_add => lang_hooks_any_ops, "anyAdd", "(any, any, string) -> any";
    any_relational => lang_hooks_any_ops, "anyRelational", "(any, any, i64, string) -> bool";
    any_loose_eq => lang_hooks_any_ops, "anyLooseEq", "(any, any, string) -> bool", lowered;

    // class_object.vlt: classes as values, and `x.constructor` of an `any`.
    class_marker => lang_hooks_class_object, "ClassMarker", "class", lowered;
    class_object => lang_hooks_class_object, "classObject", "(string, string) -> (ClassMarker<string>[]) => any", lowered;
    class_object_statics => lang_hooks_class_object, "classObjectStatics", "(string, string) -> (ClassMarker<bool>[]) => any";
    any_ctor => lang_hooks_class_object, "anyCtor", "(any, any, any, any, any, any, any) -> any";
    any_record_ctor => lang_hooks_class_object, "anyRecordCtor", "(any, any) -> any";
    any_constructor => lang_hooks_class_object, "anyConstructor", "(any) -> any";
    any_ctor_local => lang_hooks_class_object, "anyCtorLocal", "(string) -> any";
    any_ctor_unsupported => lang_hooks_class_object, "anyCtorUnsupported", "(string) -> any";

    // array_view.vlt: an `any` holding an array viewed as `T[]` (`velt_vir` `array_view.rs`).
    array_view => lang_hooks_array_view, "ArrayView", "class", lowered;
    array_view_new => lang_hooks_array_view, "arrayViewNew", "<A>(any) -> ArrayView<A>", lowered;
    array_view_sync => lang_hooks_array_view, "arrayViewSync", "<A>(ArrayView<A>) -> void", lowered;
    array_view_flush => lang_hooks_array_view, "arrayViewFlush", "<A>(ArrayView<A>) -> void", lowered;
    array_view_flush_from => lang_hooks_array_view, "arrayViewFlushFrom", "<A>(ArrayView<A>, usize) -> void", lowered;
    array_view_len => lang_hooks_array_view, "arrayViewLen", "<A>(ArrayView<A>) -> usize", lowered;
    array_view_sync_at => lang_hooks_array_view, "arrayViewSyncAt", "<A>(ArrayView<A>, usize) -> void", lowered;
    array_view_sync_len => lang_hooks_array_view, "arrayViewSyncLen", "<A>(ArrayView<A>, bool) -> void", lowered;
    array_view_flush_at => lang_hooks_array_view, "arrayViewFlushAt", "<A>(ArrayView<A>, usize) -> void", lowered;
    array_view_sort => lang_hooks_array_view, "arrayViewSort", "<A, B>(ArrayView<A>, (A, A) => B, (B) => bool) -> void", lowered;


    // record.vlt: `k in r` for a key known only at run time, and `===` on prototypes.
    prototype_same => lang_hooks_record, "prototypeSame", "(ObjectPrototype | null, ObjectPrototype | null) -> bool";
    prototype_in => lang_hooks_record, "prototypeIn", "(string, ObjectPrototype) -> bool";
    record_in => lang_hooks_record, "recordIn", "<A>(string, Record<string, A>) -> bool";
    record_in_maybe_null_proto => lang_hooks_record, "recordInMaybeNullProto", "<A>(string, Record<string, A>) -> bool";
    // error.vlt: the first line of the report of an uncaught error (`velt_vir` `entry.rs`).
    uncaught_header => lang_hooks_error, "uncaughtHeader", "(Error, string) -> string", lowered;

    // string.vlt: the characters a `for...of` over a string iterates.
    string_code_points => lang_hooks_string, "stringCodePoints", "(string) -> string[]";

    // iter.vlt, async_iter.vlt: iterable object literals.
    async_iterable_object => lang_hooks_async_iter, "AsyncIterableObject", "class";
    iterable_object => lang_hooks_iter, "IterableObject", "class";

    // proxy.vlt: the operations on `any` that ask a proxy, used once a program has loaded `Proxy`
    // (std/prelude/global/proxy.vlt).
    any_delete_proxy => lang_hooks_proxy, "anyDeleteProxy", "(any, any) -> bool";
    any_delete_sym => lang_hooks_proxy, "anyDeleteSym", "(any, string, symbol) -> bool";
    any_entries_proxy => lang_hooks_proxy, "anyEntriesProxy", "(any) -> [string, any][]";
    any_for_in_has => lang_hooks_proxy, "anyForInHas", "(any, string) -> bool";
    any_for_in_keys => lang_hooks_proxy, "anyForInKeys", "(any) -> string[]";
    any_get_key => lang_hooks_proxy, "anyGetKey", "(any, any) -> any";
    any_get_sym => lang_hooks_proxy, "anyGetSym", "(any, string, symbol) -> any";
    any_has_key => lang_hooks_proxy, "anyHasKey", "(any, any) -> bool";
    any_has_own_proxy => lang_hooks_proxy, "anyHasOwnProxy", "(any, string | symbol) -> bool";
    any_has_own_sym => lang_hooks_proxy, "anyHasOwnSym", "(any, string, symbol) -> bool";
    any_has_sym => lang_hooks_proxy, "anyHasSym", "(any, string, symbol) -> bool";
    any_is_array_proxy => lang_hooks_proxy, "anyIsArrayProxy", "(any) -> bool";
    any_set_key => lang_hooks_proxy, "anySetKey", "(any, any, any) -> void";
    any_set_sym => lang_hooks_proxy, "anySetSym", "(any, string, symbol, any) -> void";
    any_values_proxy => lang_hooks_proxy, "anyValuesProxy", "(any) -> any[]";
    brand_getter_read => lang_hooks_proxy, "brandGetterRead", "(any, string) -> any";
    has_own_property_proxy => lang_hooks_proxy, "HasOwnPropertyProxy", "class", lowered;
    proxy_any => lang_hooks_proxy, "proxyAny", "(any) -> any";
    proxy_has => lang_hooks_proxy, "proxyHas", "(any, string) -> bool";
    proxy_has_own_of => lang_hooks_proxy, "proxyHasOwnOf", "(any, string) -> bool";
    proxy_keys_of => lang_hooks_proxy, "proxyKeysOf", "(any) -> string[]";
    proxy_new_any => lang_hooks_proxy, "proxyNewAny", "(any, any) -> any";
    proxy_spread_check => lang_hooks_proxy, "proxySpreadCheck", "(any) -> void";
    spread_any_proxy => lang_hooks_proxy, "spreadAnyProxy", "(Record<string, any>, any) -> void";

    // Outside lang/hooks, defined where what they need is (none is exported to programs):
    // lang/proxy/array.vlt: an array method read through a proxy.
    array_method => lang_proxy_array, "ArrayMethod", "class", lowered;

    // lang/this_fn: a function whose `this` is bound at the call (a global loaded on demand).
    call_on => lang_this_fn, "callOn", "(any, any, any[]) -> any";
    call_opt_on => lang_this_fn, "callOptOn", "(any, any, () => any[]) -> any";
    this_fn => lang_this_fn, "ThisFn", "class", lowered;

    // collections/set.vlt: the adapter of `Set<unknown>` (not exported).
    unknown_set_in => collections_set, "unknownSetIn", "(unknown) -> Set<unknown>";
    unknown_set_out => collections_set, "unknownSetOut", "(unknown, Set<unknown>) -> void";

    // text/regex/string_methods.vlt: the string methods called with a `RegExp` (`velt_sema`
    // `regex_args.rs`), loaded with `RegExp`.
    replace_reg_exp => text_regex_string_methods, "replaceRegExp", "(string, RegExp, string) -> string";
    replace_reg_exp_calling => text_regex_string_methods, "replaceRegExpCalling", "<A>(string, RegExp, (RegExpMatch, string) => string throws A) -> string";
    replace_all_reg_exp => text_regex_string_methods, "replaceAllRegExp", "(string, RegExp, string) -> string";
    replace_all_reg_exp_calling => text_regex_string_methods, "replaceAllRegExpCalling", "<A>(string, RegExp, (RegExpMatch, string) => string throws A) -> string";
    match_reg_exp => text_regex_string_methods, "matchRegExp", "(string, RegExp) -> string[] | null";
    match_all_reg_exp => text_regex_string_methods, "matchAllRegExp", "(string, RegExp) -> RegExpMatch[]";
    search_reg_exp => text_regex_string_methods, "searchRegExp", "(string, RegExp) -> i64";
    split_reg_exp => text_regex_string_methods, "splitRegExp", "(string, RegExp, f64 | undefined) -> string[]";

    // test/registry.vlt, test/run.vlt: what `velt test`'s generated `main` calls (veltc
    // `commands/test/harness.rs`; the loader lets only the harness import these files).
    register_file => test_registry, "registerFile", "(i64) -> void";
    register_test => test_registry, "registerTest", "(string, () => void throws unknown) -> void";
    register_async_test => test_registry, "registerAsyncTest", "<A>(string, () => Promise<A, unknown>) -> void";
    run_registered => test_run, "runRegistered", "() -> void throws JsonError | IoError";

    // lang/typed_array/core.vlt: the bytes of a typed array.
    view_bytes => lang_typed_array_core, "viewBytes", "(BufferView) -> usize", lowered;
}

/// The hook whose qualified name is `qualified` (`std/lang/hooks/number::f64ToString`).
pub fn find(qualified: &str) -> Option<&'static Hook> {
    let (module, item) = qualified.rsplit_once("::")?;
    let rel = module.strip_prefix(crate::std_modules::CANONICAL_PREFIX)?;
    HOOKS
        .iter()
        .find(|h| h.item == item && crate::std_modules::rel(h.module_entry) == Some(rel))
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
mod tests {
    use super::*;

    #[test]
    fn names_are_unique() {
        let mut names: Vec<&str> = HOOKS.iter().map(|h| h.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), HOOKS.len(), "a hook is listed twice");
        let mut items: Vec<String> = HOOKS.iter().map(|h| h.qualified()).collect();
        items.sort();
        items.dedup();
        assert_eq!(items.len(), HOOKS.len(), "two entries name one definition");
    }

    #[test]
    fn every_hook_is_in_lang() {
        // Outside `lang/` only where the hook needs a module's internals, in a file no program
        // can import (not a module's `index.vlt`).
        for h in HOOKS {
            let rel = crate::std_modules::rel(h.module_entry).expect("ICE: a std module");
            assert!(
                h.module().starts_with("std/lang/")
                    || !crate::std_modules::importable(&format!("{rel}.vlt")),
                "{}: {}",
                h.name,
                h.module()
            );
        }
        let f64_to_string = "std/lang/hooks/number::f64ToString";
        assert_eq!(find(f64_to_string).map(|h| h.name), Some("f64_to_string"));
        assert!(is_lowered("std/lang/hooks/number::floatIndex"));
        assert!(!is_lowered(f64_to_string));
        assert!(find("std/lang/hooks/number::nothing").is_none());
    }
}
