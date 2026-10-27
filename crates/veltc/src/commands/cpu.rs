//! Which CPU a build is for (docs/tooling/platforms.md "CPU"): `--cpu`, else `$VELT_CPU`, else the
//! root package's `build.cpu`, else the target's default; a program built to run here and now
//! (`velt run` and `velt test` without `--release`, `velt dev --exe`) is for this machine's CPU
//! (`native`) unless one of them names another.

/// The environment variable that names the CPU when `--cpu` does not.
pub const CPU_VAR: &str = "VELT_CPU";

/// The CPU to build `target` for, from `--cpu` (`cli`), `$VELT_CPU` (`env`) and the package's
/// `build.cpu` (`config`), checked against the target; `None`: the target's default.
///
/// WebAssembly has no CPU choice: `--cpu` is an error there, the others do not apply. Likewise a
/// `$VELT_CPU` or `build.cpu` meant for another architecture (`x86-64-v3` in a build for arm64,
/// `neoverse-n1` in one for x86_64) does not apply, so one setting serves a package built for several targets.
pub fn resolve_cpu(
    cli: Option<&str>,
    env: Option<&str>,
    config: Option<&str>,
    target: &str,
) -> Result<Option<String>, String> {
    let wasm = super::wasm::is_wasm(target);
    if let Some(cpu) = cli {
        if wasm {
            return Err(format!(
                "`--cpu` does not apply to WebAssembly targets (`{target}`)"
            ));
        }
        velt_codegen_cl::check_cpu(target, cpu)?;
        return Ok(Some(cpu.to_string()));
    }
    let sources = [(env, CPU_VAR), (config, "the package's `build.cpu`")];
    for (cpu, source) in sources {
        let Some(cpu) = cpu.map(str::trim).filter(|c| !c.is_empty()) else {
            continue;
        };
        if wasm {
            return Ok(None);
        }
        match velt_codegen_cl::check_cpu(target, cpu) {
            Ok(()) => return Ok(Some(cpu.to_string())),
            // Meant for another architecture: the target's default.
            Err(_) if velt_codegen_cl::is_for_another_arch(target, cpu) => return Ok(None),
            Err(e) => return Err(format!("{source}: {e}")),
        }
    }
    Ok(None)
}

/// `$VELT_CPU`, if set.
pub fn env_cpu() -> Option<String> {
    std::env::var(CPU_VAR).ok()
}

/// The CPU of a build that runs on this machine right away: `cpu` if one was named, else this
/// machine's (`native`) when the build is for its architecture and not a release build.
pub fn for_running_here(cpu: Option<String>, target: &str, release: bool) -> Option<String> {
    let native = || {
        (!release && !super::wasm::is_wasm(target))
            .then_some("native")
            .filter(|n| velt_codegen_cl::check_cpu(target, n).is_ok())
            .map(str::to_string)
    };
    cpu.or_else(native)
}

#[cfg(test)]
mod tests {
    use super::*;

    const X86: &str = "x86_64-unknown-linux-gnu";
    const ARM: &str = "aarch64-unknown-linux-gnu";

    #[test]
    fn the_flag_wins_then_the_environment_then_the_package() {
        let r = |cli, env, config| resolve_cpu(cli, env, config, X86).unwrap();
        assert_eq!(r(None, None, None), None);
        assert_eq!(
            r(None, None, Some("x86-64-v3")).as_deref(),
            Some("x86-64-v3")
        );
        assert_eq!(
            r(None, Some("baseline"), Some("x86-64-v3")).as_deref(),
            Some("baseline")
        );
        let all = r(Some("znver4"), Some("baseline"), Some("x86-64-v3"));
        assert_eq!(all.as_deref(), Some("znver4"));
        // An empty `VELT_CPU` is not set.
        assert_eq!(
            r(None, Some(" "), Some("x86-64-v4")).as_deref(),
            Some("x86-64-v4")
        );
    }

    #[test]
    fn settings_for_another_architecture_do_not_apply() {
        assert_eq!(
            resolve_cpu(None, Some("x86-64-v3"), None, ARM).unwrap(),
            None
        );
        assert_eq!(resolve_cpu(None, None, Some("znver4"), ARM).unwrap(), None);
        let flag = resolve_cpu(Some("x86-64-v3"), None, None, ARM).unwrap_err();
        assert!(flag.contains("x86_64 targets"), "{flag}");
        assert_eq!(resolve_cpu(None, Some("znver5"), None, ARM).unwrap(), None);
        // An arm64 name leaves x86_64 builds at their default (x86-64-v2, checked).
        assert_eq!(
            resolve_cpu(None, Some("neoverse-n1"), None, X86).unwrap(),
            None
        );
        assert_eq!(
            resolve_cpu(None, None, Some("apple-m4"), X86).unwrap(),
            None
        );
        let n1 = resolve_cpu(Some("neoverse-n1"), None, None, X86).unwrap_err();
        assert!(n1.contains("is for aarch64 targets"), "{n1}");
        // A name no architecture has is an error wherever it comes from.
        let bad = resolve_cpu(None, Some("skylkae"), None, X86).unwrap_err();
        assert!(bad.starts_with("VELT_CPU: unknown CPU `skylkae`"), "{bad}");
        assert!(bad.ends_with("did you mean `skylake`?"), "{bad}");
        let bad = resolve_cpu(None, None, Some("skylkae"), ARM).unwrap_err();
        assert!(
            bad.starts_with("the package's `build.cpu`: unknown CPU"),
            "{bad}"
        );
        assert!(resolve_cpu(Some("skylkae"), None, None, X86).is_err());
        let wasm = "wasm32-wasip1";
        assert_eq!(
            resolve_cpu(None, Some("x86-64-v3"), None, wasm).unwrap(),
            None
        );
        assert!(resolve_cpu(Some("x86-64-v3"), None, None, wasm).is_err());
    }

    #[test]
    fn programs_run_here_are_for_this_cpu_unless_named() {
        let host = velt_codegen_cl::host_triple();
        assert_eq!(
            for_running_here(None, &host, false).as_deref(),
            Some("native")
        );
        assert_eq!(for_running_here(None, &host, true), None);
        let named = for_running_here(Some("baseline".into()), &host, false);
        assert_eq!(named.as_deref(), Some("baseline"));
        assert_eq!(for_running_here(None, "wasm32-wasip1", false), None);
    }
}
