//! A generated function declared in a std module the check loads on demand (velt_common
//! `ON_DEMAND_MODULES`, docs/internals/design/lazy-std.md "Loading on demand"): its generator
//! runs and its code is checked like that of a module the program loaded (#3807). No std module
//! is like that today, so std is copied to a temp directory with the hook module of `**`
//! (std/lang/hooks/pow) replaced by one that calls a generated function, and `velt run` uses it
//! through `$VELT_STD`. A build that takes longer than [`LIMIT`] is stopped and fails: the driver
//! used not to find the module, generated nothing, and the check asked again for ever.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

mod no_window;
mod test_dir;

/// How long one `velt run` may take (it takes about a second).
const LIMIT: Duration = Duration::from_secs(120);

/// The hook module of `**` on floats, calling a generated function of its own.
const HOOK: &str = r#"import { jsPow } from "velt/lang/math/pow";

export declare generated("velt/lang/hooks/pow_gen") function powName<T>(value: T): string;

export function powF64(x: f64, y: f64): f64 {
  return powName<f64>(x) == "f64" ? jsPow(x, y) : 0.0;
}
"#;

/// Its generator: the instance's type as text.
const GENERATOR: &str = r#"import { InstanceRequest, Output } from "velt/generate";

export function powName(req: InstanceRequest, out: Output): void {
  out.body(`return ${out.literal(req.model.type(req.args[0]).display)};`);
}
"#;

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            copy_tree(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}

/// `velt run main.velt` (`main` its source) with the std of `dir`, made on the first call: its
/// output. Panics when the run fails or takes longer than [`LIMIT`].
fn run(dir: &test_dir::TestDir, main: &str) -> String {
    let std: PathBuf = dir.path().join("std");
    if !std.exists() {
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
            &std,
        );
        std::fs::write(std.join("lang/hooks/pow.velt"), HOOK).unwrap();
        std::fs::write(std.join("lang/hooks/pow_gen.velt"), GENERATOR).unwrap();
    }
    let file = dir.path().join("main.velt");
    std::fs::write(&file, main).unwrap();
    let mut child = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .arg("run")
        .arg(&file)
        .env("VELT_STD", &std)
        .env("VELT_BUILD_CACHE", dir.path().join("cache"))
        // Each run of the generators writes a module here.
        .env("VELT_EMIT_GENERATED", dir.path().join("generated"))
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Read on threads, so a full pipe never blocks the build.
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text);
            text
        })
    };
    let out = drain(Box::new(child.stdout.take().unwrap()));
    let err = drain(Box::new(child.stderr.take().unwrap()));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        // timing: a hang guard, not an order: a run that ends in time is the result.
        if start.elapsed() > LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`velt run` was still running after {LIMIT:?}");
        }
        // timing: polls for the exit the hang guard above bounds.
        std::thread::sleep(Duration::from_millis(20));
    };
    let (out, err) = (out.join().unwrap(), err.join().unwrap());
    assert!(status.success(), "{err}");
    out
}

/// How many modules the generators wrote for the builds in `dir` so far.
fn generated(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    (entries.flatten())
        .map(|e| match e.path() {
            p if p.is_dir() => generated(&p),
            _ => 1,
        })
        .sum()
}

/// The generator of a module loaded on demand runs once the module is there, in the first build
/// and in one that starts from what the first cached.
#[test]
fn a_generated_function_of_a_module_loaded_on_demand_gets_its_code() {
    let dir = test_dir::TestDir::new();
    let main = "function f(x: number, y: number): number {\n  return x ** y;\n}\n\
                console.log(f(2.5, 2));\n";
    assert_eq!(run(&dir, main), "6.25\n");
    assert_eq!(run(&dir, main), "6.25\n");
    // A changed program: the code generated for the last build is offered to this one.
    let more = format!("{main}console.log(f(3, 2));\n");
    assert_eq!(run(&dir, &more), "6.25\n9\n");
}

/// A check that runs a second round (here for a thrown type known too late) starts it without
/// the module, which that round loads on demand again: the code generated for it in the first
/// round waits for it, and the generator is not run a second time.
#[test]
fn its_code_waits_for_the_module_in_a_later_round() {
    let dir = test_dir::TestDir::new();
    let main = "interface Risky {\n  run(n: number): void throws RangeError | TypeError;\n}\n\
                class Calm implements Risky {\n  run(n: number): void {}\n}\n\
                function f(x: number, y: number): number {\n  return x ** y;\n}\n\
                new Calm().run(1);\n\
                console.log(f(2.5, 2));\n";
    assert_eq!(run(&dir, main), "6.25\n");
    assert_eq!(generated(&dir.path().join("generated")), 1);
}
