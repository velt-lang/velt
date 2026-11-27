//! Every program for x86-64 and arm64 runs std's start-up check of the CPU first (rt_abi.md "CPU
//! check", std/lang/hooks/cpu.velt), so every program's code carries it. It is kept small: the
//! normal path tests the feature word and whether each variable is set, and the message and the
//! variables' parsing are out of line, with the feature names as data (std/lang/cpu/check.velt).
//! Hello world went from 133 lines of release IR to 8,795 with the check's first version; the
//! budget below keeps it from growing back unnoticed.

mod no_window;
mod test_dir;

/// Lines of release IR for hello world, per target: 2,209 to 2,457 when the budget was set (133
/// before the start-up check); 7,857 to 9,386 with std's writer of stdout and stderr (#2196,
/// std/lang/hooks/stdout_buffer.velt and the files behind `#stdout-sys`), which every program
/// compiles where it linked the runtime's: Windows' the most, with its console's UTF-16 writes.
const HELLO_BUDGET: usize = 9_600;

#[test]
fn hello_world_with_the_start_up_check_stays_within_its_budget() {
    let dir = test_dir::TestDir::new();
    let src = "function main() {\n  console.log(\"hi\");\n}\n";
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    for target in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ] {
        let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
            .args([
                "build",
                "main.velt",
                "--release",
                "--emit",
                "llvm",
                "--target",
                target,
            ])
            .current_dir(dir.path())
            .env_remove("VELT_CPU")
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{target}: {err}");
        let lines = String::from_utf8_lossy(&out.stdout).lines().count();
        assert!(
            lines <= HELLO_BUDGET,
            "{target}: hello world is {lines} lines of release IR, over its budget of {HELLO_BUDGET}"
        );
    }
}
