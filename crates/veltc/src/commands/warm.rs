//! `velt cache warm`: compile std into the build cache before a first build needs it (#1612).
//!
//! The warm-up programs (`<prefix>/share/velt/warm/*.vlt`; in a checkout, the std goldens that
//! `share/velt/warm/programs.txt` lists) together reach most of std and its common generic
//! instances. Each is built in debug, as an executable (what `velt build`, `velt run` and `velt
//! test` build: an object alone compiles some code differently), for this machine's CPU (what `velt run` and `velt test` build for) and for the
//! target's default CPU (what `velt build` builds for). The build cache then holds std's
//! functions, so a project's first build compiles little more than its own code. A marker per
//! toolchain (`<cache>/warm/<key>`) makes later runs return at once; the toolchain installers
//! start `velt cache warm --background` after installing a version, except in CI (`$CI` set),
//! whose cache goes with the runner, and when `$VELT_CACHE_WARM` is `off`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use velt_build_cache::KeyHasher;

/// The CPUs to warm for: this machine's, then the target's default.
const CPUS: [Option<&str>; 2] = [Some("native"), None];

/// `velt cache warm [--background]`.
pub fn warm_command(background: bool) -> Result<(), String> {
    if background && !wanted_in_background() {
        return Ok(());
    }
    let Some(root) = crate::build_cache::root() else {
        eprintln!("velt: the build cache is off ($VELT_BUILD_CACHE): nothing to warm");
        return Ok(());
    };
    let Some(dir) = warm_dir() else {
        return Err("no warm-up programs: this velt has no share/velt/warm/".into());
    };
    let marker = root.join("warm").join(toolchain_key().hex());
    if marker.is_file() {
        eprintln!("velt: the build cache is warm for this velt already");
        return Ok(());
    }
    if background {
        return start_in_background();
    }
    std::fs::create_dir_all(marker.parent().unwrap_or(&root))
        .map_err(|e| format!("cannot create `{}`: {e}", root.display()))?;
    // One warm-up per toolchain at a time: another one running does the work.
    let lock = std::fs::File::create(marker.with_extension("lock"))
        .map_err(|e| format!("cannot create a lock in `{}`: {e}", root.display()))?;
    if lock.try_lock().is_err() {
        eprintln!("velt: the build cache is being warmed by another velt");
        return Ok(());
    }
    let programs = programs(&dir);
    // The objects are thrown away: what counts is what codegen stored in the cache.
    let out = root
        .join("warm")
        .join(format!("out-{}", std::process::id()));
    std::fs::create_dir_all(&out).map_err(|e| format!("cannot create `{}`: {e}", out.display()))?;
    let started = std::time::Instant::now();
    let mut failed = vec![];
    for cpu in CPUS {
        for program in &programs {
            if !compile(program, cpu, &out) {
                failed.push(program.display().to_string());
            }
        }
    }
    let _ = std::fs::remove_dir_all(&out);
    eprintln!(
        "velt: warmed the build cache with {} programs in {:.1}s",
        programs.len(),
        started.elapsed().as_secs_f64()
    );
    if !failed.is_empty() {
        // A program that no longer compiles warms nothing, but the others did their part.
        return Err(format!(
            "some warm-up programs did not compile: {}",
            failed.join(", ")
        ));
    }
    std::fs::write(&marker, crate::commands::version::version_line())
        .map_err(|e| format!("cannot write `{}`: {e}", marker.display()))
}

/// Whether the installers' background warm-up should run: not in CI (`$CI`), whose cache is
/// thrown away with the runner, nor when `$VELT_CACHE_WARM` is `off`.
fn wanted_in_background() -> bool {
    let set = |name: &str| std::env::var(name).is_ok_and(|v| !v.is_empty());
    let off = std::env::var("VELT_CACHE_WARM").is_ok_and(|v| crate::build_cache::is_off(&v));
    !set("CI") && !off
}

/// Build `program` as an executable in `out` for `cpu`, by a child `velt build`, so one program
/// that fails stops nothing else. The build stamp is off: an unchanged stamp would skip codegen.
fn compile(program: &Path, cpu: Option<&str>, out: &Path) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let mut cmd = Command::new(exe);
    cmd.arg("build")
        .arg(program)
        .arg("-o")
        .arg(out.join(if cfg!(windows) { "warm.exe" } else { "warm" }))
        .env("VELT_BUILD_STAMP", "off")
        .env_remove(super::cpu::CPU_VAR)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(cpu) = cpu {
        cmd.args(["--cpu", cpu]);
    }
    cmd.status().is_ok_and(|s| s.success())
}

/// Run `velt cache warm` detached, at low priority, and return.
fn start_in_background() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this velt: {e}"))?;
    let mut cmd = Command::new(exe);
    cmd.args(["cache", "warm"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut cmd);
    cmd.spawn()
        .map_err(|e| format!("cannot start `velt cache warm`: {e}"))?;
    eprintln!("velt: warming the build cache in the background");
    Ok(())
}

/// Its own process group (a Ctrl-C in the installer's terminal does not stop it), at the lowest
/// priority, inherited by the builds it starts.
#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    // SAFETY: `setpriority` is async-signal-safe and touches no memory of the parent.
    unsafe {
        cmd.pre_exec(|| {
            libc::setpriority(libc::PRIO_PROCESS, 0, 19);
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, IDLE_PRIORITY_CLASS,
    };
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | IDLE_PRIORITY_CLASS);
}

#[cfg(not(any(unix, windows)))]
fn detach(_: &mut Command) {}

/// Who compiled the cache's entries: this velt's version line and its executable (a rebuilt
/// development velt is another toolchain).
fn toolchain_key() -> velt_build_cache::Key {
    let mut h = KeyHasher::new();
    h.part(crate::commands::version::version_line().as_bytes());
    if let Some(meta) = std::env::current_exe()
        .ok()
        .and_then(|e| std::fs::metadata(e).ok())
    {
        h.part(&meta.len().to_le_bytes());
        let time = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
        h.part(&time.unwrap_or_default().as_nanos().to_le_bytes());
    }
    h.finish()
}

/// The warm-up programs: the `.vlt` files of `dir` by name, then those `programs.txt` there lists
/// (one path per line, relative to `dir`; `#` starts a comment line).
fn programs(dir: &Path) -> Vec<PathBuf> {
    let mut programs: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "vlt"))
        .collect();
    programs.sort();
    let list = std::fs::read_to_string(dir.join(LIST)).unwrap_or_default();
    programs.extend(
        list.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| dir.join(l)),
    );
    programs
}

/// The list of warm-up programs in a checkout.
const LIST: &str = "programs.txt";

/// `$VELT_SHARE/warm`; else `share/velt/warm` beside the std this velt uses
/// (`crate::loader::std_root`: `<prefix>/std` installed, the checkout's `std/` in one, wherever
/// its target directory is), so the programs and the std they exercise come from one place.
fn warm_dir() -> Option<PathBuf> {
    if let Some(share) = std::env::var_os("VELT_SHARE").filter(|s| !s.is_empty()) {
        return Some(Path::new(&share).join("warm")).filter(|d| d.is_dir());
    }
    warm_dir_of_std(&crate::loader::std_root()?)
}

fn warm_dir_of_std(std: &Path) -> Option<PathBuf> {
    let warm = std.parent()?.join("share").join("velt").join("warm");
    warm.is_dir().then_some(warm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_installed_and_checkout_programs() {
        let tmp = tempfile::tempdir().unwrap();
        let prefix = tmp.path().join("prefix");
        let warm = prefix.join("share/velt/warm");
        std::fs::create_dir_all(&warm).unwrap();
        std::fs::create_dir_all(prefix.join("std")).unwrap();
        assert_eq!(warm_dir_of_std(&prefix.join("std")), Some(warm));

        // A checkout: beside its std, whatever directory the executable is in.
        let repo = tmp.path().join("repo");
        let warm = repo.join("share/velt/warm");
        std::fs::create_dir_all(&warm).unwrap();
        assert_eq!(warm_dir_of_std(&repo.join("std")), Some(warm.clone()));
        assert_eq!(warm_dir_of_std(&tmp.path().join("other/std")), None);
        std::fs::write(warm.join("b.vlt"), "").unwrap();
        std::fs::write(warm.join("a.vlt"), "").unwrap();
        std::fs::write(warm.join("notes.md"), "").unwrap();
        std::fs::write(warm.join(LIST), "# goldens\n\n../g/c.vlt\n").unwrap();
        assert_eq!(
            programs(&warm),
            [
                warm.join("a.vlt"),
                warm.join("b.vlt"),
                warm.join("../g/c.vlt")
            ]
        );
    }

    /// The checkout's list names programs that exist (a golden renamed or removed would make
    /// every warm-up, and packaging, fail).
    #[test]
    fn the_listed_programs_exist() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../share/velt/warm");
        let programs = programs(&dir);
        assert!(programs.len() >= 10, "{programs:?}");
        for p in programs {
            assert!(p.is_file(), "{} is listed but missing", p.display());
        }
    }
}
