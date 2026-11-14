//! `velt_eh_try` and its personality (`velt_vir::eh`, docs/internals/design/unwinding.md): the
//! module defines them when the program calls `velt_eh_try`, `linkonce_odr` so that units and
//! the std cache's objects each may and the link keeps one.
//!
//! On targets with the Itanium unwinder the call of `code(env)` is an `invoke` whose landing pad
//! catches everything the personality lets through (`catch ptr null`) and returns the exception
//! object. The personality claims only Velt's class: any other exception gets
//! `_URC_CONTINUE_UNWIND`, in the search phase too, so it passes as if the frame had no landing
//! pad ([`ITANIUM_PERSONALITY`]), which cleanup pads name too. The landing pad is out of line: the call that returns costs what a plain
//! call does.

use std::fmt::Write;

use velt_vir::eh::{self, Catching};
use velt_vir::vir;

use crate::module::FN_ATTRS;
use crate::target::Target;

/// `velt_eh_try` on targets with the Itanium unwinder: `code(env)` as an `invoke` whose landing
/// pad catches what the personality lets through (`catch ptr null`) and returns the exception.
const ITANIUM_TRY: &str = r#"define linkonce_odr hidden ptr @TRY(ptr %code, ptr %env) noinline ATTRS personality ptr @PERSONALITY {
  invoke void %code(ptr %env) to label %ok unwind label %pad
ok:
  ret ptr null
pad:
  %lp = landingpad { ptr, i32 } catch ptr null
  %e = extractvalue { ptr, i32 } %lp, 0
  ret ptr %e
}
"#;

/// The personality of every frame with a landing pad: `velt_eh_try`'s (a catch) and the cleanup
/// pads of frames that own values (design "Cleanup in unwound frames"). Any class but Velt's
/// passes in both phases (`_URC_CONTINUE_UNWIND`, 8). For Velt's it finds the LSDA's call-site
/// entry covering `ip - 1` (`velt_eh_landing`: landing-pad base omitted, the type table skipped,
/// call sites in ULEB128, as LLVM writes them); none, or landing pad 0, passes too. An entry with
/// action 0 is a cleanup: the search phase passes it, the cleanup phase lands there. An entry
/// with an action is a catch: the search phase claims it (`_URC_HANDLER_FOUND`, 6), the cleanup
/// phase lands in the handler frame only. Landing (`_URC_INSTALL_CONTEXT`, 7) puts the exception
/// in data register 0 and selector 0 in register 1 (DWARF registers 0 and 1: rax/rdx, x0/x1).
/// Only libunwind / libgcc_s functions are called: no runtime.
const ITANIUM_PERSONALITY: &str = r#"define linkonce_odr hidden i32 @PERSONALITY(i32 %v, i32 %a, i64 %c, ptr %e, ptr %x) ATTRS {
entry:
  %actp = alloca i64
  %ours = icmp eq i64 %c, CLASS
  br i1 %ours, label %mine, label %pass
pass:
  ret i32 8
mine:
  %lp = call i64 @velt_eh_landing(ptr %x, ptr %actp)
  %known = icmp ne i64 %lp, 0
  br i1 %known, label %phase, label %pass
phase:
  %act = load i64, ptr %actp
  %cleanup_only = icmp eq i64 %act, 0
  %search = and i32 %a, 1
  %is_search = icmp ne i32 %search, 0
  br i1 %is_search, label %searching, label %cleaning
searching:
  br i1 %cleanup_only, label %pass, label %found
found:
  ret i32 6
cleaning:
  br i1 %cleanup_only, label %land, label %handler
handler:
  %frame = and i32 %a, 4
  %is_frame = icmp ne i32 %frame, 0
  br i1 %is_frame, label %land, label %pass
land:
  %base = call i64 @_Unwind_GetRegionStart(ptr %x)
  %ip = add i64 %base, %lp
  %ei = ptrtoint ptr %e to i64
  call void @_Unwind_SetGR(ptr %x, i32 0, i64 %ei)
  call void @_Unwind_SetGR(ptr %x, i32 1, i64 0)
  call void @_Unwind_SetIP(ptr %x, i64 %ip)
  ret i32 7
}
define linkonce_odr hidden i64 @velt_eh_landing(ptr %x, ptr %actp) ATTRS {
entry:
  %p = alloca ptr
  %lsda = call ptr @_Unwind_GetLanguageSpecificData(ptr %x)
  %none = icmp eq ptr %lsda, null
  br i1 %none, label %unknown, label %header
unknown:
  ret i64 0
header:
  %lpstart = load i8, ptr %lsda
  %omitted = icmp eq i8 %lpstart, -1
  br i1 %omitted, label %ttype, label %unknown
ttype:
  %ttype_at = getelementptr i8, ptr %lsda, i64 1
  %tt = load i8, ptr %ttype_at
  %after = getelementptr i8, ptr %lsda, i64 2
  store ptr %after, ptr %p
  %has_types = icmp ne i8 %tt, -1
  br i1 %has_types, label %types, label %sites
types:
  %types_offset = call i64 @velt_eh_uleb(ptr %p)
  br label %sites
sites:
  %q = load ptr, ptr %p
  %enc = load i8, ptr %q
  %q1 = getelementptr i8, ptr %q, i64 1
  store ptr %q1, ptr %p
  %uleb = icmp eq i8 %enc, 1
  br i1 %uleb, label %table, label %unknown
table:
  %len = call i64 @velt_eh_uleb(ptr %p)
  %t0 = load ptr, ptr %p
  %end = getelementptr i8, ptr %t0, i64 %len
  %ipa = call i64 @_Unwind_GetIP(ptr %x)
  %region = call i64 @_Unwind_GetRegionStart(ptr %x)
  %ipr = sub i64 %ipa, %region
  %ip = sub i64 %ipr, 1
  br label %loop
loop:
  %cur = load ptr, ptr %p
  %more = icmp ult ptr %cur, %end
  br i1 %more, label %entry1, label %unknown
entry1:
  %start = call i64 @velt_eh_uleb(ptr %p)
  %size = call i64 @velt_eh_uleb(ptr %p)
  %pad = call i64 @velt_eh_uleb(ptr %p)
  %action = call i64 @velt_eh_uleb(ptr %p)
  %before = icmp ult i64 %ip, %start
  br i1 %before, label %unknown, label %upper
upper:
  %stop = add i64 %start, %size
  %inside = icmp ult i64 %ip, %stop
  br i1 %inside, label %hit, label %loop
hit:
  store i64 %action, ptr %actp
  ret i64 %pad
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
declare i64 @_Unwind_GetIP(ptr)
declare void @_Unwind_SetGR(ptr, i32, i64)
declare void @_Unwind_SetIP(ptr, i64)
"#;

/// `velt_eh_try` on Windows x64: `code(env)` as an `invoke` under `__C_specific_handler`, whose
/// filter (`FILTER`) takes Velt's exception code only and leaves the exception object in `%slot`.
/// The filter gets `EXCEPTION_POINTERS` (its first word: the `EXCEPTION_RECORD`, whose code is at
/// 0 and first parameter at 32) and the frame of `velt_eh_try`.
const SEH_TRY: &str = r#"define internal ptr @TRY(ptr %code, ptr %env) noinline ATTRS personality ptr @__C_specific_handler {
entry:
  %slot = alloca ptr, align 8
  call void (...) @llvm.localescape(ptr %slot)
  invoke void %code(ptr %env) to label %ok unwind label %dispatch
ok:
  ret ptr null
dispatch:
  %cs = catchswitch within none [label %handler] unwind to caller
handler:
  %cp = catchpad within %cs [ptr @FILTER]
  catchret from %cp to label %caught
caught:
  %e = load volatile ptr, ptr %slot
  ret ptr %e
}
define internal i32 @FILTER(ptr %info, ptr %frame) ATTRS {
entry:
  %fp = call ptr @llvm.eh.recoverfp(ptr @TRY, ptr %frame)
  %slot = call ptr @llvm.localrecover(ptr @TRY, ptr %fp, i32 0)
  %rec = load ptr, ptr %info
  %code = load i32, ptr %rec
  %ours = icmp eq i32 %code, SEHCODE
  br i1 %ours, label %take, label %pass
take:
  %arg = getelementptr i8, ptr %rec, i64 32
  %e = load ptr, ptr %arg
  store volatile ptr %e, ptr %slot
  ret i32 1
pass:
  ret i32 0
}
declare i32 @__C_specific_handler(...)
declare void @llvm.localescape(...)
declare ptr @llvm.localrecover(ptr, ptr, i32)
declare ptr @llvm.eh.recoverfp(ptr, ptr)
"#;

/// The definitions of `velt_eh_try` and its personality, and of `velt_eh_catches`, for those the
/// program calls.
pub(crate) fn definitions(program: &vir::Program, target: &Target) -> String {
    let calls = |f: fn(&str) -> bool| program.externs.iter().any(|e| f(&e.symbol));
    let catching = Catching::of_triple(&target.triple);
    let mut out = String::new();
    if calls(eh::is_catches) {
        let _ = writeln!(
            out,
            "define linkonce_odr hidden zeroext i8 @{}() {FN_ATTRS} {{\n  ret i8 {}\n}}",
            eh::CATCHES,
            u8::from(catching.catches())
        );
    }
    let catches = catching == Catching::Itanium;
    // The personality, where a frame names it: `velt_eh_try`'s, or a cleanup pad's (a program
    // with pads declares `velt_eh_resume`).
    if catches && (calls(eh::is_try) || calls(eh::is_resume)) {
        out.push_str(&fill(ITANIUM_PERSONALITY));
    }
    if !calls(eh::is_try) {
        return out;
    }
    let try_fn = eh::TRY;
    if catches {
        out.push_str(&fill(ITANIUM_TRY));
    } else if catching == Catching::Seh {
        out.push_str(&fill(SEH_TRY));
    } else {
        let _ = writeln!(
            out,
            "define linkonce_odr hidden ptr @{try_fn}(ptr %code, ptr %env) {FN_ATTRS} {{\n\
             \x20 call void %code(ptr %env)\n\
             \x20 ret ptr null\n}}"
        );
    }
    out
}

/// `ir` with its placeholders filled in.
fn fill(ir: &str) -> String {
    ir.replace("TRY", eh::TRY)
        .replace("PERSONALITY", eh::PERSONALITY)
        .replace("CLASS", &eh::CLASS.to_string())
        .replace("SEHCODE", &(eh::SEH_CODE as i32).to_string())
        .replace("FILTER", "velt_eh_filter")
        .replace("ATTRS", FN_ATTRS)
}
