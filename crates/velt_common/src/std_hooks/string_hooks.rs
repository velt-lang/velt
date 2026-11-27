//! The string methods' rows of the hook table (std/lang/string/, #2198), and a byte range as a
//! string, `process.argv`'s list and a number stored into an integer array (#2196, #3182): the searches, `trim`,
//! `repeat`, `padStart` / `padEnd`, `split`, `replace` / `replaceAll` and the case mapping, which
//! the prelude's methods call by role (`hookCall`), so a program loads a method's module only
//! when live code calls it; `replace` with a function, which sema calls; and `Date`'s zone name,
//! a string the prelude's `toTimeString` gets by role the same way. Those that take and return only strings and numbers are `prebuilt`. Kept
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

            // velt/lang/bytes/range_text: a byte range as a string (#2196), which the prelude's
            // `utf8Slice` calls by role, so a program loads the decoder only when live code calls it.
            /// `b.utf8Slice(start, end)` with its positions clamped (`from <= to <= b.length`).
            bytes_range_text => lang_bytes_range_text, "rangeText", sig((&[u8], usize, usize) -> string),
                replaces("velt_rt_bytes_range_to_str_lossy"), bench("bytes_utf8");

            // velt/lang/hooks/process_argv: `process.argv`'s list (#2196), which `processArgv`
            // calls by role, so a program loads the arguments' readers only when live code reads it.
            /// `process.argv`'s `[runtime, script, ...args]`.
            process_node_argv => lang_hooks_process_argv, "nodeArgv", sig(() -> [string]);

            // velt/lang/hooks/int_store: a `number` stored into an element of an integer array,
            // converted as a typed array stores it (#3182; `velt_sema` `element_store.rs` calls
            // them, and only a program with such a store loads the module).
            /// `buf[i] = n` on a `u8[]` (ToUint8).
            number_to_u8 => lang_hooks_int_store, "toUint8", sig((f64) -> u8);
            /// `xs[i] = n` on an `i8[]` (ToInt8).
            number_to_i8 => lang_hooks_int_store, "toInt8", sig((f64) -> i8);
            /// `xs[i] = n` on a `u16[]` (ToUint16).
            number_to_u16 => lang_hooks_int_store, "toUint16", sig((f64) -> u16);
            /// `xs[i] = n` on an `i16[]` (ToInt16).
            number_to_i16 => lang_hooks_int_store, "toInt16", sig((f64) -> i16);
            /// `xs[i] = n` on a `u32[]` (ToUint32).
            number_to_u32 => lang_hooks_int_store, "toUint32", sig((f64) -> u32);
            /// `xs[i] = n` on an `i32[]` (ToInt32).
            number_to_i32 => lang_hooks_int_store, "toInt32", sig((f64) -> i32);
            /// `s.repeat(count)` of a count the prelude's method has checked.
            string_repeat => lang_string_fill, "repeat", sig((&string, i64) -> string),
                replaces("velt_rt_str_repeat"), bench("string_methods"), prebuilt;
            /// `s.padStart(target, fill)`.
            string_pad_start => lang_string_fill, "padStart", sig((&string, i64, &string) -> string),
                replaces("velt_rt_str_pad_start"), bench("string_methods"), prebuilt;
            /// `s.padEnd(target, fill)`.
            string_pad_end => lang_string_fill, "padEnd", sig((&string, i64, &string) -> string),
                replaces("velt_rt_str_pad_end"), bench("string_methods"), prebuilt;

            // velt/lang/hooks/date_zone_name: the zone name `Date`'s `toString()` and
            // `toTimeString()` end with (#3152), called by role, so a program loads the table of
            // names only when live code calls one.
            /// The local zone's long name at `(t, offset)`, ms since the epoch and the offset there
            /// in seconds, as Node prints it.
            date_zone_name => lang_hooks_date_zone_name, "dateZoneName", sig((f64, i64) -> string);
        }
    };
}
