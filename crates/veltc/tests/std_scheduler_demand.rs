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
    vir_with(files, &[])
}

/// [`vir_of`] with the variables `env` set.
fn vir_with(files: &[(&str, &str)], env: &[(&str, &str)]) -> String {
    let dir = test_dir::TestDir::new();
    for (name, src) in files {
        std::fs::write(dir.path().join(name), src).unwrap();
    }
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", files[0].0, "--emit", "vir"])
        .current_dir(dir.path())
        .env("VELT_SCHEDULER", "std")
        .env_remove("VELT_EAGER_STD")
        .env_remove("VELT_CHECK_ALL_STD")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{env:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
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

/// The module-state path is decided once `velt_main` is built, not while `build_main` builds what
/// it queued (#2880's review). Here `main` touches no module state, so the module initializers
/// are first lowered in `build_main` (`init_module_state`), and only they reach `read`, which
/// reads module state and then calls `arm`, which arms the program's only timer. Built as
/// `build_main`'s queue empties, `read`'s access came before `arm` and took the runtime's entry,
/// and the timer's code the executor's.
#[test]
fn state_read_by_code_only_an_initializer_reaches_takes_one_path() {
    let dep = "export let count = 1;\nfunction arm() {\n  \
               setTimeout(() => console.log(\"timer\"), 1);\n}\n\
               function read(): number {\n  const c = count;\n  arm();\n  return c;\n}\n\
               export const total = read();\n";
    let main = "import \"./dep\";\nfunction main() {\n  console.log(\"main\");\n}\n";
    let std = vir_of(&[("main.velt", main), ("dep.velt", dep)]);
    assert!(
        std.contains("schedModuleState"),
        "the executor's lock is not used:\n{std}"
    );
    let helpers = std.split("\nfn#").filter(|f| f.contains("_Gmodule_"));
    let runtime: Vec<&str> = helpers.filter(|f| f.contains("velt_rt_module_")).collect();
    assert!(
        runtime.is_empty(),
        "an access calls the runtime's entry:\n{}",
        runtime.join("\nfn#")
    );
}

/// A copy of std in `dir`, for a test to change.
fn std_copy(dir: &Path) -> std::path::PathBuf {
    let std = dir.join("std");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        &std,
    );
    std
}

/// `file` built into `exe` over the std root `std`, with `VELT_SCHEDULER=std`.
fn build_with_std(std: &Path, file: &Path, exe: &Path) -> std::process::Output {
    command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(file)
        .arg("-o")
        .arg(exe)
        .current_dir(file.parent().unwrap())
        .env("VELT_SCHEDULER", "std")
        .env("VELT_STD", std)
        .env("VELT_BUILD_CACHE", "off")
        .env("VELT_BUILD_STAMP", "off")
        .output()
        .unwrap()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let (src, dst) = (e.path(), to.join(e.file_name()));
        match e.file_type().unwrap().is_dir() {
            true => copy_tree(&src, &dst),
            false => _ = std::fs::copy(&src, &dst).unwrap(),
        }
    }
}

/// `velt/fs`'s synchronous calls load none of the executor's modules (#2882): its `std-scheduler`
/// variant calls the blocking pool by role (`hookCall`), and the call names the pool only once
/// live code reaches it (velt_sema `lazy_std/hook_stubs.rs`), not when a body is checked. So the
/// program builds and runs with every `lang/hooks/sched*` module gone from std. Importing the
/// pool's module instead made `velt check` of such a program load 47 more modules (+56 % on
/// notes-cli).
#[test]
fn synchronous_fs_calls_load_no_executor_module() {
    if !cfg!(unix) {
        return;
    }
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let std = std_copy(dir.path());
    let hooks = std.join("lang/hooks");
    let mut removed = 0;
    for e in std::fs::read_dir(&hooks).unwrap() {
        let name = e.unwrap().file_name().into_string().unwrap();
        if name.starts_with("sched") && name.ends_with(".velt") {
            std::fs::remove_file(hooks.join(&name)).unwrap();
            removed += 1;
        }
    }
    assert!(removed > 30, "{removed} executor modules");
    let file = dir.path().join("sync_fs.velt");
    let data = dir.path().join("data.txt");
    std::fs::write(&data, "four").unwrap();
    let src = format!(
        "import {{ existsSync, readFileSync, writeFileSync }} from \"velt/fs\";\n\
         function main() {{\n  const p = {:?};\n  writeFileSync(p + \".out\", readFileSync(p) + \"!\");\n  \
         console.log(existsSync(p), readFileSync(p + \".out\"));\n}}\n",
        data.to_str().unwrap()
    );
    std::fs::write(&file, src).unwrap();
    let exe = dir.path().join(if cfg!(windows) {
        "sync_fs.exe"
    } else {
        "sync_fs"
    });
    let o = build_with_std(&std, &file, &exe);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = command(&exe).env_remove("VELT_SCHEDULER").output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    assert_eq!((o.status.code(), out.as_str()), (Some(0), "true four!\n"));
}

/// A hook called by role is typed from velt_common's table until live code reaches it; then its
/// own definition must have the table's signature (#2882). A pool whose `run` drifted from the
/// table (one more parameter) is an ICE naming the hook, not a call checked against the wrong
/// signature.
#[test]
fn a_hook_called_by_role_that_drifted_from_the_table_is_an_ice() {
    if !cfg!(unix) {
        return;
    }
    let dir = test_dir::TestDir::new();
    let std = std_copy(dir.path());
    let pool = std.join("lang/hooks/sched_blocking.velt");
    let text = std::fs::read_to_string(&pool).unwrap();
    let decl = "export async function run<T>(work: () => T): Promise<T> {";
    assert!(
        text.contains(decl),
        "the pool's `run` changed: update this test"
    );
    let drifted = "export async function run<T>(work: () => T, spare?: number): Promise<T> {";
    std::fs::write(&pool, text.replace(decl, drifted)).unwrap();
    let file = dir.path().join("async_fs.velt");
    let src = "import { readFile } from \"velt/fs\";\n\
               async function main() {\n  console.log((await readFile(\"missing\")).length);\n}\n";
    std::fs::write(&file, src).unwrap();
    let o = build_with_std(&std, &file, &dir.path().join("async_fs"));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "built with a drifted hook");
    assert!(
        err.contains("ICE: the hook `sched_blocking_run`") && err.contains("2 parameters"),
        "{err}"
    );
}

/// Without std's modules on demand (`VELT_EAGER_STD`, `VELT_CHECK_ALL_STD`, the editor's
/// analysis), the loader loads the executor's blocking pool up front with the executor, as
/// `velt/fs` calls it by role (#2882): its programs build under `VELT_SCHEDULER=std` with every
/// std module loaded, and give the VIR they give on demand (cli.md: "byte for byte the same").
#[test]
fn fs_programs_build_the_same_with_std_up_front() {
    let sync = "import { existsSync } from \"velt/fs\";\nfunction main() {\n  \
                console.log(existsSync(\"nothing here\"));\n}\n";
    let async_fs = "import { readFile } from \"velt/fs\";\nasync function main() {\n  \
                     try {\n    await readFile(\"nothing here\");\n  } catch (e) {\n    \
                     console.log(\"missing\");\n  }\n}\n";
    for src in [sync, async_fs] {
        let files = [("main.velt", src)];
        let on_demand = vir_of(&files);
        let eager = vir_with(&files, &[("VELT_EAGER_STD", "1")]);
        assert!(on_demand == eager, "std up front gives another VIR:\n{src}");
        vir_with(&files, &[("VELT_CHECK_ALL_STD", "1")]);
    }
}
