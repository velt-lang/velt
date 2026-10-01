//! Target selection: triple → configured Cranelift ISA.
//!
//! Settings per target:
//! - `is_pic`: true for ELF/Mach-O (PIE executables, mandatory on arm64 macOS); false for COFF,
//!   which has no GOT and relies on base relocations instead.
//! - stack probes: always inline (no dependency on `__chkstk`/`__rust_probestack`); required on
//!   Windows where the guard page must be touched in order, and harmless elsewhere.
//! - frame pointers preserved on all targets: an Apple ABI requirement, and cheap elsewhere
//!   (Cranelift already keeps them in non-leaf functions) so profilers/debuggers can walk the
//!   stack on ELF/Mach-O, where no `.eh_frame` is emitted yet.

use cranelift_codegen::isa::{self, OwnedTargetIsa};
use cranelift_codegen::settings::{self, Configurable};
use target_lexicon::{Architecture, BinaryFormat, DeploymentTarget, OperatingSystem, Triple};

use crate::CodegenResult;

/// Build the ISA for `target` (empty / `native` / `host` = detect the host).
/// `jit` forces non-PIC code, as required by `cranelift-jit`.
pub(crate) fn make_isa(target: &str, optimize: bool, jit: bool) -> CodegenResult<OwnedTargetIsa> {
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
    if native {
        cranelift_native::infer_native_flags(&mut isa_builder)
            .map_err(|e| format!("codegen: host is not supported: {e}"))?;
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
    if jit {
        set(&mut flags, "use_colocated_libcalls", "false")?;
    }
    isa_builder
        .finish(settings::Flags::new(flags))
        .map_err(|e| format!("codegen: cannot build ISA for `{triple}`: {e}"))
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

fn set(flags: &mut settings::Builder, name: &str, value: &str) -> CodegenResult<()> {
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
