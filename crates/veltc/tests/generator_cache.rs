//! The output of source generators is cached (docs/internals/design/source-generators.md §9):
//! a second build reuses it, and a change to the generator's module invalidates it.

use std::path::Path;
use std::process::Command;

fn run(dir: &Path, cache: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_velt"))
        .arg("run")
        .arg(dir.join("main.vlt"))
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

fn generator(text: &str) -> String {
    format!(
        "import {{ Request, Output }} from \"velt/generate\";\n\
         export function hello(req: Request, out: Output): void {{\n\
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
