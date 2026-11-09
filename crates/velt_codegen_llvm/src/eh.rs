//! `velt_eh_try` and its personality (`velt_vir::eh`, docs/internals/design/unwinding.md): the
//! module defines them when the program calls `velt_eh_try`, `linkonce_odr` so that units and
//! the std cache's objects each may and the link keeps one.
//!
//! On targets with the Itanium unwinder the call of `code(env)` is an `invoke` whose landing pad
//! catches everything the personality lets through (`catch ptr null`) and returns the exception
//! object. The personality claims only Velt's class: any other exception gets
//! `_URC_CONTINUE_UNWIND`, in the search phase too, so it passes as if the frame had no landing
//! pad ([`ITANIUM`]). The landing pad is out of line: the call that returns costs what a plain
//! call does.

use std::fmt::Write;

use velt_vir::eh::{self, Catching};
use velt_vir::vir;

use crate::module::FN_ATTRS;
use crate::target::Target;

/// `velt_eh_try` and its personality on targets with the Itanium unwinder. The personality claims
/// Velt's class in the search phase (`_URC_HANDLER_FOUND`, 6) and passes any other class in both
/// phases (`_URC_CONTINUE_UNWIND`, 8). In the cleanup phase it lands at the handler frame's landing
/// pad (`_URC_INSTALL_CONTEXT`, 7) with the exception in data register 0 and selector 0 in
/// register 1 (DWARF registers 0 and 1: rax/rdx, x0/x1). The landing pad is the first entry of the
/// LSDA's call-site table: `velt_eh_try` is `noinline` and its only call is the `invoke`, so LLVM
/// writes exactly one (lpstart omitted, call sites in ULEB128, as LLVM always writes them). Only
/// libunwind / libgcc_s functions are called: no runtime.
const ITANIUM: &str = r#"define linkonce_odr hidden ptr @TRY(ptr %code, ptr %env) noinline ATTRS personality ptr @PERSONALITY {
  invoke void %code(ptr %env) to label %ok unwind label %pad
ok:
  ret ptr null
pad:
  %lp = landingpad { ptr, i32 } catch ptr null
  %e = extractvalue { ptr, i32 } %lp, 0
  ret ptr %e
}
define linkonce_odr hidden i32 @PERSONALITY(i32 %v, i32 %a, i64 %c, ptr %e, ptr %x) ATTRS {
entry:
  %p = alloca ptr
  %ours = icmp eq i64 %c, CLASS
  br i1 %ours, label %mine, label %pass
pass:
  ret i32 8
mine:
  %search = and i32 %a, 1
  %is_search = icmp ne i32 %search, 0
  br i1 %is_search, label %found, label %cleanup
found:
  ret i32 6
cleanup:
  %frame = and i32 %a, 4
  %is_frame = icmp ne i32 %frame, 0
  br i1 %is_frame, label %land, label %pass
land:
  %lsda = call ptr @_Unwind_GetLanguageSpecificData(ptr %x)
  %ttype_at = getelementptr i8, ptr %lsda, i64 1
  %ttype = load i8, ptr %ttype_at
  %after = getelementptr i8, ptr %lsda, i64 2
  store ptr %after, ptr %p
  %has_types = icmp ne i8 %ttype, -1
  br i1 %has_types, label %types, label %sites
types:
  %types_offset = call i64 @velt_eh_uleb(ptr %p)
  br label %sites
sites:
  %q = load ptr, ptr %p
  %q1 = getelementptr i8, ptr %q, i64 1
  store ptr %q1, ptr %p
  %table = call i64 @velt_eh_uleb(ptr %p)
  %start = call i64 @velt_eh_uleb(ptr %p)
  %size = call i64 @velt_eh_uleb(ptr %p)
  %lp = call i64 @velt_eh_uleb(ptr %p)
  %base = call i64 @_Unwind_GetRegionStart(ptr %x)
  %ip = add i64 %base, %lp
  %ei = ptrtoint ptr %e to i64
  call void @_Unwind_SetGR(ptr %x, i32 0, i64 %ei)
  call void @_Unwind_SetGR(ptr %x, i32 1, i64 0)
  call void @_Unwind_SetIP(ptr %x, i64 %ip)
  ret i32 7
}
define linkonce_odr hidden i64 @velt_eh_uleb(ptr %pp) ATTRS {
entry:
  %p0 = load ptr, ptr %pp
  br label %loop
loop:
  %p = phi ptr [ %p0, %entry ], [ %pn, %loop ]
  %acc = phi i64 [ 0, %entry ], [ %accn, %loop ]
  %sh = phi i64 [ 0, %entry ], [ %shn, %loop ]
  %b = load i8, ptr %p
  %pn = getelementptr i8, ptr %p, i64 1
  %b64 = zext i8 %b to i64
  %low = and i64 %b64, 127
  %part = shl i64 %low, %sh
  %accn = or i64 %acc, %part
  %shn = add i64 %sh, 7
  %more = icmp ugt i8 %b, 127
  br i1 %more, label %loop, label %done
done:
  store ptr %pn, ptr %pp
  ret i64 %accn
}
declare ptr @_Unwind_GetLanguageSpecificData(ptr)
declare i64 @_Unwind_GetRegionStart(ptr)
declare void @_Unwind_SetGR(ptr, i32, i64)
declare void @_Unwind_SetIP(ptr, i64)
"#;

/// The definitions of `velt_eh_try` and its personality, if the program calls it.
pub(crate) fn definitions(program: &vir::Program, target: &Target) -> String {
    if !program.externs.iter().any(|e| eh::is_try(&e.symbol)) {
        return String::new();
    }
    let mut out = String::new();
    let try_fn = eh::TRY;
    match Catching::of_triple(&target.triple) {
        Catching::None => {
            let _ = writeln!(
                out,
                "define linkonce_odr hidden ptr @{try_fn}(ptr %code, ptr %env) {FN_ATTRS} {{\n\
                 \x20 call void %code(ptr %env)\n\
                 \x20 ret ptr null\n}}"
            );
        }
        Catching::Itanium => {
            let ir = ITANIUM
                .replace("TRY", eh::TRY)
                .replace("PERSONALITY", eh::PERSONALITY)
                .replace("CLASS", &eh::CLASS.to_string())
                .replace("ATTRS", FN_ATTRS);
            out.push_str(&ir);
        }
    }
    out
}
