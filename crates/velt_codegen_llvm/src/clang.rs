//! Compiling textual IR with the `clang` that `clang_search` chose.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use crate::clang_probe::{try_compile, MIN_LLVM_MAJOR, PROBE_LIMIT};
use crate::clang_search::{clang_version, find_clang, forget_clang, rejected_clang};
use crate::target::Target;
use crate::CodegenResult;

/// Compile `ir` to an object file for `target` and return its bytes; from `cache` when this clang
/// compiled the same IR with the same flags before (on this machine, in any build), else storing
/// it there. The IR is freed once clang has read it, before clang compiles it: a unit's text is
/// large, and several units are compiled at once.
pub(crate) fn compile_cached(
    ir: String,
    target: &Target,
    optimize: bool,
    cache: Option<&dyn velt_codegen_cl::CodeCache>,
) -> CodegenResult<(Vec<u8>, bool)> {
    let Some(clang) = find_clang() else {
        let what = rejected_clang().unwrap_or_else(|| "the LLVM backend needs clang".into());
        bail!(
            "{what}; set $VELT_CLANG, add clang {MIN_LLVM_MAJOR}+ to PATH, or install LLVM (e.g. \
             `winget install LLVM.LLVM`, `sudo apt install clang-18`, `brew install llvm`); or \
             use `--backend cranelift`"
        )
    };
    let mut args: Vec<String> = vec![if optimize { opt_flag() } else { "-O0" }.into()];
    args.extend(["-c", "-x", "ir", "-Wno-override-module"].map(String::from));
    args.push(format!("--target={}", target.triple));
    args.extend(target.cpu_arg());
    args.extend(target.deployment_target_arg().map(String::from));
    if target.pic() {
        args.push("-fPIC".into());
    }
    if target.outline_atomics() {
        args.push("-moutline-atomics".into());
    }
    args.extend(["-", "-o", "-"].map(String::from));
    // The key: this clang (path, size and time of its executable, and the version it reports: a
    // new clang can sit behind the same three), its arguments (`-march=native` means this
    // machine's CPU: the cache is the machine's), and the IR, hashed as it is read (a key of 32
    // bytes is the cache's key as it is), so the IR is not copied into it.
    let key = cache.map(|_| {
        let meta = std::fs::metadata(&clang).ok();
        let stamp = meta.map(|m| (m.len(), m.modified().ok()));
        let id = format!(
            "clang {clang:?} {stamp:?} {:?} {args:?} {}",
            clang_version(),
            velt_codegen_cl::host_triple()
        );
        let mut key = blake3::Hasher::new();
        key.update(id.as_bytes());
        key.update(&[0]);
        key.update(ir.as_bytes());
        *key.finalize().as_bytes()
    });
    if let (Some(cache), Some(key)) = (cache, &key) {
        if let Some(obj) = cache.get(key) {
            return Ok((obj, true));
        }
    }
    let mut cmd = Command::new(&clang);
    cmd.args(&args);
    let obj = run_piped(cmd, ir.into_bytes(), "clang").map_err(|e| {
        let named = unknown_cpu(e.clone(), target.cpu.as_deref());
        if named != e {
            return named;
        }
        blame(e, &clang, &args, &target.triple)
    })?;
    if let (Some(cache), Some(key)) = (cache, &key) {
        cache.put(key, &obj);
    }
    Ok((obj, false))
}

/// The arm64 features clang lets code for `-mcpu=<cpu>` on `triple` use, as a mask over
/// `velt_codegen_cl::ARM_FEATURES`: from the macros it defines, once per CPU and process. Without
/// a clang, none (code then uses no feature beyond the baseline).
pub(crate) fn arm_features(triple: &str, cpu: &str) -> CodegenResult<u32> {
    static FEATURES: OnceLock<Mutex<HashMap<(String, String), u32>>> = OnceLock::new();
    let features = FEATURES.get_or_init(Default::default);
    let key = (triple.to_string(), cpu.to_string());
    if let Some(mask) = features.lock().ok().and_then(|f| f.get(&key).copied()) {
        return Ok(mask);
    }
    let Some(clang) = find_clang() else {
        return Ok(0);
    };
    let out = Command::new(&clang)
        .args(["-dM", "-E", "-x", "c"])
        .arg(format!("--target={triple}"))
        .arg(format!("-mcpu={cpu}"))
        .arg("-")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("codegen: cannot run `{}`: {e}", clang.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(unknown_cpu(
            format!("codegen: `clang -mcpu={cpu}` failed:\n{err}"),
            Some(cpu),
        ));
    }
    let mask = velt_codegen_cl::arm_features_from_macros(&String::from_utf8_lossy(&out.stdout));
    if let Ok(mut f) = features.lock() {
        f.insert(key, mask);
    }
    Ok(mask)
}

/// The x86 features clang lets code for `-march=<cpu>` on `triple` use, as a mask over
/// `velt_codegen_cl::X86_FEATURES`: from the macros it defines (`clang -dM -E`), once per CPU
/// and process. Without a clang, Cranelift's idea of the CPU (`velt build --emit llvm` needs no
/// clang; everything that compiles the IR does).
pub(crate) fn x86_features(triple: &str, cpu: &str) -> CodegenResult<u32> {
    static FEATURES: OnceLock<Mutex<HashMap<(String, String), u32>>> = OnceLock::new();
    let features = FEATURES.get_or_init(Default::default);
    let key = (triple.to_string(), cpu.to_string());
    if let Some(mask) = features.lock().ok().and_then(|f| f.get(&key).copied()) {
        return Ok(mask);
    }
    let Some(clang) = find_clang() else {
        return velt_codegen_cl::required_features(triple, Some(cpu));
    };
    let out = Command::new(&clang)
        .args(["-dM", "-E", "-x", "c"])
        .arg(format!("--target={triple}"))
        .arg(format!("-march={cpu}"))
        .arg("-")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("codegen: cannot run `{}`: {e}", clang.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(unknown_cpu(
            format!("codegen: `clang -march={cpu}` failed:\n{err}"),
            Some(cpu),
        ));
    }
    let mask = velt_codegen_cl::x86_features_from_macros(&String::from_utf8_lossy(&out.stdout));
    if let Ok(mut f) = features.lock() {
        f.insert(key, mask);
    }
    Ok(mask)
}

/// `err` from a failed compile by `clang` with `args`, as what it means. Generated IR that clang
/// rejects is the compiler's bug, but a clang that can't run, or doesn't have the target, fails on
/// any IR. So it is tried on the smallest IR: failing that too says it's the clang. Then what is
/// known about it is taken back, since the file can be the same when what it needs has gone: the
/// next build asks it again and can pass it over. This one has no other clang to go to, because
/// its keys name this one.
fn blame(err: String, clang: &Path, args: &[String], triple: &str) -> String {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let Err(failure) = try_compile(clang, &args, PROBE_LIMIT) else {
        return err;
    };
    forget_clang();
    format!(
        "the clang in use, `{}` ({}), can't compile for `{triple}`, whatever the program: {}; set \
         $VELT_CLANG to a clang {MIN_LLVM_MAJOR}+ that can, or use `--backend cranelift`",
        clang.display(),
        clang_version().as_deref().unwrap_or("version unknown"),
        failure.into_text()
    )
}

/// `err` from clang, as the user's error when it is that clang does not know the CPU `cpu` (a
/// name newer than the clang in use: the names are checked before, `cpu_names.rs`).
fn unknown_cpu(err: String, cpu: Option<&str>) -> String {
    match cpu {
        Some(cpu) if err.contains("unknown target CPU") => {
            let clang = find_clang()
                .map(|c| c.display().to_string())
                .unwrap_or_default();
            format!(
                "the clang in use (`{clang}`) does not know CPU `{cpu}`: use a newer clang \
                 ($VELT_CLANG) or name another CPU"
            )
        }
        _ => err,
    }
}

/// Run an LLVM tool that reads its input from stdin and writes its output to stdout (`-` for
/// both), and return the output. Nothing goes through files: a scratch file just written by a
/// tool could be unreadable for a moment on Windows (another process, a virus scanner, still
/// holding it), and a scratch path can collide with one a crashed process left behind. An owned
/// `input` is freed as soon as the tool has read it.
pub(crate) fn run_piped(
    mut cmd: Command,
    input: impl AsRef<[u8]> + Send,
    name: &str,
) -> CodegenResult<Vec<u8>> {
    let program = cmd.get_program().to_owned();
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "codegen: cannot run `{}`: {e}",
                Path::new(&program).display()
            )
        })?;
    let mut stdin = child.stdin.take().expect("ICE: stdin is piped");
    // Feed the input from another thread while the output is read here: a tool may write
    // before it has read everything, and both pipes have small buffers.
    let (out, fed) = std::thread::scope(|scope| {
        // `stdin` is closed and `input` freed when the feeder is done.
        let feeder = scope.spawn(move || stdin.write_all(input.as_ref()));
        let out = child.wait_with_output();
        let fed = feeder.join().expect("ICE: the stdin feeder panicked");
        out.map(|out| (out, fed))
    })
    .map_err(|e| {
        format!(
            "codegen: cannot run `{}`: {e}",
            Path::new(&program).display()
        )
    })?;
    if !out.status.success() {
        bail!(
            "codegen: {name} failed on the generated IR (this is a compiler bug):\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // A tool that failed may stop reading: then its error above is reported, not the broken
    // pipe. One that succeeded without reading all of its input compiled only part of it.
    fed.map_err(|e| format!("codegen: cannot write the IR to {name}: {e}"))?;
    Ok(out.stdout)
}

/// clang's optimization flag for release builds: `-O3`, or `$VELT_LLVM_OPT` (`1`, `2`, `3`, `s`
/// or `z`, with or without the `-O`) to trade code speed for compile time.
pub fn opt_flag() -> &'static str {
    static FLAG: OnceLock<&'static str> = OnceLock::new();
    FLAG.get_or_init(|| {
        let level = std::env::var("VELT_LLVM_OPT").unwrap_or_default();
        parse_opt_level(&level).unwrap_or("-O3")
    })
}

fn parse_opt_level(level: &str) -> Option<&'static str> {
    Some(match level.trim().trim_start_matches("-O") {
        "1" => "-O1",
        "2" => "-O2",
        "3" => "-O3",
        "s" => "-Os",
        "z" => "-Oz",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_cpus_have_the_features_clang_gives_them() {
        if find_clang().is_none() {
            return;
        }
        let names = |mask: u32| {
            (0..velt_codegen_cl::X86_FEATURES.len())
                .filter(|b| mask & 1 << b != 0)
                .map(|b| velt_codegen_cl::X86_FEATURES[b].0)
                .collect::<Vec<_>>()
        };
        let linux = "x86_64-unknown-linux-gnu";
        // Cranelift's presets of these have no AVX2 (nor MOVBE and F16C): clang's do.
        for cpu in ["alderlake", "znver4", "haswell"] {
            let has = names(x86_features(linux, cpu).unwrap());
            for f in ["AVX", "AVX2", "MOVBE", "F16C", "POPCNT"] {
                assert!(has.contains(&f), "{cpu}: {f} in {has:?}");
            }
        }
        let skx = names(x86_features(linux, "skylake-avx512").unwrap());
        assert!(
            skx.contains(&"AVX512BW") && skx.contains(&"AVX512CD"),
            "{skx:?}"
        );
        assert!(!names(x86_features(linux, "core2").unwrap()).contains(&"SSE4.1"));
        let err = "clang: error: unknown target CPU 'apple-m9'".to_string();
        let msg = unknown_cpu(err.clone(), Some("apple-m9"));
        assert!(msg.contains("does not know CPU `apple-m9`"), "{msg}");
        assert_eq!(unknown_cpu(err.clone(), None), err);
        assert_eq!(unknown_cpu("other".into(), Some("znver4")), "other");
    }

    #[test]
    fn optimization_levels() {
        assert_eq!(parse_opt_level("2"), Some("-O2"));
        assert_eq!(parse_opt_level("-O2"), Some("-O2"));
        assert_eq!(parse_opt_level(" s "), Some("-Os"));
        assert_eq!(parse_opt_level(""), None);
        assert_eq!(parse_opt_level("fast"), None);
    }
}
