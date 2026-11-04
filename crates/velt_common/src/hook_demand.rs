//! Which hooks lowering may call, decided by sema (#2161 step 2): the HIR constructs whose
//! lowering calls a hook ([`Construct`], listed per entry in `std_hooks.rs`'s `constructs(…)`),
//! and the set of hooks sema demanded ([`HookSet`], `hir::Program::hooks_demanded`).
//! CONTRACT FILE — changes need maintainer review (docs/internals/contracts/README.md).
//!
//! Sema (`velt_sema` `body/lazy_std/live.rs`) evaluates the constructs on *live* code, a superset
//! of the functions lowering builds (user code, and the std definitions that code reaches, that
//! lowering looks up by name, that are methods of a type in live code, or that lowering makes
//! values of), with the operand's type, and demands each hook one of whose constructs occurs; a
//! hook whose entry lists no construct is always demanded. It checks the
//! demanded hooks, and leaves the others unchecked. Lowering's builder of a hook with constructs
//! (`velt_vir` `lower/hook_calls.rs`) asserts, in release compilers too, that its entry lists the
//! construct and that sema demanded it:
//! `ICE: hook <name> reached by lowering but not demanded by sema (construct <c>)` names the
//! construct missing from the table.
//!
//! **Why a hook may have a narrow demand.** A construct in a function lowering builds is in a
//! function sema walked. A debug compiler, or `VELT_LIVE_CHECK=1`, has lowering assert that each
//! function it builds was live (`hir::Program::live`), and the goldens run such a compiler; the
//! exception a release compiler doesn't walk is in `live.rs`'s `live_defs`. Glue is lowered per
//! type, not per function: its call site's construct must be listed in the table and found on
//! the type, which the builder's assertion above checks. Today's table doesn't exercise this:
//! sema takes one construct present as all of them, so a program demands none of the integer
//! hooks or all of them (`velt_sema`'s `live_hooks` test). `console.log`'s number writers
//! (`int_write`, `uint_write`, `number_write`) list `Format` alone; a float printed alone sets
//! `Format` too.
//!
//! ```
//! use velt_common::std_hooks::{hook_index, Construct, HookSet, HOOKS};
//! let int_push = &HOOKS[hook_index::int_push];
//! assert!(int_push.constructs.contains(&Construct::Format));
//! let mut set = HookSet::default();
//! set.insert(hook_index::int_push);
//! assert!(set.contains(hook_index::int_push) && !set.contains(hook_index::int_text));
//! ```

/// A HIR construct whose lowering calls a hook, as sema evaluates it on live code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Construct {
    /// `ToString` / `TemplateToString` (`String(v)`, `${v}`, `v.toString()`, `"a" + v`) of a
    /// signed integer: its text alone (`template.rs` `stringify`), or appended to the
    /// template's builder.
    TextInt,
    /// The same of an unsigned integer.
    TextUint,
    /// Lowering's formatting glue for a value of a type that may print an integer (an integer, an
    /// enum's discriminant, an array's or a map's size, a class's fields, a value of a type
    /// parameter or held as `any`): `console.log` and `console.error` of it, `inspect`,
    /// `errorText`, `String()` and `${}` of a value that is no scalar, the report of it thrown
    /// and not caught, its conversion to an interface or `any`, and the `format` slot of an
    /// interface value's table. A class's table has that slot only where this is present. Also
    /// `console.log` of a float, whose writer's code holds the integer's.
    Format,
    /// A bounds-checked index of an array (`xs[i]`, an array pattern, the array intrinsics): the
    /// out-of-bounds message prints the length and the index.
    IndexCheck,
    /// A value held as `any` (`unknown`, `object`, `Function`): its table's glue writes numeric
    /// keys (`Object.keys`, `k in x`) and formats what it holds.
    Dynamic,
    /// A call of `catchUnwind` (std only, docs/internals/design/unwinding.md): every panic of the
    /// program goes through std's panic path, which raises Velt's exception when a catch is
    /// active on the thread.
    CatchUnwind,
}

impl Construct {
    /// Every construct, in bit order ([`ConstructSet`]).
    pub const ALL: [Construct; 6] = [
        Construct::TextInt,
        Construct::TextUint,
        Construct::Format,
        Construct::IndexCheck,
        Construct::Dynamic,
        Construct::CatchUnwind,
    ];

    /// The construct as the ICE names it.
    pub fn name(self) -> &'static str {
        match self {
            Construct::TextInt => "TextInt (ToString or TemplateToString of a signed integer)",
            Construct::TextUint => "TextUint (ToString or TemplateToString of an unsigned integer)",
            Construct::Format => "Format (formatting glue of a type that may print an integer)",
            Construct::IndexCheck => "IndexCheck (a bounds-checked array index)",
            Construct::Dynamic => "Dynamic (the glue of a value held as any)",
            Construct::CatchUnwind => "CatchUnwind (a call of catchUnwind)",
        }
    }

    /// Its bit in a [`ConstructSet`].
    pub const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A set of [`Construct`]s, one bit each ([`Construct::bit`]).
pub type ConstructSet = u8;

/// The words of a [`HookSet`]: one bit per entry of [`crate::std_hooks::HOOKS`].
const WORDS: usize = crate::std_hooks::HOOKS.len().div_ceil(64);

/// A set of hooks by their index in [`crate::std_hooks::HOOKS`] (`std_hooks::hook_index::int_push`):
/// the hooks sema demanded (`hir::Program::hooks_demanded`). Testing one is one bit test.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookSet([u64; WORDS]);

impl HookSet {
    /// Every hook: what a program checked with every std body may call.
    pub fn all() -> HookSet {
        let mut s = HookSet::default();
        for i in 0..crate::std_hooks::HOOKS.len() {
            s.insert(i);
        }
        s
    }

    /// The hooks lowering may call in a program whose live code has the constructs `present`:
    /// those listing one of them, and those listing none.
    pub fn demanded_by(present: ConstructSet) -> HookSet {
        let mut s = HookSet::default();
        for (i, h) in crate::std_hooks::HOOKS.iter().enumerate() {
            if h.constructs.is_empty() || h.constructs.iter().any(|c| present & c.bit() != 0) {
                s.insert(i);
            }
        }
        s
    }

    #[inline]
    pub fn insert(&mut self, i: usize) {
        self.0[i / 64] |= 1 << (i % 64);
    }

    #[inline]
    pub fn contains(&self, i: usize) -> bool {
        self.0[i / 64] & (1 << (i % 64)) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::std_hooks::{hook_index, HOOKS};

    #[test]
    fn the_index_is_the_table_order() {
        assert_eq!(HOOKS[hook_index::int_text].name, "int_text");
        assert_eq!(HOOKS[hook_index::uint_push].name, "uint_push");
        assert_eq!(HOOKS[hook_index::array_filled].name, "array_filled");
    }

    #[test]
    fn a_hook_without_constructs_is_always_demanded() {
        let none = HookSet::demanded_by(0);
        for (i, h) in HOOKS.iter().enumerate() {
            assert_eq!(none.contains(i), h.constructs.is_empty(), "{}", h.name);
        }
        assert_eq!(
            HookSet::demanded_by(Construct::ALL.iter().fold(0, |s, c| s | c.bit())),
            HookSet::all()
        );
    }

    #[test]
    fn the_integer_hooks_are_demanded_by_their_constructs() {
        use crate::std_hooks::hook_index::{int_push, int_text, int_write, number_write};
        use crate::std_hooks::hook_index::{uint_push, uint_text, uint_write};
        const WRITE: [usize; 3] = [int_write, uint_write, number_write];
        // Every entry of the table, and that nothing else demands these hooks.
        for (c, hooks) in [
            (Construct::TextInt, &[int_text, int_push][..]),
            (Construct::TextUint, &[uint_text, uint_push]),
            (
                Construct::Format,
                &[int_push, uint_push, WRITE[0], WRITE[1], WRITE[2]],
            ),
            (Construct::IndexCheck, &[int_text, uint_text]),
            (Construct::Dynamic, &[uint_text, int_push, uint_push]),
        ] {
            let by = HookSet::demanded_by(c.bit());
            for hook in [int_text, uint_text, int_push, uint_push]
                .into_iter()
                .chain(WRITE)
            {
                assert_eq!(by.contains(hook), hooks.contains(&hook), "{}", c.name());
            }
        }
    }
}
