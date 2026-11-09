//! Child-process ABI tests (Unix shells; the Windows equivalents are covered by the goldens).

use super::command::VeltCommand;
use super::output::{velt_rt_child_output_sync, VeltOutput};
use super::*;
use crate::str_array::VeltStrArray;
use std::mem::MaybeUninit;

fn spec(program: &'static str, args: &[&'static str], stdio: u32) -> VeltCommand {
    spec_env(program, args, stdio, &[("VELT_T", "42")])
}

/// A spec adding `env` to the inherited environment.
fn spec_env(
    program: &'static str,
    args: &[&'static str],
    stdio: u32,
    env: &[(&str, &str)],
) -> VeltCommand {
    VeltCommand {
        program: VeltStr::from_static(program.as_bytes()),
        args: VeltStrArray::from_vec(
            args.iter()
                .map(|a| VeltStr::from_static(a.as_bytes()))
                .collect(),
        ),
        cwd: VeltStr::empty(),
        env: VeltStrArray::from_vec(
            env.iter()
                .flat_map(|(name, value)| [name, value])
                .map(|s| VeltStr::from_bytes(s.as_bytes()))
                .collect(),
        ),
        stdio,
        clear_env: 0,
    }
}

unsafe fn text(s: &VeltStr) -> String {
    String::from_utf8_lossy(s.as_bytes()).into_owned()
}

fn output_sync(
    spec: &VeltCommand,
    input: &VeltStr,
) -> Result<(i32, String, String), (i32, String)> {
    let mut out = MaybeUninit::<IoResult<VeltOutput>>::uninit();
    // SAFETY: valid spec and buffers; results are read according to the code.
    unsafe {
        velt_rt_child_output_sync(spec, input, out.as_mut_ptr());
        let r = out.assume_init();
        if r.err.code != 0 {
            return Err((r.err.code, text(&r.err.message)));
        }
        let o = r.value.assume_init();
        Ok((o.code, text(&o.stdout), text(&o.stderr)))
    }
}

#[cfg(unix)]
#[test]
fn output_sync_collects_everything() {
    let s = spec(
        "sh",
        &["-c", "cat; echo \"env=$VELT_T\"; echo oops >&2; exit 3"],
        0,
    );
    // The input in every string form (regression: it was read as a `u8[]`, which only the
    // static form resembled).
    let long = "a line longer than the inline form\n";
    let mut inputs = [
        VeltStr::from_static(b"input\n"),
        VeltStr::from_bytes(b"input\n"),
        VeltStr::from_bytes(long.as_bytes()),
    ];
    for (input, text) in inputs.iter().zip(["input\n", "input\n", long]) {
        assert_eq!(
            output_sync(&s, input),
            Ok((3, format!("{text}env=42\n"), "oops\n".to_string()))
        );
    }
    inputs.iter_mut().for_each(|i| unsafe { i.release() });
}

#[cfg(unix)]
#[test]
fn spawned_child_is_reaped_and_killable() {
    let s = spec("sleep", &["30"], 2 | (2 << 2) | (2 << 4));
    let mut out = MaybeUninit::uninit();
    // SAFETY: valid spec; the handle is released at the end.
    unsafe {
        velt_rt_child_spawn(&s, out.as_mut_ptr());
        let out = out.assume_init();
        assert_eq!(out.err.code, 0);
        let child = out.value.assume_init();
        assert!(velt_rt_child_pid(child) > 0);
        assert_eq!(velt_rt_child_exit_code(child), -1);
        let mut err = MaybeUninit::uninit();
        velt_rt_child_kill(child, libc::SIGTERM, err.as_mut_ptr());
        assert_eq!(err.assume_init().code, 0);
        let mut status = super::CHILDREN.get(child).expect("open").status.clone();
        crate::task::runtime::runtime().block_on(async {
            status
                .wait_for(Option::is_some)
                .await
                .expect("reaper publishes");
        });
        assert_eq!(velt_rt_child_exit_code(child), 128 + libc::SIGTERM as i64);
        velt_rt_child_close(child);
    }
}

/// A fresh directory for one test (its mode decides what the lookup may see in it).
#[cfg(unix)]
fn test_dir(name: &str, mode: u32) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("velt-child-{name}-{}", std::process::id()));
    let _ = std::fs::create_dir(&dir);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
    dir
}

#[cfg(unix)]
fn remove_test_dir(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    let _ = std::fs::remove_dir(dir);
}

/// A program missing from every `PATH` entry is `ENOENT`. The lookup uses the `PATH` given to
/// the child, as in Node, so the test doesn't depend on the machine's `PATH` (an entry the
/// lookup may not search would make it `EACCES`, see below).
#[cfg(unix)]
#[test]
fn missing_program_is_enoent() {
    let empty = test_dir("empty", 0o700);
    let path = format!("{}:/velt-no-such-dir", empty.display());
    let missing = spec_env("velt-no-such-program", &[], 0, &[("PATH", &path)]);
    let missing = output_sync(&missing, &VeltStr::empty());
    remove_test_dir(&empty);
    let (code, message) = missing.unwrap_err();
    assert_eq!(code, crate::result::code::NOT_FOUND, "{message}");
    assert!(
        message.starts_with("spawn velt-no-such-program: "),
        "{message}"
    );
}

/// A `PATH` entry the lookup may not search makes a missing program `EACCES`, as in Node
/// (`spawn x EACCES`: the lookup, like `execvp`, remembers the denied entry), and a program in
/// a later entry is still found.
#[cfg(unix)]
#[test]
fn unsearchable_path_entry_is_eacces_like_node() {
    // SAFETY: no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return; // root may search any directory
    }
    let closed = test_dir("closed", 0o000);
    let empty = test_dir("closed-empty", 0o700);
    let path = format!("{}:{}", closed.display(), empty.display());
    let missing = spec_env("velt-no-such-program", &[], 0, &[("PATH", &path)]);
    let missing = output_sync(&missing, &VeltStr::empty());
    let path = format!("{}:/bin:/usr/bin", closed.display());
    let found = spec_env("sh", &["-c", "exit 4"], 0, &[("PATH", &path)]);
    let found = output_sync(&found, &VeltStr::empty());
    remove_test_dir(&closed);
    remove_test_dir(&empty);
    let (code, message) = missing.unwrap_err();
    assert_eq!(code, crate::result::code::PERMISSION_DENIED, "{message}");
    assert!(
        message.starts_with("spawn velt-no-such-program: "),
        "{message}"
    );
    assert_eq!(found, Ok((4, String::new(), String::new())));
}
