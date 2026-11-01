//! A `shared` 64-bit integer is an atomic one (`add`, `get`, `set`). Generic code bounded by
//! `Frozen` reads one as its `T` (concrete code may not): velt_vir `lower/async_fn/sync.rs`
//! lowers that read to the atomic load `get()` uses, on every path (`shared_read_in` for a value
//! read in place, `shared_read_owned` for one kept), never to a plain load that would race with
//! another thread's `add`. A plain load reads the same value on x86-64 and AArch64, so only the
//! VIR shows the difference.

mod no_window;
mod test_dir;

/// The VIR of every function whose name contains `name`, in `src` compiled as `main.vlt`.
fn vir_of(src: &str, name: &str) -> String {
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
function rd<V extends Frozen>(s: shared<V>): V {
  return s;
}

function bind<V extends Frozen>(s: shared<V>): V {
  const v: V = s;
  return v;
}

function later<V extends Frozen>(s: shared<V>): () => V {
  return () => s;
}

async function awaited<V extends Frozen>(s: shared<V>): Promise<V> {
  await Promise.resolve(0);
  return s;
}

async function main() {
  const c = shared<u64>(7);
  console.log(rd<u64>(c), bind<u64>(c), later<u64>(c)(), await awaited<u64>(c));
}
";

/// A plain read of a box's value cell: `_n = (*_m as agg#k).1`, not its address `&(…).1`.
fn plain_cell_reads(body: &str) -> Vec<&str> {
    body.lines()
        .filter(|l| l.contains(" = (*") && l.trim_end().ends_with(").1"))
        .collect()
}

#[test]
fn a_shared_u64_read_through_frozen_is_an_atomic_load() {
    // The return, the binding and the closure: no plain read of the cell is left.
    for f in ["_V2rd_T3u64", "_V4bind_T3u64", "_V5laterN"] {
        let body = vir_of(SRC, f);
        assert!(body.contains("velt_atomic_load_64"), "{f}: {body}");
        assert!(plain_cell_reads(&body).is_empty(), "{f}: {body}");
    }
    // After an `await` (the state machine reads its own fields plainly, so only the load).
    let body = vir_of(SRC, "_V7awaited_T3u64$poll");
    assert!(body.contains("velt_atomic_load_64"), "{body}");
}
