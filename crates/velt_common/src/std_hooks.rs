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
//! void)`, `class`, or `decorator` for a compile-time decorator the compiler acts on, #2203;
//! [`crate::hook_sig`]), which the table turns into a [`Sig`]: `velt_sema`'s
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

pub use crate::hook_demand::{Construct, ConstructSet, HookSet};
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
    /// one of [`ON_DEMAND_FLOAT`] or [`ON_DEMAND_WRITE`]): its `velt_vir` builder returns an
    /// `Option`. Every other lowered hook missing is an internal compiler error.
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

/// An entry is `name => module, "item", sig(…)` (or `class`, `decorator`), then optional flags
/// after commas, in any order: `lowered`, `fallback`, `replaces("velt_rt_…", …)`, `bench("row")`
/// and `constructs(TextInt, …)` (the fields of [`Hook`]).
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
    /// `s.localeCompare(t)` (only std calls it: the method, and `anyStr_localeCompare` for a
    /// string held as `any`).
    locale_order => lang_string_collate, "localeOrder", sig((&string, &string) -> i64),
        replaces("velt_rt_str_locale_compare"), bench("locale_compare");
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
    /// A signed integer and the byte after it (a space or the newline) written to a stream in one
    /// write: a `console.log` argument (`velt_vir` `console.rs`). `Format`, like `int_push`, which
    /// prints it when the line isn't all scalars: the same demand, not a narrower one
    /// (`hook_demand.rs`).
    int_write => lang_hooks_number_text, "intWrite", sig((u32, i64, u8) -> void), lowered,
        replaces("velt_rt_write_i64"), constructs(Format);
    /// An unsigned integer and the byte after it, as `int_write`.
    uint_write => lang_hooks_number_text, "uintWrite", sig((u32, u64, u8) -> void), lowered,
        replaces("velt_rt_write_u64"), constructs(Format);
    /// A float as `console.log` prints it and the byte after it: a whole number below 2^53 other
    /// than `-0` by `whole_number_write`, anything else by the runtime's printer, which lowering
    /// calls while this is unchecked ([`ON_DEMAND_WRITE`]). It holds no integer hook, so a printed
    /// float checks no integer printing. numrep turns a call with such an `x` into one of
    /// `int_write` where the program has that, else of `whole_number_write`.
    number_write => lang_hooks_number_write, "numberWrite", sig((u32, f64, u8) -> void), lowered,
        fallback, replaces("velt_rt_write_f64");
    /// A whole number below 2^53 other than `-0`, as an `i64`, and the byte after it, as
    /// `number_write` writes the float, by digits of its own (no integer hook): `number_write`'s
    /// and numrep's.
    whole_number_write => lang_hooks_number_write, "wholeNumberWrite", sig((u32, i64, u8) -> void),
        lowered, fallback;
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

    // process_statics.velt: the process-wide statics the runtime reads (#2779), whose addresses
    // `velt_main` hands it first thing (`velt_rt_statics`).
    /// The epoch of the runtime's cache of the last position translation (`velt_rt`
    /// `str/recent.rs`): bumped by the runtime when a string buffer it may have remembered is
    /// freed or moved.
    str_epoch => lang_hooks_process_statics, "STR_EPOCH", process_static, lowered;

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
    /// The mark of a write of standard output on this thread, before each `console.log` (velt_vir
    /// `console.rs`): velt/sync's hand-offs publish the thread's buffered output only when it is
    /// set (std/sync/output.velt, #2771).
    stdout_written => lang_hooks_process, "stdoutWritten", sig(() -> void), lowered;

    // unwind.velt: the panic path in std (docs/internals/design/unwinding.md), in a program that
    // calls `catchUnwind`; its module is loaded on demand.
    /// Every panic (`velt_rt_panic`'s job when a catch is active): `(message)`. It raises Velt's
    /// exception when a `catchUnwind` is active on the thread, and calls `velt_rt_panic`
    /// otherwise.
    panic_raise => lang_hooks_unwind, "panicRaise", sig((&string) -> never), lowered, constructs(CatchUnwind);
    // sched.velt: the executor in Velt (#2649), with `VELT_SCHEDULER=std` ([`std_scheduler`]).
    /// `velt_rt_block_on`'s job: `(poll, state)`.
    sched_block_on => lang_hooks_sched, "schedBlockOn", sig((usize, usize, u64) -> void), lowered;
    /// `velt_rt_spawn`'s: `(poll, drop, state, size, align, result_size, result_drop)`, the handle.
    sched_spawn => lang_hooks_sched, "schedSpawn", sig((usize, usize, usize, u64, u64, u64, usize, usize) -> usize), lowered;
    /// `velt_rt_yield_now`'s: `(cx)`; then the end of `velt_main` with its exit code (0: the
    /// tasks left run).
    sched_yield => lang_hooks_sched, "schedYield", sig((usize) -> void), lowered;
    sched_finish => lang_hooks_sched, "schedFinish", sig((i32) -> void), lowered;
    /// `velt_rt_fut_box`'s: `(poll, drop, state, size, align)`, the lazy boxed promise.
    sched_box => lang_hooks_sched, "schedBox", sig((usize, usize, usize, u64, u64) -> usize), lowered;
    /// `velt_rt_fut_start`'s, `_start_rejecting`'s (`(f, result_drop, rejecting)`), `_detach`'s.
    sched_start => lang_hooks_sched, "schedStart", sig((usize, usize, bool) -> void), lowered;
    sched_detach => lang_hooks_sched, "schedDetach", sig((usize, usize) -> void), lowered;
    /// `velt_rt_futs_handled`'s: `(futs, n, quiet_drop)`.
    sched_handled => lang_hooks_sched, "schedHandled", sig((usize, u64, usize) -> void), lowered;
    /// `velt_rt_await_tick`'s: `(cx)`, the ticket; then `velt_rt_await_ready`'s: `(ticket, cx)`.
    sched_await_tick => lang_hooks_sched, "schedAwaitTick", sig((usize) -> u64), lowered;
    sched_await_ready => lang_hooks_sched, "schedAwaitReady", sig((u64, usize) -> u32), lowered;
    /// `velt_rt_microtask`'s (std/prelude/promise.velt's extern): the heap future.
    sched_microtask => lang_hooks_sched, "schedMicrotask", sig(() -> usize), lowered;
    /// `velt_rt_latch_new`'s, `_free`'s, `_open`'s and `_wait`'s (std/lang/promise/settle.velt's
    /// externs): a latch is its address; a wait is a heap future.
    sched_latch_new => lang_hooks_sched, "schedLatchNew", sig(() -> u64), lowered;
    sched_latch_free => lang_hooks_sched, "schedLatchFree", sig((u64) -> void), lowered;
    sched_latch_open => lang_hooks_sched, "schedLatchOpen", sig((u64) -> void), lowered;
    sched_latch_wait => lang_hooks_sched, "schedLatchWait", sig((u64) -> usize), lowered;
    /// The process lifetime's: `velt_rt_keep_alive_acquire`'s and `_release`'s (a ref'd timer),
    /// `velt_rt_timer_unref_due`'s (a heap future), `velt_rt_tla_at`'s (the top-level `await`).
    sched_keep_alive_acquire => lang_hooks_sched, "schedKeepAliveAcquire", sig(() -> void), lowered;
    sched_keep_alive_release => lang_hooks_sched, "schedKeepAliveRelease", sig(() -> void), lowered;
    sched_unref_due => lang_hooks_sched, "schedUnrefDue", sig(() -> usize), lowered;
    sched_tla_at => lang_hooks_sched, "schedTlaAt", sig((&string) -> void), lowered;
    /// `velt_rt_spawn_fut`'s: `(f, result_size, result_drop)`, the handle; then
    /// `velt_rt_spawn_target`'s (`(worker)`: the next spawn is pinned to that worker).
    sched_spawn_fut => lang_hooks_sched, "schedSpawnFut", sig((usize, u64, usize) -> usize), lowered;
    sched_pinned => lang_hooks_sched, "schedPinned", sig((i64) -> void), lowered;
    /// `velt_rt_sleep`'s: `(ms)`, a heap future; then the Node timer order's `velt_rt_timer_new`,
    /// `_turn`, `_again`, `_clear`, `_free`, then `velt_rt_uncaught_now`.
    sched_sleep => lang_hooks_sched, "schedSleep", sig((i64) -> usize), lowered;
    sched_timer_new => lang_hooks_sched, "schedTimerNew", sig((i64) -> u64), lowered;
    sched_timer_turn => lang_hooks_sched, "schedTimerTurn", sig((u64, u64) -> usize), lowered;
    sched_timer_again => lang_hooks_sched, "schedTimerAgain", sig((u64, i64) -> void), lowered;
    sched_timer_clear => lang_hooks_sched, "schedTimerClear", sig((u64) -> void), lowered;
    sched_timer_free => lang_hooks_sched, "schedTimerFree", sig((u64) -> void), lowered;
    sched_uncaught_now => lang_hooks_sched, "schedUncaughtNow", sig(() -> void), lowered;
    /// Module state's `velt_rt_module_state` (`(desc, slot)`), `_block` and `_ready` (workers).
    sched_module_state => lang_hooks_sched, "schedModuleState", sig((usize, u64) -> usize), lowered;
    sched_module_block => lang_hooks_sched, "schedModuleBlock", sig((usize) -> usize), lowered;
    sched_module_ready => lang_hooks_sched, "schedModuleReady", sig((usize, u64) -> void), lowered;

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
    /// `f.call(o)` of a class's method read through `any`, which reads the member again: a
    /// proxy `o` is not supported, whose `get` trap would run twice.
    proxy_call_check => lang_hooks_proxy, "proxyCallCheck", sig((&any) -> void);
    /// `{ ...x }` of an object in a program that makes proxies: a typed proxy is not supported.
    proxy_spread_check => lang_hooks_proxy, "proxySpreadCheck", sig((&any) -> void);
    /// `{ ...o }` of an `any` that may be a proxy: its enumerable own entries.
    spread_any_proxy => lang_hooks_proxy, "spreadAnyProxy", sig((&mut Record(string, any), move any) -> void);
    /// `rest_any` in a program that loads `Proxy`.
    rest_any_proxy => lang_hooks_proxy, "restAnyProxy", sig((move any, &any) -> any);

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
    ffi_ptr_address => ffi_ptr, "ptrAddress", sig(<A>(Ptr(A)) -> usize), lowered;
    ffi_ptr_address_or_zero => ffi_ptr, "ptrAddressOrZero", sig(<A>(or_null(Ptr(A))) -> usize), lowered;
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

    // lang/intrinsics/decorators.velt: the compile-time decorators the compiler acts on (#2203),
    // found by these roles, never by the name a use is written with.
    /// `@inline` on a std function or method: every call is inlined in release builds
    /// (`vir::Inline::Always`; `velt_sema` `decorators/known.rs`, `velt_vir` lowering).
    decorator_inline => lang_intrinsics_decorators, "inline", decorator;
    /// `@noInline`: calls stay calls (`vir::Inline::Never`).
    decorator_no_inline => lang_intrinsics_decorators, "noInline", decorator;
    /// `@intrinsic declare function`: the declaration of a compiler intrinsic
    /// (`velt_sema` `intrinsic_decls.rs`).
    decorator_intrinsic => lang_intrinsics_decorators, "intrinsic", decorator;
    // ffi/export.velt: `@export("c_name")`, a function C calls (#2203 step 5; `velt_sema`
    // `decorators/export.rs`, `velt_vir` lowering's `c_export.rs`).
    /// `@export("c_name")` on a top-level function: an unmangled symbol of that name with the C
    /// calling convention, whose parameters and result cross as a `declare c` function's.
    decorator_export => ffi_export, "export", decorator;
}

#[macro_use]
mod keys;
pub use keys::{Key, KEYS};
mod deleted;
pub use deleted::DELETED;
mod scheduler;
pub use scheduler::{std_scheduler, SCHEDULER_ENV};
mod lookup;
pub use lookup::{find, is_lowered};
mod on_demand;
pub use on_demand::{on_demand, ON_DEMAND_FLOAT, ON_DEMAND_MODULES, ON_DEMAND_WRITE};

#[cfg(test)]
#[path = "std_hooks_tests.rs"]
mod tests;
