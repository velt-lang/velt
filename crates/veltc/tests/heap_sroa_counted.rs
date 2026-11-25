//! Short-lived counted objects kept in locals by `heap_sroa` (#1718), end to end: release builds
//! linked with the debug runtime, which counts every block and reports a use after free or a
//! double free. Debug builds do not optimize, so these are the runs that check the pass.
//! - Views made and dropped in a loop (`subarray`, `new DataView`) are not allocated: their
//!   counts, their buffer's counts and their drops stay, as locals, and every block balances.
//! - A view alive across a caught throw (unwinding through the cleanup pads), a view returned
//!   (its buffer outlives the page), and `using` objects whose dispose prints, in Node's order.
//! - An object a `WeakRef` holds stays on the heap: the weak table knows it by its address.

mod no_window;
mod test_dir;

use std::path::{Path, PathBuf};

const VIEWS: &str = r#"// Short-lived counted objects (#1718): views made and dropped in a loop, a view alive across a
// caught throw, a view's buffer outliving the page it came from, and objects with a dispose
// that has side effects. The leak check counts every block.

class Logged {
  name: string;
  constructor(name: string) {
    this.name = name;
  }
  [Symbol.dispose]() {
    console.log(`dispose ${this.name}`);
  }
}

function sumViews(page: Uint8Array, cells: number): number {
  let s = 0;
  for (let i = 0; i < cells; i++) {
    const body = page.subarray(i, i + 8);
    const dv = new DataView(body.buffer, body.byteOffset, body.byteLength);
    s += body[0] + dv.getUint16(1) + body.length;
  }
  return s;
}

function check(n: number): number {
  if (n > 5) {
    throw new RangeError(`too big: ${n}`);
  }
  return n;
}

function viewAcrossThrow(page: Uint8Array, at: number): string {
  const v = page.subarray(at, at + 4);
  try {
    return `${check(v[0]) + v.length}`;
  } catch (e) {
    return `${(e as Error).message} (${v.length})`;
  }
}

function lastView(): Uint8Array {
  const page = new Uint8Array(16);
  page[3] = 7;
  return page.subarray(2, 6);
}

function disposeOrder(): number {
  using a = new Logged("a");
  using b = new Logged("b");
  console.log(`body ${a.name}${b.name}`);
  return a.name.length + b.name.length;
}

function main() {
  const page = new Uint8Array(64);
  for (let i = 0; i < 64; i++) {
    page[i] = (i * 7) & 0xff;
  }
  console.log(sumViews(page, 40), sumViews(page, 0));
  console.log(viewAcrossThrow(page, 0), viewAcrossThrow(page, 1));
  const kept = lastView();
  console.log(kept.length, kept[1], kept.byteOffset, kept.buffer.byteLength);
  console.log(disposeOrder());
}
"#;

const WEAK: &str = r#"class Cell {
  v: number = 1;
  next: Cell | null = null;
}

function weakly(n: number): number {
  const c = new Cell();
  c.v = n;
  const w = new WeakRef(c);
  return (w.deref()?.v ?? 0) + c.v;
}

function cyclic(n: number): number {
  const c = new Cell();
  c.v = n;
  c.next = c;
  const r = c.next!.v;
  c.next = null;
  return r;
}

function main() {
  let s = 0;
  for (let i = 0; i < 10; i++) {
    s += weakly(i) + cyclic(i);
  }
  console.log(s);
}
"#;

fn std_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std")
}

fn debug_runtime() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_velt"))
        .parent()
        .unwrap()
        .join(if cfg!(windows) {
            "velt_rt.lib"
        } else {
            "libvelt_rt.a"
        })
}

/// `velt <args> main.velt` with `src`, linked with the debug runtime: (stdout, stderr).
fn velt(src: &str, args: &[&str]) -> (String, String) {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .arg("main.velt")
        .env("VELT_STD", std_dir())
        .env("VELT_RC_STATS", "1")
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .env("VELT_RT_LIB", debug_runtime())
        .current_dir(dir.path())
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (text(&out.stdout), text(&out.stderr))
}

/// The `blocks=allocated/freed` of the runtime's report.
fn blocks(stderr: &str) -> (u64, u64) {
    let line = stderr
        .lines()
        .rfind(|l| l.starts_with("rc stats:"))
        .unwrap_or_else(|| panic!("no rc stats: {stderr}"));
    let at = line.find(" blocks=").expect("blocks") + " blocks=".len();
    let (a, f) = line[at..]
        .split(' ')
        .next()
        .unwrap()
        .split_once('/')
        .unwrap();
    (a.parse().unwrap(), f.parse().unwrap())
}

#[test]
fn views_in_a_loop_are_not_allocated_and_every_block_is_freed() {
    let want = "1217048 0\n4 too big: 7 (4)\n4 7 2 16\nbody ab\ndispose b\ndispose a\n2\n";
    let (debug, debug_err) = velt(VIEWS, &["run"]);
    let (release, release_err) = velt(VIEWS, &["run", "--release"]);
    assert_eq!(debug, want, "{debug_err}");
    assert_eq!(release, want, "{release_err}");
    assert!(!release_err.contains("debug-alloc"), "{release_err}");
    let (made, freed) = blocks(&release_err);
    assert_eq!(made, freed, "a block leaked: {release_err}");
    // Debug builds allocate both views of each of the 40 rounds (94 blocks); release builds
    // only the pages, the returned view and what the error paths make.
    let (debug_made, _) = blocks(&debug_err);
    assert!(debug_made >= 94, "{debug_err}");
    assert!(
        made <= 20,
        "views were allocated: {made} blocks\n{release_err}"
    );
}

#[test]
fn an_object_a_weak_ref_holds_stays_on_the_heap() {
    let (out, err) = velt(WEAK, &["run", "--release"]);
    assert_eq!(out, "135\n", "{err}");
    let (made, freed) = blocks(&err);
    // `weakly`'s ten cells; `cyclic`'s, plain objects, were in locals before #1718.
    assert_eq!((made, freed), (10, 10), "{err}");
}
