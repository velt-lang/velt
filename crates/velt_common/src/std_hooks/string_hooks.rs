//! The string methods' rows of the hook table (std/lang/string/, #2198): the searches, `trim`,
//! `repeat`, `padStart` / `padEnd`, `split`, `replace` / `replaceAll` and the case mapping, which
//! the prelude's methods call by role (`hookCall`), so a program loads a method's module only
//! when live code calls it; and `replace` with a function, which sema calls. Those that take and return only strings and numbers are `prebuilt`. Kept
//! apart from the rest of the table ([`super`]), which [`with_string_hooks`] gives to
//! `with_inspect_layout_hooks!` with these rows after it.

/// `with_inspect_layout_hooks!` over the rows given, then the string methods'.
macro_rules! with_string_hooks {
    ($($rows:tt)*) => {
        with_inspect_layout_hooks! {
            $($rows)*
            // velt/lang/string/search: the string searches (#2198), which the prelude's methods call by
            // role (`hookCall`), so a program loads them only when live code calls one.
            /// `s.indexOf(needle, from)`.
            string_index_of => lang_string_search, "indexOf", sig((&string, &string, i64) -> i64);
            /// `s.lastIndexOf(needle, from)`.
            string_last_index_of => lang_string_search, "lastIndexOf", sig((&string, &string, i64) -> i64);
            /// `s.includes(needle, position)`.
            string_includes => lang_string_search, "includes", sig((&string, &string, i64) -> bool);
            /// `s.startsWith(prefix)`.
            string_starts_with => lang_string_search, "startsWith", sig((&string, &string) -> bool);
            /// `s.endsWith(suffix)`.
            string_ends_with => lang_string_search, "endsWith", sig((&string, &string) -> bool);
            /// `s.replace(from, (match, offset, string) => …)` with a string pattern (`velt_sema`
            /// `regex_args.rs`): a function taking fewer of JavaScript's arguments converts as any
            /// function value does.
            replace_string_calling => lang_hooks_string, "replaceCalling",
                sig(<A>(&mut string, &mut string, &Fn((string, f64, string) -> string throws A)) -> string throws A);
            /// `s.replaceAll(from, (match, offset, string) => …)` with a string pattern.
            replace_all_string_calling => lang_hooks_string, "replaceAllCalling",
                sig(<A>(&mut string, &mut string, &Fn((string, f64, string) -> string throws A)) -> string throws A);

            // velt/lang/string/trim and fill: the methods that return part of a string or build one
            // (#2198), called by role as the searches are.
            /// `s.trim()`.
            string_trim => lang_string_trim, "trim", sig((&string) -> string),
                replaces("velt_rt_str_trim"), bench("string_methods"), prebuilt;
            /// `s.trimStart()`.
            string_trim_start => lang_string_trim, "trimStart", sig((&string) -> string),
                replaces("velt_rt_str_trim_start"), bench("string_methods"), prebuilt;
            /// `s.trimEnd()`.
            string_trim_end => lang_string_trim, "trimEnd", sig((&string) -> string),
                replaces("velt_rt_str_trim_end"), bench("string_methods"), prebuilt;
            /// `s.split(sep, limit)` with a string separator; `limit` after JavaScript's `ToUint32`.
            string_split => lang_string_split, "split", sig((&string, &string, u32) -> [string]),
                replaces("velt_rt_str_split"), bench("string_methods");
            /// `s.replace(from, to)` with a string pattern.
            string_replace => lang_string_replace, "replace", sig((&string, &string, &string) -> string),
                replaces("velt_rt_str_replace"), bench("string_methods"), prebuilt;
            /// `s.replaceAll(from, to)` with a string pattern.
            string_replace_all => lang_string_replace, "replaceAll",
                sig((&string, &string, &string) -> string),
                replaces("velt_rt_str_replace_all"), bench("string_methods"), prebuilt;
            /// `s.toUpperCase()`: ASCII text in Velt, other text through the runtime's Unicode mapping.
            string_to_upper => lang_string_case, "toUpperCase", sig((&string) -> string),
                bench("string_methods"), prebuilt;
            /// `s.toLowerCase()`, as `string_to_upper`.
            string_to_lower => lang_string_case, "toLowerCase", sig((&string) -> string),
                bench("string_methods"), prebuilt;
            /// `s.repeat(count)` of a count the prelude's method has checked.
            string_repeat => lang_string_fill, "repeat", sig((&string, i64) -> string),
                replaces("velt_rt_str_repeat"), bench("string_methods"), prebuilt;
            /// `s.padStart(target, fill)`.
            string_pad_start => lang_string_fill, "padStart", sig((&string, i64, &string) -> string),
                replaces("velt_rt_str_pad_start"), bench("string_methods"), prebuilt;
            /// `s.padEnd(target, fill)`.
            string_pad_end => lang_string_fill, "padEnd", sig((&string, i64, &string) -> string),
                replaces("velt_rt_str_pad_end"), bench("string_methods"), prebuilt;
        }
    };
}
