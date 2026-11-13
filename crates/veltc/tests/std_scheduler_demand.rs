//! With `VELT_SCHEDULER=std`, only a program that runs something on the executor in Velt has it
//! (#2649 P3(a), velt_sema `lazy_std` `sched_hooks`): async code anywhere it reaches (also through
//! a function value, or inside std behind a synchronous API), or an extern the executor serves
//! (timers, microtasks, `new Promise`'s latch). A program without such code is built as without
//! the variable: no executor, and its module state through the runtime's entries. Each program
//! must print what Node prints.

use std::path::Path;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

/// `src` built with `VELT_SCHEDULER=std` and run: its stdout, and its binary.
fn build_and_run(dir: &Path, name: &str, src: &str) -> (String, Vec<u8>) {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!(
        "{name}_std{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = command(&exe)
        .env("VELT_THREADS", "1")
        .env_remove("VELT_SCHEDULER")
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{name}: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    (out, std::fs::read(&exe).unwrap())
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Builds `src`: it prints `want`, and has the executor in Velt exactly when `executor` (and its module lock when `module_lock`).
fn check(name: &str, src: &str, want: &str, executor: bool, module_lock: bool) {
    // The executor in Velt waits in an epoll or kqueue reactor; Windows has none yet.
    if !cfg!(unix) {
        return;
    }
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let (std, std_bin) = build_and_run(dir.path(), name, src);
    assert_eq!(std, want, "{name}");
    assert_eq!(
        contains(&std_bin, b"schedBlockOn"),
        executor,
        "{name}: the executor in Velt is in the std build"
    );
    assert_eq!(
        contains(&std_bin, b"schedModuleState"),
        module_lock,
        "{name}: module state through the executor's lock"
    );
}

#[test]
fn a_program_without_tasks_has_no_executor() {
    let src = "function main() {\n  console.log(\"hello\");\n}\n";
    check("plain", src, "hello\n", false, false);
}

#[test]
fn module_state_without_tasks_goes_through_the_runtime() {
    let src = "let count = 0;\nfunction bump(): number {\n  count += 1;\n  return count;\n}\n\
               function main() {\n  bump();\n  console.log(bump(), count);\n}\n";
    check("state", src, "2 2\n", false, false);
}

#[test]
fn a_synchronous_api_that_starts_promises_runs_on_the_executor() {
    // `main` is synchronous and awaits nothing; std's promise and timer code behind it does run
    // on the executor (microtasks, a timer).
    let src = "function main() {\n  Promise.resolve(1).then((v) => console.log(\"then\", v));\n  \
               setTimeout(() => console.log(\"timer\"), 1);\n  console.log(\"main\");\n}\n";
    check("sync_api", src, "main\nthen 1\ntimer\n", true, false);
}

#[test]
fn async_code_reached_only_through_a_function_value_runs_on_the_executor() {
    let src = "async function hidden(): Promise<string> {\n  return \"via a value\";\n}\n\
               function pick(): () => Promise<string> {\n  return hidden;\n}\n\
               function main() {\n  const f = pick();\n  f().then((s) => console.log(s));\n}\n";
    check("fn_value", src, "via a value\n", true, false);
}

#[test]
fn module_state_with_tasks_takes_the_executors_lock() {
    let src = "let count = 0;\nasync function add(n: number): Promise<number> {\n  await null;\n  \
               count += n;\n  return count;\n}\nasync function main() {\n  console.log(await add(2));\n}\n";
    check("state_tasks", src, "2\n", true, true);
}

/// The VIR of `files` (name, source; the first is the root), built with `VELT_SCHEDULER=std`.
fn vir_of(files: &[(&str, &str)]) -> String {
    let dir = test_dir::TestDir::new();
    for (name, src) in files {
        std::fs::write(dir.path().join(name), src).unwrap();
    }
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", files[0].0, "--emit", "vir"])
        .current_dir(dir.path())
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// A module initializer that arms a timer puts the program on the executor: every access of
/// module state then goes through the executor's module lock (its three hooks), none through the
/// runtime's entries, though `main` itself runs nothing on it. Without the variable, module state
/// is reached as before the executor in Velt, with no access functions in between.
#[test]
fn module_state_takes_one_path_decided_after_the_initializers() {
    let dep = "export let count = 0;\nfunction arm(): number {\n  setTimeout(() => {\n    count += 1;\n  }, 1);\n  return 2;\n}\nexport const armed = arm();\n";
    let main = "import { count, armed } from \"./dep\";\nlet local = 1;\nfunction main() {\n  local += armed;\n  console.log(\"main\", local, count);\n}\n";
    let files = [("main.velt", main), ("dep.velt", dep)];
    let std = vir_of(&files);
    for hook in ["schedModuleState", "schedModuleBlock", "schedModuleReady"] {
        assert!(
            std.contains(hook),
            "the executor's {hook} is not used:\n{std}"
        );
    }
    let runtime = |vir: &str| {
        let helpers = vir.split("\nfn#").filter(|f| f.contains("_Gmodule_"));
        helpers.filter(|f| f.contains("velt_rt_module_")).count()
    };
    assert_eq!(
        runtime(&std),
        0,
        "an access function calls the runtime's entry"
    );
}
