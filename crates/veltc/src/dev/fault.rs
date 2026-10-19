//! Windows x64: a JIT host that crashes with an access violation says where, on stderr, before
//! it dies: the faulting instruction, what it touched, and the stack (module + offset per frame;
//! JIT code is not in any module). A crash in JIT code is otherwise only an exit code.
//!
//! A vectored handler sees the fault first, before frame-based handlers, so the report also
//! comes when unwinding through the faulting frame would not work. It reports and passes the
//! fault on: what happens next is unchanged.

use std::ffi::c_void;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, HMODULE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, RtlLookupFunctionEntry, RtlVirtualUnwind, CONTEXT,
    EXCEPTION_POINTERS, UNW_FLAG_NHANDLER,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows_sys::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
/// Frames to walk at most.
const MAX_FRAMES: usize = 64;

/// Register the handler (once).
pub fn install() {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::AcqRel) {
        return;
    }
    // SAFETY: registers a handler that reads the exception record and context, and copies the
    // context before walking the stack.
    unsafe { AddVectoredExceptionHandler(0, Some(on_fault)) };
}

unsafe extern "system" fn on_fault(info: *mut EXCEPTION_POINTERS) -> i32 {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    let record = &*(*info).ExceptionRecord;
    if record.ExceptionCode != EXCEPTION_ACCESS_VIOLATION || REPORTED.swap(true, Ordering::AcqRel) {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    let context = *(*info).ContextRecord;
    let access = match record.ExceptionInformation[0] {
        0 => "reading",
        1 => "writing",
        8 => "executing",
        _ => "accessing",
    };
    let mut out = String::new();
    let thread = std::thread::current();
    let _ = writeln!(
        out,
        "velt dev: access violation {access} {:#x} at {:#x} in {} (thread {} `{}`)",
        record.ExceptionInformation[1],
        context.Rip,
        place(context.Rip),
        GetCurrentThreadId(),
        thread.name().unwrap_or("?"),
    );
    let c = &context;
    let _ = writeln!(
        out,
        "  rax {:#x} rbx {:#x} rcx {:#x} rdx {:#x} rsi {:#x} rdi {:#x}\n  rbp {:#x} rsp {:#x} r8 {:#x} r9 {:#x} r10 {:#x} r11 {:#x}\n  r12 {:#x} r13 {:#x} r14 {:#x} r15 {:#x}",
        c.Rax, c.Rbx, c.Rcx, c.Rdx, c.Rsi, c.Rdi, c.Rbp, c.Rsp, c.R8, c.R9, c.R10, c.R11, c.R12,
        c.R13, c.R14, c.R15
    );
    let _ = writeln!(
        out,
        "  touched: {}",
        region(record.ExceptionInformation[1] as u64)
    );
    let _ = writeln!(out, "  code: {}", region(context.Rip));
    let _ = writeln!(out, "  stack:");
    walk(context, &mut out);
    eprint!("{out}");
    eprintln!("{}", std::backtrace::Backtrace::force_capture());
    EXCEPTION_CONTINUE_SEARCH
}

/// Walk the stack from `context` with the system's unwind tables (the JIT registers its own).
unsafe fn walk(mut context: CONTEXT, out: &mut String) {
    for frame in 0..MAX_FRAMES {
        let pc = context.Rip;
        // A call through a null pointer faults at 0: its caller's return address is on top.
        if pc == 0 && frame > 0 {
            break;
        }
        let _ = writeln!(out, "    {pc:#x} {}", place(pc));
        let mut base = 0u64;
        let entry = RtlLookupFunctionEntry(pc, &mut base, std::ptr::null_mut());
        if entry.is_null() {
            // A leaf (or a trampoline): the return address is on top of the stack.
            if !readable(context.Rsp) {
                break;
            }
            context.Rip = *(context.Rsp as *const u64);
            context.Rsp += 8;
        } else {
            let mut data: *mut c_void = std::ptr::null_mut();
            let mut frame = 0u64;
            RtlVirtualUnwind(
                UNW_FLAG_NHANDLER,
                base,
                pc,
                entry,
                &mut context,
                &mut data,
                &mut frame,
                std::ptr::null_mut(),
            );
        }
    }
}

/// `module+offset`, or where non-module memory came from.
unsafe fn place(address: u64) -> String {
    let mut module: HMODULE = std::ptr::null_mut();
    let flags =
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
    if GetModuleHandleExW(flags, address as *const u16, &mut module) != 0 {
        let mut name = [0u16; 260];
        let n = GetModuleFileNameW(module, name.as_mut_ptr(), name.len() as u32) as usize;
        let path = String::from_utf16_lossy(&name[..n]);
        let file = path.rsplit('\\').next().unwrap_or(&path).to_string();
        return format!("{file}+{:#x}", address - module as u64);
    }
    let mut base = 0u64;
    let entry = RtlLookupFunctionEntry(address, &mut base, std::ptr::null_mut());
    if entry.is_null() {
        "no module (JIT code without unwind info: trampoline?)".into()
    } else {
        let begin = base + (*entry).BeginAddress as u64;
        format!("JIT code (function at {begin:#x}+{:#x})", address - begin)
    }
}

/// What kind of memory `address` is in.
unsafe fn region(address: u64) -> String {
    let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
    let size = std::mem::size_of::<MEMORY_BASIC_INFORMATION>();
    if VirtualQuery(address as *const c_void, &mut info, size) == 0 {
        return format!("{address:#x}: no region");
    }
    format!(
        "{address:#x}: region {:#x}+{:#x} of allocation {:#x}, state {:#x}, protect {:#x} (allocated {:#x}), type {:#x}",
        info.BaseAddress as u64,
        info.RegionSize,
        info.AllocationBase as u64,
        info.State,
        info.Protect,
        info.AllocationProtect,
        info.Type
    )
}

/// Whether 8 bytes at `address` can be read.
unsafe fn readable(address: u64) -> bool {
    let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
    let size = std::mem::size_of::<MEMORY_BASIC_INFORMATION>();
    VirtualQuery(address as *const c_void, &mut info, size) != 0
        && info.State == 0x1000
        && info.Protect & 0xEE != 0
}
