//! The output of source generators is cached (docs/internals/design/source-generators.md §9):
//! a second build reuses it, and a change to the generator's module invalidates it.

use std::path::Path;
use std::process::Command;

fn run(dir: &Path, cache: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_velt"))
        .arg("run")
        .arg(dir.join("main.vlt"))
        // Its own `target/velt`: the tests run at once.
        .current_dir(dir)
        .env("VELT_BUILD_CACHE", cache)
        .output()
        .expect("run velt");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `velt run main.vlt` in `dir` (the path as typed: relative): its output, or its errors.
fn run_relative(dir: &Path, cache: &Path) -> Result<String, String> {
    let out = Command::new(env!("CARGO_BIN_EXE_velt"))
        .args(["run", "main.vlt"])
        .current_dir(dir)
        .env("VELT_BUILD_CACHE", cache)
        .output()
        .expect("run velt");
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        false => Err(String::from_utf8_lossy(&out.stderr).into_owned()),
    }
}

fn generator(text: &str) -> String {
    format!(
        "import {{ InstanceRequest, Output }} from \"velt/generate\";\n\
         export function hello(req: InstanceRequest, out: Output): void {{\n\
         \x20 out.line(`return ${{out.literal(\"{text} \" + req.model.type(req.args[0]).display)}};`);\n\
         }}\n"
    )
}

#[test]
fn a_changed_generator_is_run_again() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("prog");
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        dir.join("main.vlt"),
        "declare generated(\"./gen\") function hello<T>(): string;\n\
         class User {}\n\
         function main() { console.log(hello<User>()); }\n",
    )
    .expect("main");
    std::fs::write(dir.join("gen.vlt"), generator("hello")).expect("gen");
    assert_eq!(run(&dir, &cache), "hello User\n");
    // A cached result: the same output.
    assert_eq!(run(&dir, &cache), "hello User\n");
    // The generator changed: its output must not come from the cache.
    std::fs::write(dir.join("gen.vlt"), generator("goodbye")).expect("gen");
    assert_eq!(run(&dir, &cache), "goodbye User\n");
}

/// Programs run by the same relative path (`velt run main.vlt`) from their own directories are
/// different programs to the caches: each gets its own generators' output, and a generator that
/// reads the clock is rejected as when run by its full path.
#[test]
fn programs_at_the_same_relative_path_have_their_own_generators() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let cache = tmp.path().join("cache");
    let main = "declare generated(\"./gen\") function hello<T>(): string;
                class User {}
                function main() { console.log(hello<User>()); }
";
    for (name, text) in [("a", "hello"), ("b", "goodbye"), ("c", "welcome")] {
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("main.vlt"), main).expect("main");
        std::fs::write(dir.join("gen.vlt"), generator(text)).expect("gen");
        assert_eq!(
            run_relative(&dir, &cache),
            Ok(format!(
                "{text} User
"
            ))
        );
        // Cached: still this program's.
        assert_eq!(
            run_relative(&dir, &cache),
            Ok(format!(
                "{text} User
"
            ))
        );
    }
    let dir = tmp.path().join("clock");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("main.vlt"), main).expect("main");
    std::fs::write(
        dir.join("gen.vlt"),
        "import { InstanceRequest, Output } from \"velt/generate\";
         export function hello(req: InstanceRequest, out: Output): void {
           out.line(`return ${out.literal(Date.now().toString())};`);
         }
",
    )
    .expect("gen");
    for _ in 0..2 {
        let err = run_relative(&dir, &cache).expect_err("a generator reading the clock");
        assert!(err.contains("cannot reach the clock"), "{err}");
    }
}

/// The generator module resolves to another file (`./gen` was `gen/index.vlt`; a `gen.vlt` added
/// next to it wins): the cached output and the offered code are stale.
#[test]
fn a_generator_that_resolves_to_another_file_is_run_again() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("prog");
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(dir.join("gen")).expect("dir");
    std::fs::write(
        dir.join("main.vlt"),
        "declare generated(\"./gen\") function hello<T>(): string;\n\
         class User {}\n\
         function main() { console.log(hello<User>()); }\n",
    )
    .expect("main");
    std::fs::write(dir.join("gen").join("index.vlt"), generator("hello")).expect("gen");
    assert_eq!(run(&dir, &cache), "hello User\n");
    assert_eq!(run(&dir, &cache), "hello User\n");
    std::fs::write(dir.join("gen.vlt"), generator("goodbye")).expect("gen");
    assert_eq!(run(&dir, &cache), "goodbye User\n");
}

/// An instance a generator refuses (`JSON.stringify` of a `Map<i32, i32>`) is reported once,
/// also when later rounds of generating (generic methods reached through an interface) ask
/// for it again.
#[test]
fn a_refused_instance_is_reported_once() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let main = tmp.path().join("main.vlt");
    std::fs::write(
        &main,
        "class Client {\n\
         \x20 async query<P>(sql: string, params: P): Promise<string> { return sql + JSON.stringify(params); }\n\
         }\n\
         interface Q { query<P>(sql: string, params: P): Promise<string>; }\n\
         function f(m: Map<i32, i32>) { console.log(JSON.stringify(m)); }\n\
         async function main() {\n\
         \x20 const c = new Client();\n\
         \x20 console.log(await c.query(\"x\", [1]));\n\
         \x20 const q: Q = c;\n\
         \x20 console.log(await q.query(\"y\", \"s\"));\n\
         }\n",
    )
    .expect("main");
    let out = Command::new(env!("CARGO_BIN_EXE_velt"))
        .arg("check")
        .arg(&main)
        .current_dir(tmp.path())
        .env("VELT_BUILD_CACHE", tmp.path().join("cache"))
        .output()
        .expect("run velt");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let reports = stderr.matches("`Map<i32, i32>` has no JSON form").count();
    assert_eq!(reports, 1, "{stderr}");
}

/// A generator that lists a type's fields: its output changes when the type does.
const FIELDS: &str = "import { InstanceRequest, Output } from \"velt/generate\";\n\
     export function fields(req: InstanceRequest, out: Output): void {\n\
     \x20 const t = req.model.type(req.args[0]);\n\
     \x20 const names: string[] = [];\n\
     \x20 for (const f of t.fields) { names.push(f.name); }\n\
     \x20 out.line(`return ${out.literal(t.display + \": \" + names.join(\",\"))};`);\n\
     }\n";

fn program(types: &str, uses: &str) -> String {
    format!(
        "declare generated(\"./gen\") function fields<T>(): string;\n\
         {types}\n\
         function main() {{ {uses} }}\n"
    )
}

/// The second build is offered the first one's generated code: it is used only while the types
/// it was generated for are unchanged (their fingerprints), and dropped without a word when it
/// no longer compiles.
#[test]
fn offered_code_is_used_only_while_it_fits() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("prog");
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("gen.vlt"), FIELDS).expect("gen");
    let main = dir.join("main.vlt");
    let write = |text: String| std::fs::write(&main, text).expect("main");
    write(program(
        "class User { name = \"\"; }\nclass Admin { level = 1; }",
        "console.log(fields<User>()); console.log(fields<Admin>());",
    ));
    assert_eq!(run(&dir, &cache), "User: name\nAdmin: level\n");
    // A field added: the offered code for `User` still compiles, but is stale.
    write(program(
        "class User { name = \"\"; age = 0; }\nclass Admin { level = 1; }",
        "console.log(fields<User>()); console.log(fields<Admin>());",
    ));
    assert_eq!(run(&dir, &cache), "User: name,age\nAdmin: level\n");
    // `Admin` gone: the offered code that names it does not compile, and is not reported.
    write(program(
        "class User { name = \"\"; age = 0; }",
        "console.log(fields<User>());",
    ));
    assert_eq!(run(&dir, &cache), "User: name,age\n");
    // A new instance the offer does not hold.
    write(program(
        "class User { name = \"\"; age = 0; }\nclass Team { size = 2; }",
        "console.log(fields<User>()); console.log(fields<Team>());",
    ));
    assert_eq!(run(&dir, &cache), "User: name,age\nTeam: size\n");
}

/// Instances that need code are generated during the check that finds them, and their code is
/// added to that check: a program with generated functions is checked once, as one without
/// (docs/internals/design/source-generators.md "As built so far").
#[test]
fn generated_code_is_added_to_the_check_that_needs_it() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("prog");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("gen.vlt"), FIELDS).expect("gen");
    std::fs::write(
        dir.join("main.vlt"),
        program(
            "class User { name = \"\"; }",
            "console.log(fields<User>()); console.log(fields<User[]>());",
        ),
    )
    .expect("main");
    let out = Command::new(env!("CARGO_BIN_EXE_velt"))
        .args(["check", "-v"])
        .arg(dir.join("main.vlt"))
        .current_dir(&dir)
        .env("VELT_BUILD_CACHE", tmp.path().join("cache"))
        .output()
        .expect("run velt");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let checks = stderr
        .lines()
        .filter(|l| l.starts_with("velt: sema"))
        .count();
    assert_eq!(
        checks, 1,
        "one check, the generators run inside it:\n{stderr}"
    );
    assert!(
        stderr.lines().any(|l| l.starts_with("velt: generate")),
        "{stderr}"
    );
}
