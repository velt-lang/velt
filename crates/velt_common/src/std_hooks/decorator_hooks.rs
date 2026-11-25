//! The decorators' rows of the hook table: the compile-time decorators the compiler acts on
//! (#2203), found by these roles, never by the name a use is written with. Kept apart from the
//! rest of the table ([`super`]), which [`with_decorator_hooks`] gives on, with these rows after
//! it, to `with_module_hooks!`.

/// `with_module_hooks!` over the rows given, then the decorators'.
macro_rules! with_decorator_hooks {
    ($($rows:tt)*) => {
        with_module_hooks! {
            $($rows)*
            // lang/intrinsics/decorators.velt.
            /// `@inline` on a function or method (any module; velt/compiler re-exports it):
            /// `@inline("always")` inlines every call in release builds (`vir::Inline::Always`), the
            /// default `"hint"` is `vir::Inline::Default` while a program is one unit (`velt_sema`
            /// `decorators/known.rs`, `velt_vir` lowering).
            decorator_inline => lang_intrinsics_decorators, "inline", decorator;
            /// `@noInline`: calls stay calls (`vir::Inline::Never`).
            decorator_no_inline => lang_intrinsics_decorators, "noInline", decorator;
            /// `@cold`: its calls are rare, though they return (`vir::Function::rare`).
            decorator_cold => lang_intrinsics_decorators, "cold", decorator;
            /// `@variant("plain", ...when)` on a function (any module; velt/compiler re-exports
            /// it): a call whose arguments pass every condition calls `plain` instead (`velt_sema`
            /// `decorators::known`, `body::expr::call`; docs/internals/design/call-variants.md).
            decorator_variant => lang_intrinsics_decorators, "variant", decorator;
            /// `@intrinsic declare function`: the declaration of a compiler intrinsic
            /// (`velt_sema` `intrinsic_decls.rs`).
            decorator_intrinsic => lang_intrinsics_decorators, "intrinsic", decorator;
            /// `@targetFeature("sha-arm")` on a function (any module; velt/compiler re-exports it): the
            /// CPU extension it is compiled for (`velt_sema` `body/gated.rs`, `hir::Program::cpu_targets`).
            decorator_target_feature => lang_intrinsics_decorators, "targetFeature", decorator;
            /// `@repr("transparent")` on a struct (any module; velt/compiler re-exports it): passed as
            /// its one field, to C too (`velt_sema` `decorators/layout.rs`, `velt_vir` `c_call.rs`).
            decorator_repr => lang_intrinsics_decorators, "repr", decorator;
            /// `@align(n)` on a struct: its values aligned to `n`, its size a multiple of it
            /// (`velt_vir` `layout.rs`).
            decorator_align => lang_intrinsics_decorators, "align", decorator;
            /// `@modifies("w")` on std's `declare generated` functions: the parameters every
            /// instance modifies, given their final mode where the function is declared
            /// (`velt_sema` `decorators/modifies.rs`; #3543).
            decorator_modifies => lang_intrinsics_decorators, "modifies", decorator;
            // ffi/export.velt: `@export("c_name")`, a function C calls (#2203 step 5; `velt_sema`
            // `decorators/export.rs`, `velt_vir` lowering's `c_export.rs`).
            /// `@export("c_name")` on a top-level function: an unmangled symbol of that name with the C
            /// calling convention, whose parameters and result cross as a `declare c` function's.
            decorator_export => ffi_export, "export", decorator;
        }
    };
}
