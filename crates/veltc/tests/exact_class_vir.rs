//! `isExactClass(this)` (`Intrinsic::IsExactClass`, #3792) is a constant `true` in a program
//! where no class extends the receiver's, and an `if` on it lowers only the branch it takes:
//! std's `RegExp` then compiles as it did before its methods learned to call a subclass's
//! `exec`. With a class that extends `RegExp`, the other branch is there too.
//!
//! The check: the VIR of a program names the functions only an object of a subclass reaches
//! (std/text/regex/exec_calls.velt, and `RegExp`'s `execOwn`) exactly when it has such a class.

mod no_window;
mod test_dir;

/// Every string method that takes a regex, and `test` and `exec`.
const USES: &str = "const s = \"xab a\";
console.log(re.test(s), s.match(re), s.search(re), s.replace(re, \"-\"), s.split(re, 2));
console.log(s.replace(re, (m) => m + m), s.replaceAll(re, \"+\"), re.exec(s)?.index);
console.log([...s.matchAll(re)].length);
";

fn vir(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.velt", "--emit", "vir"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The lines of `vir` that name a function of the route through a subclass's `exec` (a mangled
/// name writes the module exec_calls as `exec__calls`).
fn exec_route(vir: &str) -> Vec<&str> {
    vir.lines()
        .filter(|l| l.contains("exec__calls") || l.contains("execOwn"))
        .collect()
}

#[test]
fn a_program_without_a_regexp_subclass_has_no_exec_route() {
    let v = vir(&format!("const re = /a(b)?/g;\n{USES}"));
    assert!(v.contains("RegExp"), "the program uses `RegExp`");
    let route = exec_route(&v);
    assert!(route.is_empty(), "lowered for no subclass: {route:?}");
}

#[test]
fn a_subclass_of_another_class_changes_nothing_for_regexp() {
    let v = vir(&format!(
        "class Mine extends Error {{}}\nconsole.log(new Mine(\"m\").message);\nconst re = /a(b)?/g;\n{USES}"
    ));
    let route = exec_route(&v);
    assert!(route.is_empty(), "lowered for no subclass: {route:?}");
}

#[test]
fn a_regexp_subclass_gets_the_exec_route() {
    let v = vir(&format!(
        "class Mine extends RegExp {{}}\nconst re = new Mine(\"a(b)?\", \"g\");\n{USES}"
    ));
    assert!(v.contains("execOwn"), "`exec` of a subclass's object");
    assert!(v.contains("exec__calls"), "the specification's steps");
}
