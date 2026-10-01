//! Child-process ABI tests (Unix shells; the Windows equivalents are covered by the goldens).

use super::command::VeltCommand;
use super::output::{velt_rt_child_output_sync, VeltOutput};
use super::*;
use crate::bytes::VeltBytes;
use crate::str_array::VeltStrArray;
use std::mem::MaybeUninit;

fn spec(program: &'static str, args: &[&'static str], stdio: u32) -> VeltCommand {
    VeltCommand {
        program: VeltStr::from_static(program.as_bytes()),
        args: VeltStrArray::from_vec(
            args.iter()
                .map(|a| VeltStr::from_static(a.as_bytes()))
                .collect(),
        ),
        cwd: VeltStr::empty(),
        env: VeltStrArray::from_vec(vec![
            VeltStr::from_static(b"VELT_T"),
            VeltStr::from_static(b"42"),
        ]),
        stdio,
        clear_env: 0,
    }
}

unsafe fn text(s: &VeltStr) -> String {
    String::from_utf8_lossy(s.as_bytes()).into_owned()
}

fn output_sync(
    spec: &VeltCommand,
    input: &'static [u8],
) -> Result<(i32, String, String), (i32, String)> {
    let input = VeltBytes::from_vec(input.to_vec());
    let mut out = MaybeUninit::<IoResult<VeltOutput>>::uninit();
    // SAFETY: valid spec and buffers; results are read according to the code.
    unsafe {
        velt_rt_child_output_sync(spec, &input, out.as_mut_ptr());
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
    assert_eq!(
        output_sync(&s, b"input\n"),
        Ok((3, "input\nenv=42\n".to_string(), "oops\n".to_string()))
    );
    let missing = spec("velt-no-such-program", &[], 0);
    let (code, message) = output_sync(&missing, b"").unwrap_err();
    assert_eq!(code, crate::result::code::NOT_FOUND);
    assert!(
        message.starts_with("spawn velt-no-such-program: "),
        "{message}"
    );
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
        crate::task::runtime::handle().block_on(async {
            status
                .wait_for(Option::is_some)
                .await
                .expect("reaper publishes");
        });
        assert_eq!(velt_rt_child_exit_code(child), 128 + libc::SIGTERM as i64);
        velt_rt_child_close(child);
    }
}
