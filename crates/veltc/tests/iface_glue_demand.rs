//! An interface table holds clone, format, share and transfer glue only once the program
//! dispatches that operation on values of the interface (velt_vir `glue/demand.rs`): an
//! interface whose values only have their methods called compiles none of the four, and each
//! operation the program applies keeps its own.

mod no_window;
mod test_dir;

/// The VIR of `src`, compiled as `main.velt` with `args`.
fn vir_of(src: &str, args: &[&str]) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.velt", "--emit", "vir"])
        .args(args)
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

/// Whether `vir` defines the glue `kind` (`dynclone`, …) of type `name`.
fn has_glue(vir: &str, kind: &str, name: &str) -> bool {
    let symbol = format!("_G{kind}_{}{name}", name.len());
    vir.lines().filter(|l| l.starts_with("fn#")).any(|l| {
        l.split_whitespace()
            .nth(2)
            .is_some_and(|s| s.starts_with(&symbol))
    })
}

const SHAPES: &str = "
interface Shape {
  area(): number;
}

class Circle implements Shape {
  r: number[] = [2];
  area(): number {
    return 3 * this.r[0] * this.r[0];
  }
}

class Square implements Shape {
  s: number[] = [3];
  area(): number {
    return this.s[0] * this.s[0];
  }
}

function pick(i: number): Shape {
  return i > 0 ? new Circle() : new Square();
}

class Holder {
  shape: Shape;
  constructor(shape: Shape) {
    this.shape = shape;
  }
}

function twice<T>(x: T): T[] {
  return [x.clone(), x];
}

async function area(s: Shape): Promise<number> {
  return s.area();
}
";

const OPS: [&str; 4] = ["dynclone", "dynformat", "dynshare", "dyntransfer"];

/// The glue kinds of `OPS` that `vir` has for both implementations.
fn kept(vir: &str) -> Vec<&'static str> {
    let mut out = vec![];
    for kind in OPS {
        let (c, s) = (has_glue(vir, kind, "Circle"), has_glue(vir, kind, "Square"));
        assert_eq!(c, s, "{kind}: one implementation has it, the other not");
        if c {
            out.push(kind);
        }
    }
    out
}

#[test]
fn method_calls_only_keep_no_glue() {
    for mode in [&[][..], &["--release"][..]] {
        let vir = vir_of(
            &format!("{SHAPES}\nconsole.log(pick(1).area() + pick(0).area());\n"),
            mode,
        );
        assert_eq!(kept(&vir), Vec::<&str>::new(), "{mode:?}");
        // Drop glue stays: every interface value is dropped through its table.
        assert!(has_glue(&vir, "dyndrop", "Circle"), "{mode:?}");
    }
}

/// A clone of an interface value through generic code (`T` is `Shape`) keeps the clone glue.
#[test]
fn a_generic_clone_keeps_clone_glue() {
    let vir = vir_of(
        &format!(
            "{SHAPES}\nconst xs = twice(pick(1));\nconsole.log(xs[0].area() + xs[1].area());\n"
        ),
        &[],
    );
    assert!(kept(&vir).contains(&"dynclone"), "{:?}", kept(&vir));
    assert!(!kept(&vir).contains(&"dynformat"));
}

/// Logging a class holding an interface value, through `any`, formats it through its table.
#[test]
fn logging_through_any_keeps_format_glue() {
    let vir = vir_of(
        &format!("{SHAPES}\nconst h: any = new Holder(pick(1));\nconsole.log(h);\n"),
        &[],
    );
    assert!(kept(&vir).contains(&"dynformat"), "{:?}", kept(&vir));
}

/// An interface value going to another task is transferred through its table.
#[test]
fn spawn_keeps_transfer_glue() {
    let vir = vir_of(
        &format!(
            "{SHAPES}\nasync function main(): Promise<void> {{\n  const s = pick(0);\n  console.log(await spawn(area(s)), s.area());\n}}\n"
        ),
        &[],
    );
    assert!(kept(&vir).contains(&"dyntransfer"), "{:?}", kept(&vir));
    assert!(!kept(&vir).contains(&"dynformat"));
}
