//! `declare system function` (docs/reference/system-functions.md): every declaration, in std or
//! any package or program, must bind a function of the compiler's table of system functions
//! (`velt_vir::system`) with the Velt types the table states for it. A function the target
//! lacks but another target has is accepted here; calling it is an error from lowering.

use velt_common::{Diagnostic, Diagnostics};
use velt_sema::hir;
use velt_vir::system::{self, Mismatch};

/// Check the system function declaration `f` against the table of `target`.
pub(super) fn check(p: &hir::Program, f: &hir::ExternFnDef, target: &str, diags: &mut Diagnostics) {
    let name = &f.symbol;
    match system::match_decl(p, f, target) {
        Ok(_) => {}
        Err(Mismatch::Unknown { close }) => {
            let mut d = Diagnostic::error(
                format!("`{name}` is not a system function"),
                f.span,
            )
            .with_note(
                "`declare system function` binds a function of the platform's C library that \
                 the compiler knows how to call safely (system-functions.md lists them)",
            );
            if let Some(c) = close {
                d = d.with_note(format!("did you mean `{c}`?"));
            }
            diags.push(d);
        }
        Err(Mismatch::Form(why)) => diags.push(Diagnostic::error(why, f.span)),
        Err(Mismatch::Signature {
            reasons,
            expected,
            prototypes,
        }) => {
            let mut d = Diagnostic::error(
                format!(
                    "`declare system function {name}` does not match the C function `{}`",
                    system::base_name(name)
                ),
                f.span,
            );
            for r in reasons {
                d = d.with_note(r);
            }
            for (c, v) in prototypes.iter().zip(&expected) {
                d = d.with_note(format!("C: `{c}`; Velt: `{v}`"));
            }
            diags.push(d);
        }
    }
}

/// A note for a plain `declare function` of a name in the table: the form that binds it.
pub(super) fn hint(f: &hir::ExternFnDef) -> Option<String> {
    let known = system::all_names().contains(&system::base_name(&f.symbol));
    known.then(|| {
        format!(
            "`{}` is a system function: `declare system function {}(...)` binds the C library's",
            f.symbol, f.symbol
        )
    })
}
