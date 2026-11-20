//! `velt_simd_cpuid` and `velt_simd_xgetbv` on x86-64 (`velt_vir::simd::SimdCall`): Cranelift has
//! no instruction for either, so the module defines each as a local function of fixed machine code
//! (as `velt_eh_try` on Windows, eh_seh.rs), which std's start-up check (std/lang/cpu/detect_x86.velt)
//! calls. Debug builds use `--cpu`'s features too, so they need the check. Only SSE2 instructions,
//! which every x86-64 CPU has: the code runs before anything knows what else the CPU has.

use cranelift_codegen::isa::CallConv;
use cranelift_module::{FuncId, Linkage, Module};

use crate::CodegenResult;

/// `cpuid` with EAX = the first argument and ECX = the second, as EAX, EBX, ECX, EDX in the lanes
/// of the vector returned in XMM0. RBX, which both calling conventions preserve, is kept in R11,
/// which neither does: the stack stays as the call left it, so the function is a leaf that needs
/// no unwind information (on Windows, a stack walk inside it reads the frame right).
fn cpuid_code(windows: bool) -> Vec<u8> {
    let mut c = vec![0x49, 0x89, 0xdb]; // mov r11, rbx
    c.extend(match windows {
        true => [0x89, 0xc8, 0x89, 0xd1],  // mov eax, ecx; mov ecx, edx
        false => [0x89, 0xf8, 0x89, 0xf1], // mov eax, edi; mov ecx, esi
    });
    c.extend([
        0x0f, 0xa2, // cpuid
        0x66, 0x0f, 0x6e, 0xc0, // movd xmm0, eax
        0x66, 0x0f, 0x6e, 0xcb, // movd xmm1, ebx
        0x66, 0x0f, 0x62, 0xc1, // punpckldq xmm0, xmm1
        0x66, 0x0f, 0x6e, 0xc9, // movd xmm1, ecx
        0x66, 0x0f, 0x6e, 0xd2, // movd xmm2, edx
        0x66, 0x0f, 0x62, 0xca, // punpckldq xmm1, xmm2
        0x66, 0x0f, 0x6c, 0xc1, // punpcklqdq xmm0, xmm1
        0x4c, 0x89, 0xdb, // mov rbx, r11
        0xc3, // ret
    ]);
    c
}

/// `xgetbv` of the register the first argument names, as EDX:EAX in RAX.
fn xgetbv_code(windows: bool) -> Vec<u8> {
    let mut c = match windows {
        true => vec![],            // the argument is in ECX already
        false => vec![0x89, 0xf9], // mov ecx, edi
    };
    c.extend([
        0x0f, 0x01, 0xd0, // xgetbv
        0x48, 0xc1, 0xe2, 0x20, // shl rdx, 32
        0x48, 0x09, 0xd0, // or rax, rdx
        0xc3, // ret
    ]);
    c
}

/// Which of the two `call` is, if the module defines it: on x86-64 only.
pub(crate) fn kind<M: Module>(module: &M, call: &velt_vir::simd::SimdCall) -> Option<bool> {
    use velt_vir::simd::SimdCall;
    if module.isa().triple().architecture != target_lexicon::Architecture::X86_64 {
        return None;
    }
    match call {
        SimdCall::Cpuid => Some(true),
        SimdCall::Xgetbv => Some(false),
        _ => None,
    }
}

/// Define `id` as `cpuid` (`is_cpuid`) or `xgetbv`, for `call_conv`.
pub(crate) fn define<M: Module>(
    module: &mut M,
    call_conv: CallConv,
    id: FuncId,
    is_cpuid: bool,
) -> CodegenResult<()> {
    let windows = call_conv == CallConv::WindowsFastcall;
    let code = match is_cpuid {
        true => cpuid_code(windows),
        false => xgetbv_code(windows),
    };
    module
        .define_function_bytes(id, 16, &code, &[])
        .map_err(|e| format!("codegen: defining the CPU's question: {e}"))?;
    Ok(())
}

/// The linkage of the two functions: this object's own.
pub(crate) const LINKAGE: Linkage = Linkage::Local;

#[cfg(test)]
mod tests {
    use super::*;

    /// The code runs on the CPU this machine has: with each calling convention's argument
    /// registers, it gives what Rust's `__cpuid_count` and `_xgetbv` give.
    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn the_code_asks_the_cpu_as_rust_does() {
        let cpuid = cpuid_code(false);
        let xgetbv = xgetbv_code(false);
        let page = executable(&[&cpuid[..], &xgetbv[..]].concat());
        // SAFETY: the bytes are the functions above, in executable memory, called with the
        // System V convention they are written for.
        unsafe {
            let f: extern "sysv64" fn(u32, u32) -> std::arch::x86_64::__m128i =
                std::mem::transmute(page);
            for (leaf, sub) in [(0, 0), (1, 0), (7, 0), (0x8000_0001, 0)] {
                let want = std::arch::x86_64::__cpuid_count(leaf, sub);
                let got: [u32; 4] = std::mem::transmute(f(leaf, sub));
                assert_eq!(
                    got,
                    [want.eax, want.ebx, want.ecx, want.edx],
                    "leaf {leaf:#x}"
                );
            }
            let osxsave = std::arch::x86_64::__cpuid_count(1, 0).ecx & (1 << 27) != 0;
            if osxsave {
                let g: extern "sysv64" fn(u32) -> u64 = std::mem::transmute(page.add(cpuid.len()));
                assert_eq!(g(0), xcr0());
            }
        }
        // Windows' convention, which Rust calls on any x86-64 host.
        let (cpuid, xgetbv) = (cpuid_code(true), xgetbv_code(true));
        let page = executable(&[&cpuid[..], &xgetbv[..]].concat());
        // SAFETY: as above, with the Windows x64 convention the bytes are written for.
        unsafe {
            let f: extern "win64" fn(u32, u32) -> std::arch::x86_64::__m128i =
                std::mem::transmute(page);
            for (leaf, sub) in [(0, 0), (1, 0), (7, 0)] {
                let want = std::arch::x86_64::__cpuid_count(leaf, sub);
                let got: [u32; 4] = std::mem::transmute(f(leaf, sub));
                assert_eq!(
                    got,
                    [want.eax, want.ebx, want.ecx, want.edx],
                    "win64 {leaf:#x}"
                );
            }
            if std::arch::x86_64::__cpuid_count(1, 0).ecx & (1 << 27) != 0 {
                let g: extern "win64" fn(u32) -> u64 = std::mem::transmute(page.add(cpuid.len()));
                assert_eq!(g(0), xcr0());
            }
        }
    }

    #[cfg(all(target_arch = "x86_64", unix))]
    #[target_feature(enable = "xsave")]
    unsafe fn xcr0_inner() -> u64 {
        // SAFETY: the caller checked OSXSAVE.
        unsafe { std::arch::x86_64::_xgetbv(0) }
    }

    #[cfg(all(target_arch = "x86_64", unix))]
    fn xcr0() -> u64 {
        // SAFETY: called only where CPUID says OSXSAVE.
        unsafe { xcr0_inner() }
    }

    /// `code` copied into a fresh executable mapping (never freed: a test).
    #[cfg(all(target_arch = "x86_64", unix))]
    fn executable(code: &[u8]) -> *const u8 {
        extern "C" {
            fn mmap(a: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
        }
        const PROT_ALL: i32 = 1 | 2 | 4;
        let flags = if cfg!(target_os = "macos") {
            0x1002
        } else {
            0x22
        }; // MAP_PRIVATE | MAP_ANON
           // SAFETY: an anonymous mapping, written before it is run.
        unsafe {
            let p = mmap(std::ptr::null_mut(), 4096, PROT_ALL, flags, -1, 0);
            assert!(!p.is_null() && p as isize != -1, "mmap");
            std::ptr::copy_nonoverlapping(code.as_ptr(), p, code.len());
            p
        }
    }
}
