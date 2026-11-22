//! `console.log`'s line breaking's rows of the hook table (std/lang/hooks/inspect_layout.velt and
//! inspect_layout_text.velt, #3508; docs/internals/design/inspect-shapes.md "Line breaking"). Kept
//! apart from the rest of the table ([`super`]), which [`with_inspect_layout_hooks`] gives to
//! `with_weak_hooks!` with these rows after it.

/// `with_weak_hooks!` over the rows given, then the line breaking's.
macro_rules! with_inspect_layout_hooks {
    ($($rows:tt)*) => {
        with_weak_hooks! {
            $($rows)*
            // Checked where live code prints a value that isn't scalar at the top level (velt_sema
            // `live_constructs.rs`); lowering leaves a value on one line where they are unchecked
            // (only in programs without std).
            /// After a top-level value written on one line from byte `start`: broken across lines as
            /// Node's `util.inspect` breaks it.
            inspect_layout => lang_hooks_inspect_layout, "inspectLayout", sig((&mut string, u64) -> void),
                lowered, fallback, constructs(Format, Dynamic), replaces("velt_rt_strbuf_inspect_layout");
            /// The value at bytes `start..` of the builder laid out, "" when it stays as it is (called by
            /// role from `inspectLayout`).
            inspect_layout_text => lang_hooks_inspect_layout_text, "inspectLayoutText",
                sig((&string, u64) -> string), prebuilt;
            /// The text from byte `start` to the end of the builder is what a custom inspect returned:
            /// the layout keeps it as one piece.
            inspect_atom => lang_hooks_inspect_layout_text, "inspectAtom", sig((&string, u64) -> void),
                lowered, fallback, constructs(Format, Dynamic), replaces("velt_rt_strbuf_inspect_atom"),
                prebuilt;
            /// The start of a top-level value: the pieces a value before it left are dropped.
            inspect_atoms_begin => lang_hooks_inspect_layout_text, "inspectAtomsBegin", sig(() -> void),
                lowered, fallback, constructs(Format, Dynamic), replaces("velt_rt_strbuf_inspect_begin"),
                prebuilt;
            /// Before a user's custom inspect: the pieces so far, saved for `inspect_atoms_restore`.
            inspect_atoms_save => lang_hooks_inspect_layout_text, "inspectAtomsSave", sig(() -> u64),
                lowered, fallback, constructs(Format, Dynamic), prebuilt;
            /// After a user's custom inspect: the pieces `inspect_atoms_save` saved, back.
            inspect_atoms_restore => lang_hooks_inspect_layout_text, "inspectAtomsRestore",
                sig((u64) -> void), lowered, fallback, constructs(Format, Dynamic), prebuilt;
        }
    };
}
