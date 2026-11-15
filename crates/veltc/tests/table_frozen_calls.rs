//! A module constant table (#1334) of an array type the program counts (#3015) goes to a
//! read-only param through the callee's `Work::Frozen` instance (velt_vir `lower/frozen.rs`
//! `table_mask`), which gets the table's read-only header, with no count word in front of it.
//! That instance passes it on as it is to a direct call (another frozen instance), but a call
//! through a vtable reaches code not specialized for it, which counts what it is given: there
//! it must pass a copy (`unfrozen_args`). Without the copy the method changes the word before
//! the table's data and the program still prints the right thing, so only the VIR shows it.

mod no_window;
mod test_dir;

/// The VIR of every function whose name contains `name`, in `src` compiled as `main.velt`.
fn vir_of(src: &str, name: &str) -> String {
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
    let vir = String::from_utf8_lossy(&out.stdout);
    let mut bodies = String::new();
    let mut inside = false;
    for line in vir.lines() {
        if line.starts_with("fn#") {
            inside = line.contains(name);
        }
        if inside {
            bodies.push_str(line);
            bodies.push('\n');
        }
    }
    assert!(!bodies.is_empty(), "no {name} in {vir}");
    bodies
}

const SRC: &str = "
const TABLE: u32[] = [10, 20, 30, 40];
class Holder {
  xs: u32[];
  constructor(xs: u32[]) {
    this.xs = xs;
  }
}
class Reader {
  read(xs: u32[]): u32 {
    return xs[1];
  }
}
class Doubler extends Reader {
  override read(xs: u32[]): u32 {
    return xs[1] * 2;
  }
}
function add(xs: u32[], n: u32): u32 {
  return xs[3] + n;
}
function apply(r: Reader, xs: u32[]): u32 {
  return add(xs, r.read(xs));
}
function main() {
  const a: u32[] = [1, 2];
  const h1 = new Holder(a);
  const h2 = new Holder(a);
  h1.xs.push(3);
  const r: Reader = a.length > 5 ? new Reader() : new Doubler();
  console.log(apply(r, TABLE), h2.xs.length);
}
";

#[test]
fn a_counted_table_is_copied_for_a_vtable_call_and_passed_as_it_is_to_a_direct_one() {
    // `apply`'s frozen instance (`_Z`: its `xs` is in the mask), the one `main` calls.
    let body = vir_of(SRC, "_V5apply_Z");
    let lines: Vec<&str> = body.lines().collect();
    let pos = |what: &dyn Fn(&str) -> bool| lines.iter().position(|l| what(l));
    let clone = pos(&|l| l.contains("call fn#") && l.contains("_Gclone_9u32_5b_5d"));
    let vtable = pos(&|l| l.contains("call (") && l.contains("fn(ptr, ptr) -> u32"));
    let direct = pos(&|l| l.contains("_V3add_Z") && l.contains("(_1,"));
    let (Some(clone), Some(vtable), Some(_)) = (clone, vtable, direct) else {
        panic!("expected a copy before the vtable call and `add_Z(_1, …)`:\n{body}");
    };
    assert!(
        clone < vtable,
        "the copy comes after the vtable call:\n{body}"
    );
}

#[test]
fn main_passes_the_table_without_making_an_array() {
    let body = vir_of(SRC, "_V4main");
    assert!(
        body.contains("_V5apply_Z"),
        "`apply` is not its frozen instance:\n{body}"
    );
    // The table's 4 words would be a 16-byte block.
    assert!(
        !body.contains("velt_rt_alloc(16_u64, 4_u64)"),
        "the table is evaluated to a new array:\n{body}"
    );
}
