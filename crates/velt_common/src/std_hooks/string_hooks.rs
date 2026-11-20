//! The string methods' rows of the hook table (std/lang/string/, #2198): the searches, `trim`,
//! `repeat`, `padStart` / `padEnd`, `split`, `replace` / `replaceAll` and the case mapping, which
//! the prelude's methods call by role (`hookCall`), so a program loads a method's module only
//! when live code calls it; and `replace` with a function, which sema calls. Those that take and return only strings and numbers are `prebuilt`. Kept
//! apart from the rest of the table ([`super`]), which [`with_string_hooks`] gives to
//! `with_weak_hooks!` with these rows after it.

/// `with_weak_hooks!` over the rows given, then the string methods'.
macro_rules! with_string_hooks {
    ($($rows:tt)*) => {
        with_weak_hooks! {
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
