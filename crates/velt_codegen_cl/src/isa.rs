//! Target selection: triple → configured Cranelift ISA.
//!
//! Settings per target:
//! - `is_pic`: true for ELF/Mach-O (PIE executables, mandatory on arm64 macOS); false for COFF,
//!   which has no GOT and relies on base relocations instead.
//! - stack probes: always inline (no dependency on `__chkstk`/`__rust_probestack`); required on
//!   Windows where the guard page must be touched in order, and harmless elsewhere.
//! - frame pointers preserved on all targets: an Apple ABI requirement, and cheap elsewhere
//!   (Cranelift already keeps them in non-leaf functions) so profilers and debuggers that walk
//!   frame pointers see every frame. Unwind tables are emitted too (`crate::unwind`: `.eh_frame`
//!   on ELF and Mach-O, `.pdata`/`.xdata` on Windows): a caught Velt panic unwinds through them
//!   (`catchPanic`, velt_rt panic.rs).
//! - the CPU (`--cpu`, `crate::cpu`): x86_64 code is for `cpu::DEFAULT_X86_CPU` unless another
//!   one is asked for.
//! - Cranelift's IR verifier only in the compiler's own debug builds (`VELT_CL_VERIFY=1` turns
//!   it on), and the register allocator Cranelift's default unless `VELT_CL_REGALLOC` names one.

use cranelift_codegen::isa::{self, OwnedTargetIsa, TargetIsa};
use cranelift_codegen::settings::{self, Configurable, TlsModel};
use target_lexicon::{Architecture, BinaryFormat, DeploymentTarget, OperatingSystem, Triple};

use crate::{cpu, CodegenResult};

/// Build the ISA for `target` (empty / `native` / `host` = detect the host) and CPU `cpu`
/// (`None`: `native` for a native target, else `cpu::DEFAULT_X86_CPU` on x86_64).
/// `jit` forces non-PIC code, as required by `cranelift-jit`.
pub(crate) fn make_isa(
    target: &str,
    cpu: Option<&str>,
    optimize: bool,
    jit: bool,
) -> CodegenResult<OwnedTargetIsa> {
    let target = target.trim();
    let native = target.is_empty() || target == "native" || target == "host";
    let triple = if native {
        Triple::host()
    } else {
        target
            .parse()
            .map_err(|e| format!("codegen: invalid target triple `{target}`: {e}"))?
    };
    let triple = with_macos_platform(triple);
    check_supported(&triple)?;
    let mut isa_builder =
        isa::lookup(triple).map_err(|e| format!("codegen: unsupported target `{target}`: {e}"))?;
    let cpu = cpu.or(native.then_some("native"));
    if let Some(c) = cpu {
        cpu::check_cpu(target, c).map_err(|e| format!("codegen: {e}"))?;
    }
    cpu::configure(&mut isa_builder, cpu)?;
    let arm64 = matches!(isa_builder.triple().architecture, Architecture::Aarch64(_));
    let apple = matches!(
        isa_builder.triple().operating_system,
        OperatingSystem::Darwin(_) | OperatingSystem::MacOSX(_)
    );
    if arm64 && (jit || apple) {
        // A native CPU's flags sign return addresses (pointer authentication). Apple's arm64 ABI
        // (not arm64e) has no signed return addresses: clang emits none, and macOS 26's unwinder
        // can't read the rule Cranelift writes for them (`DW_CFA_val_expression` of
        // RA_SIGN_STATE, register 34), so a caught panic (#2008) failed to unwind there, and
        // stack walks through JIT frames stopped on some Apple CPUs. Code for Apple targets, and
        // JIT code anywhere, keeps the CPU's other instructions with plain return addresses.
        set(&mut isa_builder, "sign_return_address", "false")?;
    }
    let triple = isa_builder.triple().clone();
    check_supported(&triple)?;

    let mut flags = settings::builder();
    let opt_level = if optimize { "speed" } else { "none" };
    let pic = !jit && triple.binary_format != BinaryFormat::Coff;
    set(&mut flags, "opt_level", opt_level)?;
    set(&mut flags, "is_pic", if pic { "true" } else { "false" })?;
    set(&mut flags, "enable_probestack", "true")?;
    set(&mut flags, "probestack_strategy", "inline")?;
    set(&mut flags, "preserve_frame_pointers", "true")?;
    // Cranelift verifies its IR after every pass by default: 23% of a debug build's
    // instructions on a generated 10k-line application (#2707). VIR is verified before code
    // generation (`velt_vir::verify`), so the compiler's own debug builds and
    // `VELT_CL_VERIFY=1` keep it.
    let verify =
        cfg!(debug_assertions) || std::env::var_os("VELT_CL_VERIFY").is_some_and(|v| v != "0");
    set(
        &mut flags,
        "enable_verifier",
        if verify { "true" } else { "false" },
    )?;
    // `VELT_CL_REGALLOC=single_pass`: 11-13% fewer build instructions, programs 1.2-1.7x
    // slower than with the default `backtracking` allocator (#2707).
    if let Some(alloc) = std::env::var_os("VELT_CL_REGALLOC") {
        set(&mut flags, "regalloc_algorithm", &alloc.to_string_lossy())?;
    }
    if jit {
        set(&mut flags, "use_colocated_libcalls", "false")?;
    }
    // Thread-local storage where Cranelift has it for the format; the JIT (`cranelift-jit` has
    // none) and arm64 COFF use the operating system's thread keys instead (tls.rs).
    let tls = match (jit, triple.binary_format, &triple.architecture) {
        (true, ..) => "none",
        (false, BinaryFormat::Elf, _) => "elf_gd",
        (false, BinaryFormat::Macho, _) => "macho",
        (false, BinaryFormat::Coff, Architecture::X86_64) => "coff",
        _ => "none",
    };
    set(&mut flags, "tls_model", tls)?;
    isa_builder
        .finish(settings::Flags::new(flags))
        .map_err(|e| format!("codegen: cannot build ISA for `{triple}`: {e}"))
}

/// The ISA of an object's start-up code (`vir::Function::baseline`, the code that runs before the
/// start-up check of the CPU): `isa`'s target and settings for the baseline CPU. Always an
/// object's (a `velt dev` version, the JIT's, has no baseline ISA): telling a JIT ISA by its PIC
/// flag took every Windows object for one, since COFF code is never PIC, and the baseline code
/// then had no thread-local storage while the module inlined its thread-local values.
pub(crate) fn baseline_of(isa: &dyn TargetIsa) -> CodegenResult<OwnedTargetIsa> {
    let optimize = isa.flags().opt_level() != settings::OptLevel::None;
    make_isa(&isa.triple().to_string(), Some("baseline"), optimize, false)
}

/// Whether a program for `target` has thread-local storage that LLVM-built objects can share
/// (`velt_tls_data_<id>`), not the operating system's thread keys (tls.rs).
pub fn has_thread_local_data(target: &str) -> bool {
    make_isa(target, None, false, false).is_ok_and(|isa| isa.flags().tls_model() != TlsModel::None)
}

/// `*-apple-darwin` → `*-apple-macosx<min>`. `cranelift-object` writes the Mach-O
/// `LC_BUILD_VERSION` from the triple's OS, and a bare `darwin` yields `PLATFORM_UNKNOWN`,
/// which Apple's linker (ld-prime) rejects with "unknown platform". The minimums match rustc's
/// defaults (11.0 for arm64, 10.12 for x86_64), so objects link cleanly with the runtime.
fn with_macos_platform(mut triple: Triple) -> Triple {
    if let OperatingSystem::Darwin(version) | OperatingSystem::MacOSX(version) =
        triple.operating_system
    {
        let default = match triple.architecture {
            Architecture::Aarch64(_) => DeploymentTarget {
                major: 11,
                minor: 0,
                patch: 0,
            },
            _ => DeploymentTarget {
                major: 10,
                minor: 12,
                patch: 0,
            },
        };
        triple.operating_system = OperatingSystem::MacOSX(Some(version.unwrap_or(default)));
    }
    triple
}

fn set(flags: &mut impl Configurable, name: &str, value: &str) -> CodegenResult<()> {
    flags
        .set(name, value)
        .map_err(|e| format!("codegen: cannot set `{name}={value}`: {e}"))
}

fn check_supported(triple: &Triple) -> CodegenResult<()> {
    let arch_ok = matches!(
        triple.architecture,
        Architecture::X86_64 | Architecture::Aarch64(_)
    );
    let os_ok = matches!(
        triple.operating_system,
        OperatingSystem::Windows
            | OperatingSystem::Linux
            | OperatingSystem::Darwin(_)
            | OperatingSystem::MacOSX(_)
    );
    let format_ok = matches!(
        triple.binary_format,
        BinaryFormat::Coff | BinaryFormat::Elf | BinaryFormat::Macho
    );
    if arch_ok && os_ok && format_ok {
        Ok(())
    } else {
        Err(format!(
            "codegen: unsupported target `{triple}` (supported: x86_64/aarch64 × windows-msvc, \
             apple-darwin, unknown-linux-gnu)"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The start-up code of an object keeps the object's thread-local storage: on Windows too,
    /// whose objects are not PIC, so a test of PIC took them for JIT code, which has none.
    #[test]
    fn baseline_code_keeps_thread_local_storage() {
        for target in [
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-linux-gnu",
            "aarch64-apple-darwin",
        ] {
            let isa = make_isa(
                target,
                target.starts_with("x86").then_some("x86-64-v3"),
                false,
                false,
            )
            .unwrap();
            let base = baseline_of(&*isa).unwrap();
            assert_eq!(
                base.flags().tls_model(),
                isa.flags().tls_model(),
                "{target}"
            );
            assert_ne!(base.flags().tls_model(), TlsModel::None, "{target}");
            assert_eq!(base.flags().is_pic(), isa.flags().is_pic(), "{target}");
        }
    }
}
