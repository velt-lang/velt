//! A class table holds clone, format and transfer glue only for the operations the program
//! performs on objects of that class through their header (velt_vir `glue/slot_uses.rs`): a
//! hierarchy that is never logged, cloned or sent compiles none of the three.

mod no_window;
mod test_dir;

/// The VIR of `src`, compiled as `main.vlt`.
fn vir_of(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.vlt", "--emit", "vir"])
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

/// The functions of `vir` whose symbol starts with `prefix` and names `class`.
fn glue(vir: &str, prefix: &str, class: &str) -> usize {
    vir.lines()
        .filter(|l| l.starts_with("fn#"))
        .filter(|l| l.contains(prefix) && l.contains(class))
        .count()
}

const HIERARCHY: &str = "
class Animal {
  name: string;
  constructor(name: string) {
    this.name = name;
  }
  sound(): string {
    return \"...\";
  }
}

class Dog extends Animal {
  override sound(): string {
    return \"woof\";
  }
}

function pick(i: number): Animal {
  return i > 0 ? new Dog(\"rex\") : new Animal(\"generic\");
}
";

#[test]
fn unused_operations_have_no_glue() {
    let vir = vir_of(&format!(
        "{HIERARCHY}\nconsole.log(pick(1).sound() + pick(0).sound());\n"
    ));
    for prefix in ["_Gobjformat", "_Gobjclone", "_Gobjtransfer"] {
        assert_eq!(glue(&vir, prefix, "Dog"), 0, "{prefix} Dog");
        assert_eq!(glue(&vir, prefix, "Animal"), 0, "{prefix} Animal");
    }
    // Drop glue stays: every object is dropped through its table.
    assert!(glue(&vir, "_Gobjdrop", "Dog") > 0);
}

/// Logging a base-class value reaches the subclasses' format glue (the object may be a `Dog`),
/// and nothing else.
#[test]
fn a_logged_base_keeps_its_subclasses_format_glue() {
    let vir = vir_of(&format!("{HIERARCHY}\nconsole.log(pick(1));\n"));
    assert!(glue(&vir, "_Gobjformat", "Dog") > 0);
    assert!(glue(&vir, "_Gobjformat", "Animal") > 0);
    assert_eq!(glue(&vir, "_Gobjclone", "Dog"), 0);
    assert_eq!(glue(&vir, "_Gobjtransfer", "Dog"), 0);
}

/// Logging a subclass value keeps that subclass's format glue; its base, never logged as such,
/// has none.
#[test]
fn a_logged_subclass_keeps_only_its_own_format_glue() {
    let vir = vir_of(&format!(
        "{HIERARCHY}\nconst d: Dog = new Dog(\"rex\");\nconsole.log(d);\nconsole.log(pick(0).sound());\n"
    ));
    assert!(glue(&vir, "_Gobjformat", "Dog") > 0);
    assert_eq!(glue(&vir, "_Gobjformat", "Animal"), 0);
}
