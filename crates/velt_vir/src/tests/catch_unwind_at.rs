//! `catchUnwindAt(code, arg)` (hir `Intrinsic::CatchUnwindAt`, lower/attempt.rs): the call of
//! `velt_eh_try` with the function's address and the argument, and nothing in between (no
//! function value, closure environment or thunk). That a panic in the function is caught is
//! veltc's `unwinding` test (the interpreter has no unwinder).

use super::{lower_ok, std_src};
use crate::vir::{Callee, Terminator};

#[test]
fn catch_unwind_at_is_one_call_of_velt_eh_try_with_the_address_and_the_argument() {
    let p = std_src::check_std_src(
        "catch_unwind_at_test",
        "import { fnAddress } from \"velt/lang/intrinsics\";
         import { catchUnwindAt } from \"velt/lang/intrinsics/unwind\";
         function atAddress(a: usize): void {
           if (a === 7) {
             panic(\"at 7\");
           }
         }
         function main() {
           // SAFETY: `atAddress` takes one number; the exception, if any, is only printed.
           const e = unsafe { catchUnwindAt(fnAddress(atAddress), 5) };
           console.log(e);
         }",
    );
    let v = lower_ok(&p);
    let main = (v.funcs.iter())
        .find(|f| f.symbol == "_V4main")
        .unwrap_or_else(|| panic!("no main in:\n{v}"));
    let at = (v.funcs.iter())
        .position(|f| f.symbol.contains("atAddress"))
        .unwrap_or_else(|| panic!("no atAddress in:\n{v}"));
    let tries: Vec<_> = (main.blocks.iter())
        .filter_map(|b| match &b.term {
            Terminator::Call {
                callee: Callee::Extern(id),
                args,
                ..
            } if v.externs[id.0 as usize].symbol == crate::eh::TRY => Some(args),
            _ => None,
        })
        .collect();
    assert_eq!(tries.len(), 1, "{v}");
    assert_eq!(tries[0].len(), 2, "{v}");
    // Each operand, followed back through the casts that make it a pointer, is what it names:
    // the code operand `atAddress`'s address (`fnAddress`, taken in `main` itself: no thunk stands
    // between `velt_eh_try` and `atAddress`), the argument the literal (no environment).
    let stmts: Vec<String> = (main.blocks.iter())
        .flat_map(|b| &b.stmts)
        .map(|s| s.to_string())
        .collect();
    let source = |op: &crate::vir::Operand| {
        let mut x = op.to_string();
        while let Some(def) = stmts
            .iter()
            .find_map(|s| s.strip_prefix(&format!("{x} = ")))
        {
            let Some(cast) = def.strip_prefix("cast ") else {
                break;
            };
            x = cast.split(" as ").next().unwrap_or(cast).to_string();
        }
        x
    };
    assert_eq!(
        (source(&tries[0][0]), source(&tries[0][1])),
        (format!("fn#{at}"), "5_u64".to_string()),
        "{v}"
    );
}
