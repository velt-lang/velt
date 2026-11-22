//! A union made from a local only to be lent to a call that reads it (`size(b)` for
//! `size(x: u8[] | Foo)`, #3537) holds a borrowed copy of the local: no retain before the call
//! and no union drop after it. Where the union must own its payload it still does: a later
//! argument that assigns the local, a callee that changes the union, a payload that is not a
//! local. What each program prints is checked by the golden `lang/union_lent_payload`.

mod no_window;
mod test_dir;

const CLASS: &str = "class Foo {
  n: number = 1;
}
";

fn vir(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), format!("{CLASS}{src}")).unwrap();
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

/// The body of `main` in `vir`.
fn main_of(vir: &str) -> &str {
    let start = vir.find(" _V4main(").expect("no main");
    let end = vir[start..].find("\n}\n").map_or(vir.len(), |e| start + e);
    &vir[start..end]
}

/// Does `main` retain a counted value (`rc += 1` on a box)?
fn retains(vir: &str) -> bool {
    main_of(vir).contains(" as u64), 1_u64")
}

/// Does `main` drop a `Foo | u8[]` union (the call's argument was owned)?
fn drops_union(vir: &str) -> bool {
    main_of(vir).contains("_Gdrop_20Foo_20_7c_20u8_5b_5d")
}

#[test]
fn a_local_lent_to_a_reading_union_param_is_not_retained_or_dropped() {
    let vir = vir("function size(x: u8[] | Foo, k: number): number {
  return x instanceof Foo ? x.n : (x.length as number) + k;
}
function main() {
  const b = Buffer.alloc(8);
  let s = 0;
  for (let i = 0; i < 10; i++) {
    s += size(b, i % 3);
  }
  console.log(s);
}
");
    assert!(
        !drops_union(&vir),
        "the lent union is dropped:\n{}",
        main_of(&vir)
    );
    assert!(
        !retains(&vir),
        "`b` is retained for the union:\n{}",
        main_of(&vir)
    );
}

#[test]
fn a_later_argument_that_assigns_the_local_keeps_the_union_owning() {
    let vir = vir("function size(x: u8[] | Foo, k: number): number {
  return x instanceof Foo ? x.n : (x.length as number) + k;
}
function main() {
  let b = Buffer.alloc(8);
  console.log(size(b, (b = Buffer.alloc(2)).length));
  console.log(b.length);
}
");
    assert!(
        retains(&vir),
        "`b` is not retained for the union:\n{}",
        main_of(&vir)
    );
    assert!(
        drops_union(&vir),
        "the union does not own its payload:\n{}",
        main_of(&vir)
    );
}

#[test]
fn a_callee_that_changes_the_union_keeps_it_owning() {
    // `swap` assigns its parameter, so it takes the union to drop or replace: the caller retains
    // `b` for it rather than lending a borrowed copy, which `swap`'s assignment would free.
    let vir = vir("function swap(x: u8[] | Foo): number {
  x = new Foo();
  return x instanceof Foo ? x.n : 0;
}
function main() {
  const b = Buffer.alloc(8);
  console.log(swap(b), b.length);
}
");
    assert!(
        retains(&vir),
        "`b` is not retained for the union:\n{}",
        main_of(&vir)
    );
}

#[test]
fn a_payload_that_is_not_a_local_keeps_the_union_owning() {
    let vir = vir("function size(x: u8[] | Foo): number {
  return x instanceof Foo ? x.n : x.length as number;
}
function main() {
  console.log(size(Buffer.alloc(8)));
}
");
    assert!(
        drops_union(&vir),
        "the union does not own its payload:\n{}",
        main_of(&vir)
    );
}
