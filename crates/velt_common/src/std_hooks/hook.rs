//! The form of [`super`]'s table: one [`Hook`] per entry, and the `hooks!` macro that turns the
//! entries into [`super::HOOKS`], [`super::hook_index`] and the `std_hook!`, `std_hook_name!` and
//! `std_hook_table!` macros.

use super::{Construct, Sig};

/// One hook: its entry name, its module's canonical path, its name there, its signature, whether
/// lowering calls it, whether lowering may find it missing and fall back to the runtime
/// (`fallback`), whether a build may call a prebuilt object of its module instead (`prebuilt`),
/// the runtime externs it replaces and the benchmark that measures it.
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
    /// inputs: what counts is that the compiler no longer calls it in the hook's place.
    pub replaces: &'static [&'static str],
    /// Lowering does without it when the hook is missing or left unchecked (it is one of
    /// [`super::ON_DEMAND_FLOAT`], whose place `number_text` / `number_push` take, or of
    /// [`super::INSPECT_SHAPES`], whose place lowering's own glue takes): its `velt_vir` builder
    /// returns an `Option`. Every other lowered hook missing is an internal compiler error.
    pub fallback: bool,
    /// The program of `bench/stdport/` (`int_to_string` for `bench/stdport/int_to_string.velt`)
    /// that measures this hook against the Rust it replaced, if one does.
    pub bench: Option<&'static str>,
    /// The HIR constructs whose lowering calls it ([`crate::hook_demand`]): sema checks it only
    /// where live code has one, and lowering asserts that. None: always needed.
    pub constructs: &'static [Construct],
    /// A build may call it in an object of its module compiled once ([`super::prebuilt`]): where
    /// the build cache holds that object, a `hookCall` of it is a call of the declared function
    /// [`super::prebuilt::symbol`] and the module is not loaded. Only for a function whose
    /// parameters and result are numbers and strings ([`super::prebuilt::scalar`], checked when
    /// the table is compiled).
    pub prebuilt: bool,
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

/// An entry is `name => module, "item", sig(…)` (or `class`, `decorator`), then optional flags
/// after commas, in any order: `lowered`, `fallback`, `prebuilt`, `replaces("velt_rt_…", …)`,
/// `bench("row")` and `constructs(TextInt, …)` (the fields of [`Hook`]).
macro_rules! hooks {
    // Each flag is one of these (an unknown one fails here).
    (@flag lowered) => {};
    (@flag fallback) => {};
    (@flag prebuilt) => {};
    (@flag replaces) => {};
    (@flag bench) => {};
    (@flag constructs) => {};
    (@lowered) => { false };
    (@lowered lowered $($rest:tt)*) => { true };
    (@lowered $skip:tt $($rest:tt)*) => { hooks!(@lowered $($rest)*) };
    (@fallback) => { false };
    (@fallback fallback $($rest:tt)*) => { true };
    (@fallback $skip:tt $($rest:tt)*) => { hooks!(@fallback $($rest)*) };
    (@prebuilt) => { false };
    (@prebuilt prebuilt $($rest:tt)*) => { true };
    (@prebuilt $skip:tt $($rest:tt)*) => { hooks!(@prebuilt $($rest)*) };
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
            prebuilt: hooks!(@prebuilt $($flag $(($($arg)*))?)*),
        }),*];

        // A `prebuilt` entry takes and returns only numbers and strings: refused when the
        // compiler is built, never by a build that would call it (`prebuilt::scalar`).
        const _: () = {
            let mut i = 0;
            while i < HOOKS.len() {
                assert!(
                    !HOOKS[i].prebuilt || $crate::std_hooks::prebuilt::scalar(&HOOKS[i].sig),
                    "a `prebuilt` hook takes and returns only numbers and strings"
                );
                i += 1;
            }
        };

        #[allow(non_camel_case_types, dead_code)]
        enum Index { $($name),* }

        /// Each hook's index in [`HOOKS`] (a [`HookSet`]'s bit): `hook_index::int_push`.
        #[allow(non_upper_case_globals)]
        pub mod hook_index {
            $(pub const $name: usize = super::Index::$name as usize;)*
        }
    };
}
