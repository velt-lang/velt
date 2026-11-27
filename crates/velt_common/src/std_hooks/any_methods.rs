//! The built-in methods of values held as `any` (#924, #2197): the rows of the hook table that
//! lowering calls for `x.name(args)` on an `any` holding a string, an array, a number, a boolean, a
//! promise, a `Record<string, V>`, a `Map` or a `Set` (`velt_vir` `any_builtin.rs`). Each is a std
//! function taking the value and the arguments as an `any[]` (a `Map`'s and a `Set`'s also the
//! value as an `any` of its own class) and giving an `any`, named by its receiver's prefix and the
//! method's name (`anyStr_trim`: [`Receiver::prefix`]). Lowering finds a receiver's methods here
//! ([`methods`]), each through its row, so a method renamed or missing in std fails
//! `velt_sema`'s test of the table, and a module loaded without one of its rows is an internal
//! error (`velt_vir` `std_defs.rs`), not a call that silently isn't there.
//!
//! Kept apart from the rest of the table ([`super`]), which [`with_any_method_hooks`] gives to
//! `hooks!` with these rows after it.

use super::{Hook, HOOKS};

/// The kind of value a built-in method of an `any` is called on (a `method(…)` row's flag).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Receiver {
    /// A string: `anyStr_<name>(s, args)`.
    Str,
    /// An array of `T`: `anyArr_<name><T>(a, args)`.
    Arr,
    /// A number (an integer read through `any` is a `number`): `anyNum_<name>(n, args)`.
    Num,
    /// A boolean: `anyBool_<name>(b, args)`.
    Bool,
    /// A function value. It has no row yet: `call`, `apply`, `bind` and `toString` are not
    /// supported, and any other name is not a function.
    Fn,
    /// A promise: `anyPromise_<name><T, E>(p, args)`; `then`, `catch` and `finally` come through
    /// sema's own route (`any_promise_members.rs`).
    Promise,
    /// A `Record<string, V>`: `anyRec_<name><V>(r, args)`, called for the key of std's symbol
    /// `record<Name>` ([`record_key`]), which no program's key reaches.
    Rec,
    /// A `Map<K, V>` (or a class extending it): `anyMap_<name><K, V>(m, args, whole)`.
    Map,
    /// A `Set<T>` (or a class extending it): `anySet_<name><T>(s, args, whole)`.
    Set,
}

impl Receiver {
    /// The prefix of its methods' names in std: `anyStr_`.
    pub const fn prefix(self) -> &'static str {
        match self {
            Receiver::Str => "anyStr_",
            Receiver::Arr => "anyArr_",
            Receiver::Num => "anyNum_",
            Receiver::Bool => "anyBool_",
            Receiver::Fn => "anyFn_",
            Receiver::Promise => "anyPromise_",
            Receiver::Rec => "anyRec_",
            Receiver::Map => "anyMap_",
            Receiver::Set => "anySet_",
        }
    }

    /// The receiver of a std class's methods (`Map`, `Set`), by the class's name.
    pub fn of_class(class: &str) -> Option<Receiver> {
        match class {
            "Map" => Some(Receiver::Map),
            "Set" => Some(Receiver::Set),
            _ => None,
        }
    }
}

/// Is `item` named as a method of `r` (its prefix, then a name)? Used where the table is compiled.
pub(crate) const fn named_by(item: &str, r: Receiver) -> bool {
    let (item, prefix) = (item.as_bytes(), r.prefix().as_bytes());
    if item.len() <= prefix.len() {
        return false;
    }
    let mut i = 0;
    while i < prefix.len() {
        if item[i] != prefix[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The methods of receiver `r`, in table order: each one's JavaScript name (`trim`) and its row.
pub fn methods(r: Receiver) -> impl Iterator<Item = (&'static str, &'static Hook)> {
    HOOKS
        .iter()
        .filter(move |h| h.method == Some(r))
        .map(move |h| {
            let name = h.item.strip_prefix(r.prefix()).unwrap_or_else(|| {
                panic!("ICE: method hook `{}` is not named by its prefix", h.name)
            });
            (name, h)
        })
}

/// Is `item` (a definition's last name segment) a built-in method of a value held as `any`, a row
/// of this table?
pub fn is_any_method(item: &str) -> bool {
    // Every prefix is `any`, a capital and `_`: lazy std asks this of every std function, most of
    // which the first test rules out without a walk of the table.
    item.starts_with("any")
        && item.contains('_')
        && HOOKS.iter().any(|h| h.method.is_some() && h.item == item)
}

/// The member name of std's symbol `record<Name>` (std/lang/hooks/keys.velt: `RECORD_SYM_GET` for
/// `symGet`) that std calls a record's method `name` by: `o[RECORD_SYM_GET](k)` on an `any`
/// holding a record. Every [`Receiver::Rec`] row's is a constant of keys.velt
/// (`std_hooks_tests.rs`).
pub fn record_key(name: &str) -> String {
    let mut c = name.chars();
    let first = c.next().map(|f| f.to_ascii_uppercase()).unwrap_or_default();
    format!("\u{FFFF}Symbol(record{first}{})", c.as_str())
}

/// `hooks!` over the rows given, then the built-in methods of values held as `any`.
macro_rules! with_any_method_hooks {
    ($($rows:tt)*) => {
        hooks! {
            $($rows)*
            // any_methods.velt: a string's (`s` and the arguments as an `any[]`).
            any_str_trim => lang_hooks_any_methods, "anyStr_trim", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_trim_start => lang_hooks_any_methods, "anyStr_trimStart", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_trim_end => lang_hooks_any_methods, "anyStr_trimEnd", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_to_upper_case => lang_hooks_any_methods, "anyStr_toUpperCase", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_to_lower_case => lang_hooks_any_methods, "anyStr_toLowerCase", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_to_string => lang_hooks_any_methods, "anyStr_toString", sig((move string, &[any]) -> any), lowered, method(Str);
            any_str_value_of => lang_hooks_any_methods, "anyStr_valueOf", sig((move string, &[any]) -> any), lowered, method(Str);
            any_str_slice => lang_hooks_any_methods, "anyStr_slice", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_substring => lang_hooks_any_methods, "anyStr_substring", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_index_of => lang_hooks_any_methods, "anyStr_indexOf", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_last_index_of => lang_hooks_any_methods, "anyStr_lastIndexOf", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_includes => lang_hooks_any_methods, "anyStr_includes", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_starts_with => lang_hooks_any_methods, "anyStr_startsWith", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_ends_with => lang_hooks_any_methods, "anyStr_endsWith", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_split => lang_hooks_any_methods, "anyStr_split", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_replace => lang_hooks_any_methods, "anyStr_replace", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_replace_all => lang_hooks_any_methods, "anyStr_replaceAll", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_char_at => lang_hooks_any_methods, "anyStr_charAt", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_char_code_at => lang_hooks_any_methods, "anyStr_charCodeAt", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_code_point_at => lang_hooks_any_methods, "anyStr_codePointAt", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_at => lang_hooks_any_methods, "anyStr_at", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_repeat => lang_hooks_any_methods, "anyStr_repeat", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_pad_start => lang_hooks_any_methods, "anyStr_padStart", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_pad_end => lang_hooks_any_methods, "anyStr_padEnd", sig((&string, &[any]) -> any), lowered, method(Str);
            any_str_concat => lang_hooks_any_methods, "anyStr_concat", sig((move string, &[any]) -> any), lowered, method(Str);
            // locale_compare.velt, loaded with the method `localeCompare` itself.
            any_str_locale_compare => lang_hooks_locale_compare, "anyStr_localeCompare", sig((&string, &[any]) -> any), lowered, method(Str);
            // A number's, a boolean's and a promise's.
            any_num_to_fixed => lang_hooks_any_methods, "anyNum_toFixed", sig((f64, &[any]) -> any), lowered, method(Num);
            any_num_to_string => lang_hooks_any_methods, "anyNum_toString", sig((f64, &[any]) -> any), lowered, method(Num);
            any_num_to_precision => lang_hooks_any_methods, "anyNum_toPrecision", sig((f64, &[any]) -> any), lowered, method(Num);
            any_num_value_of => lang_hooks_any_methods, "anyNum_valueOf", sig((f64, &[any]) -> any), lowered, method(Num);
            any_bool_to_string => lang_hooks_any_methods, "anyBool_toString", sig((bool, &[any]) -> any), lowered, method(Bool);
            any_bool_value_of => lang_hooks_any_methods, "anyBool_valueOf", sig((bool, &[any]) -> any), lowered, method(Bool);
            any_promise_to_string => lang_hooks_any_methods, "anyPromise_toString", sig(<A, B>(&Promise(A, B), &[any]) -> any), lowered, method(Promise);
            any_promise_value_of => lang_hooks_any_methods, "anyPromise_valueOf", sig(<A, B>(move Promise(A, B), &[any]) -> any), lowered, method(Promise);
            // any_array_methods.velt: an array's, at its element type.
            any_arr_push => lang_hooks_any_array_methods, "anyArr_push", sig(<A>(&mut [A], &[any]) -> any), lowered, method(Arr);
            any_arr_pop => lang_hooks_any_array_methods, "anyArr_pop", sig(<A>(&mut [A], &[any]) -> any), lowered, method(Arr);
            any_arr_shift => lang_hooks_any_array_methods, "anyArr_shift", sig(<A>(&mut [A], &[any]) -> any), lowered, method(Arr);
            any_arr_unshift => lang_hooks_any_array_methods, "anyArr_unshift", sig(<A>(&mut [A], &[any]) -> any), lowered, method(Arr);
            any_arr_splice => lang_hooks_any_array_methods, "anyArr_splice", sig(<A>(&mut [A], &[any]) -> any), lowered, method(Arr);
            any_arr_slice => lang_hooks_any_array_methods, "anyArr_slice", sig(<A>(&[A], &[any]) -> any), lowered, method(Arr);
            any_arr_at => lang_hooks_any_array_methods, "anyArr_at", sig(<A>(&[A], &[any]) -> any), lowered, method(Arr);
            any_arr_reverse => lang_hooks_any_array_methods, "anyArr_reverse", sig(<A>(move [A], &[any]) -> any), lowered, method(Arr);
            any_arr_copy_within => lang_hooks_any_array_methods, "anyArr_copyWithin", sig(<A>(move [A], &[any]) -> any), lowered, method(Arr);
            any_arr_sort => lang_hooks_any_array_methods, "anyArr_sort", sig(<A>(move [A], &[any]) -> any), lowered, method(Arr);
            any_arr_concat => lang_hooks_any_array_methods, "anyArr_concat", sig(<A>(&[A], &[any]) -> any), lowered, method(Arr);
            any_arr_index_of => lang_hooks_any_array_methods, "anyArr_indexOf", sig(<A>(&[A], &[any]) -> any), lowered, method(Arr);
            any_arr_last_index_of => lang_hooks_any_array_methods, "anyArr_lastIndexOf", sig(<A>(&[A], &[any]) -> any), lowered, method(Arr);
            any_arr_includes => lang_hooks_any_array_methods, "anyArr_includes", sig(<A>(&[A], &[any]) -> any), lowered, method(Arr);
            any_arr_map => lang_hooks_any_array_methods, "anyArr_map", sig(<A>(move [A], &[any]) -> any), lowered, method(Arr);
            any_arr_for_each => lang_hooks_any_array_methods, "anyArr_forEach", sig(<A>(move [A], &[any]) -> any), lowered, method(Arr);
            any_arr_filter => lang_hooks_any_array_methods, "anyArr_filter", sig(<A>(move [A], &[any]) -> any), lowered, method(Arr);
            any_arr_find => lang_hooks_any_array_methods, "anyArr_find", sig(<A>(move [A], &[any]) -> any), lowered, method(Arr);
            any_arr_find_index => lang_hooks_any_array_methods, "anyArr_findIndex", sig(<A>(move [A], &[any]) -> any), lowered, method(Arr);
            any_arr_some => lang_hooks_any_array_methods, "anyArr_some", sig(<A>(move [A], &[any]) -> any), lowered, method(Arr);
            any_arr_every => lang_hooks_any_array_methods, "anyArr_every", sig(<A>(move [A], &[any]) -> any), lowered, method(Arr);
            any_arr_reduce => lang_hooks_any_array_methods, "anyArr_reduce", sig(<A>(move [A], &[any]) -> any), lowered, method(Arr);
            any_arr_reduce_right => lang_hooks_any_array_methods, "anyArr_reduceRight", sig(<A>(move [A], &[any]) -> any), lowered, method(Arr);
            any_arr_flat_map => lang_hooks_any_array_methods, "anyArr_flatMap", sig(<A>(move [A], &[any]) -> any), lowered, method(Arr);
            any_arr_to_string => lang_hooks_any_array_methods, "anyArr_toString", sig(<A>(move [A], &[any]) -> any), lowered, method(Arr);
            // A map's (also its file's) and a set's (collections/set.velt), with the value as an
            // `any` of its own class.
            any_map_for_each => lang_hooks_any_array_methods, "anyMap_forEach", sig(<A, B>(&Map(A, B), &[any], &any) -> any), lowered, method(Map);
            any_set_for_each => collections_set, "anySet_forEach", sig(<A>(&mut Set(A), &[any], &any) -> any), lowered, method(Set);
            // lang/record/reflect.velt: a record's, keyed by std's symbols (`Receiver::Rec`).
            any_rec_sym_get => lang_record_reflect, "anyRec_symGet", sig(<A>(&Record(string, A), &[any]) -> any), lowered, method(Rec);
            any_rec_sym_set => lang_record_reflect, "anyRec_symSet", sig(<A>(&mut Record(string, A), &[any]) -> any), lowered, method(Rec);
            any_rec_sym_has => lang_record_reflect, "anyRec_symHas", sig(<A>(&Record(string, A), &[any]) -> any), lowered, method(Rec);
            any_rec_sym_delete => lang_record_reflect, "anyRec_symDelete", sig(<A>(&mut Record(string, A), &[any]) -> any), lowered, method(Rec);
            any_rec_own_keys => lang_record_reflect, "anyRec_ownKeys", sig(<A>(&Record(string, A), &[any]) -> any), lowered, method(Rec);
            any_rec_descriptor => lang_record_reflect, "anyRec_descriptor", sig(<A>(&Record(string, A), &[any]) -> any), lowered, method(Rec);
        }
    };
}
