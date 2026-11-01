//! `velt test`'s `toEqual` on values held as an interface or as a base class (#1882): by the
//! fields of their objects' own class, as Vitest compares them, with cycles stopped by the
//! same rule. Vitest 3.2.4 gives the same results except where the two classes differ (`C`
//! and `D` with the same fields, an `Animal` and a `Cat` with the same fields): Vitest compares
//! their fields by name and passes, Velt calls objects of different classes unequal, as
//! Vitest's `toStrictEqual` does.

use std::path::Path;
use std::process::Output;

mod no_window;
mod runtime_support;
mod test_dir;

fn velt_test(dir: &Path, args: &[&str]) -> Output {
    let mut command = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    command
        .arg("test")
        .args(args)
        .current_dir(dir)
        .env("VELT_HOME", dir.join("home"));
    command.output().unwrap()
}

const INTERFACES: &str = r#"import { describe, expect, it } from "velt/test";

interface I {
  v: number;
  next: I | null;
  get(): number;
}
class C implements I {
  next: I | null = null;
  constructor(public v: number) {}
  get(): number {
    return this.v;
  }
}
class D implements I {
  next: I | null = null;
  constructor(public v: number) {}
  get(): number {
    return this.v;
  }
}
interface Shape {
  area(): number;
}
class Sq implements Shape {
  constructor(public s: number) {}
  area(): number {
    return this.s * this.s;
  }
}
class Animal implements Named {
  constructor(public name: string) {}
  speak(): string {
    return this.name;
  }
}
class Dog extends Animal {
  constructor(name: string, public bark: number) {
    super(name);
  }
  override speak(): string {
    return "woof";
  }
}
class Cat extends Animal {
  constructor(name: string) {
    super(name);
  }
  override speak(): string {
    return "meow";
  }
}
interface Named {
  speak(): string;
}
class Box<T> implements Named {
  constructor(public t: T) {}
  speak(): string {
    return "box";
  }
}

describe("interfaces", () => {
  it("same class by fields", () => {
    const a: I = new C(1);
    const b: I = new C(1);
    expect(a).toEqual(b);
  });
  it("fields differ", () => {
    const a: I = new C(1);
    const b: I = new C(2);
    expect(a).toEqual(b);
  });
  it("classes differ", () => {
    const a: I = new C(1);
    const b: I = new D(1);
    expect(a).toEqual(b);
  });
  it("arrays of interface values", () => {
    const xs: Shape[] = [new Sq(2), new Sq(3)];
    const ys: Shape[] = [new Sq(2), new Sq(3)];
    expect(xs).toEqual(ys);
    expect(xs).not.toEqual([new Sq(2), new Sq(4)] as Shape[]);
  });
  it("interface cycles", () => {
    const a = new C(1);
    a.next = a;
    const b = new C(1);
    b.next = b;
    const x: I = a;
    const y: I = b;
    expect(x).toEqual(y);
  });
  it("interface rings of two lengths", () => {
    const a = new C(1);
    a.next = a;
    const b = new C(1);
    const c = new C(1);
    b.next = c;
    c.next = b;
    const x: I = a;
    const y: I = b;
    expect(x).toEqual(y);
  });
  it("subclasses by their own fields", () => {
    const a: Animal = new Dog("rex", 1);
    const b: Animal = new Dog("rex", 1);
    expect(a).toEqual(b);
    expect(a).not.toEqual(new Dog("rex", 2) as Animal);
    expect(a).not.toEqual(new Cat("rex") as Animal);
    expect(new Animal("rex")).not.toEqual(new Cat("rex") as Animal);
  });
  it("subclasses through an interface", () => {
    const a: Named = new Dog("rex", 1);
    const b: Named = new Dog("rex", 1);
    expect(a).toEqual(b);
    expect(a).not.toEqual(new Dog("rex", 2) as Named);
    expect(a).not.toEqual(new Cat("rex") as Named);
  });
  it("generic classes", () => {
    const a: Named = new Box<number>(1);
    expect(a).toEqual(new Box<number>(1) as Named);
    expect(a).not.toEqual(new Box<number>(2) as Named);
    expect(a).not.toEqual(new Box<string>("1") as Named);
  });
});
"#;

#[test]
fn to_equal_compares_interface_values_by_their_objects() {
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let path = dir.join("t/iface.test.vlt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, INTERFACES).unwrap();
    let o = velt_test(dir, &["t"]);
    let stdout = String::from_utf8_lossy(&o.stdout).replace("t\\iface", "t/iface");
    let lines: Vec<&str> = stdout
        .lines()
        .skip_while(|l| !l.ends_with("iface.test.vlt"))
        .skip(1)
        .take_while(|l| !l.is_empty())
        .map(|l| l.split(" at t/iface.test.vlt:").next().unwrap())
        .collect();
    assert_eq!(
        lines,
        [
            r#"ok interfaces > same class by fields"#,
            r#"FAILED interfaces > fields differ"#,
            r#"    AssertionError: expected C { v: 1, next: null } to deeply equal C { v: 2, next: null }"#,
            r#"FAILED interfaces > classes differ"#,
            r#"    AssertionError: expected C { v: 1, next: null } to deeply equal D { v: 1, next: null }"#,
            r#"ok interfaces > arrays of interface values"#,
            r#"ok interfaces > interface cycles"#,
            r#"FAILED interfaces > interface rings of two lengths"#,
            r#"    AssertionError: expected <ref *1> C { v: 1, next: [Circular *1] } to deeply equal <ref *1> C { v: 1, next: C { v: 1, next: [Circular *1] } }"#,
            r#"ok interfaces > subclasses by their own fields"#,
            r#"ok interfaces > subclasses through an interface"#,
            r#"ok interfaces > generic classes"#,
        ],
        "{stdout}"
    );
}
