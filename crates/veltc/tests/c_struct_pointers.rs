//! C structs with pointer fields crossing to C and back (#3094, docs/internals/design/c-ffi.md
//! "C structs"): a package's C `sources` fill a list of structs linked by their `next` pointers,
//! with a nested struct pointing at text; Velt reads it with `Ptr.read`, writes another list
//! with `Ptr.write`, and C checks every field of it. A pointer field is C's word at C's offset,
//! `NULL` as `null`, so both sides see the same memory. These tests need `VELT_ZIG` as
//! tests/c_sources.rs's do, and run on Unix (the shared zig cache is a symlink).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
#[allow(dead_code)]
mod reload_support;
#[allow(dead_code)]
mod runtime_support;
mod test_dir;
mod zig_env;

const SOURCE: &str = r#"#include <stdint.h>
#include <stdlib.h>

typedef struct sp_label { uint8_t len; const char *text; } sp_label;
typedef struct sp_node { int32_t value; struct sp_node *next; sp_label label; _Bool done; } sp_node;

static const char TEXT[] = "velt";

const char *sp_text(void) { return TEXT; }

sp_node *sp_alloc(size_t n) { return calloc(n, sizeof(sp_node)); }

void sp_free(sp_node *p) { free(p); }

/* `out[0] -> out[1] -> out[2]`, values 10, 20, 30, labels the last 4, 3 and 2 bytes of TEXT,
   the last node done. */
void sp_fill(sp_node *out) {
  for (int i = 0; i < 3; i++) {
    out[i].value = 10 * (i + 1);
    out[i].next = i < 2 ? &out[i + 1] : NULL;
    out[i].label.len = (uint8_t)(4 - i);
    out[i].label.text = TEXT + i;
    out[i].done = i == 2;
  }
}

/* What Velt wrote from `head`: values 1, 2, 3 linked by `next` up to a NULL, each label's text
   `text` and its length the value, only the last one done. The sum of the values, or the
   negated position of the first wrong field. */
int32_t sp_check(const sp_node *head, const char *text) {
  int32_t sum = 0, at = 1;
  for (const sp_node *n = head; n != NULL; n = n->next, at++) {
    if (n->value != at) return -10 * at - 1;
    if (n->label.text != text) return -10 * at - 2;
    if (n->label.len != (uint8_t)at) return -10 * at - 3;
    if (n->done != (n->next == NULL)) return -10 * at - 4;
    sum += n->value;
  }
  return at == 4 ? sum : -100 - at;
}

/* C's `sizeof` and offsets, for Velt to compare with its own. */
int64_t sp_layout(void) {
  return (int64_t)sizeof(sp_node) * 1000000 + (int64_t)__builtin_offsetof(sp_node, next) * 10000
    + (int64_t)__builtin_offsetof(sp_node, label) * 100 + (int64_t)__builtin_offsetof(sp_node, done);
}
"#;

const LIB: &str = r#"import { Ptr } from "velt/ffi";

declare c struct Label { len: u8; text: Ptr<u8>; }
declare c struct Node { value: i32; next: Ptr<Node> | null; label: Label; done: bool; }

declare c("sp") function sp_text(): Ptr<u8>;
declare c("sp") function sp_alloc(n: usize): Ptr<Node>;
declare c("sp") function sp_free(p: Ptr<Node>): void;
declare c("sp") function sp_fill(out: Ptr<Node>): void;
declare c("sp") function sp_check(head: Ptr<Node>, text: Ptr<u8>): i32;
declare c("sp") function sp_layout(): i64;

/** The `len` bytes at `text`, as a string. */
function bytes(text: Ptr<u8>, len: u8): string {
  let s = "";
  for (let i: usize = 0; i < (len as usize); i++) {
    // SAFETY: C's label points at `len` bytes of its text.
    s += String.fromCharCode(unsafe { Ptr.read(text, i) } as i32);
  }
  return s;
}

/** What C filled, read along the `next` pointers. */
export function readC(): string[] {
  // SAFETY: room for three nodes, freed below.
  const p = unsafe { sp_alloc(3) };
  // SAFETY: it writes the three nodes `p` has room for.
  unsafe { sp_fill(p) };
  const out: string[] = [];
  let at: Ptr<Node> | null = p;
  while (at !== null) {
    // SAFETY: C linked its nodes, the last one's `next` NULL.
    const n = unsafe { Ptr.read(at, 0) };
    out.push(`${n.value} ${bytes(n.label.text, n.label.len)} ${n.done}`);
    at = n.next;
  }
  // SAFETY: allocated above, not used after.
  unsafe { sp_free(p) };
  return out;
}

/** A list Velt writes, which C checks: its sum, or where it went wrong. */
export function writeForC(): i32 {
  // SAFETY: room for three nodes, freed below.
  const p = unsafe { sp_alloc(3) };
  // SAFETY: it returns its static text.
  const text = unsafe { sp_text() };
  for (let i: usize = 0; i < 3; i++) {
    const value = (i + 1) as i32;
    // SAFETY: the address of element `i + 1`, inside the block.
    const next = i < 2 ? unsafe { Ptr.at<Node>(Ptr.addressOf(p) + (i + 1) * 40) } : null;
    // SAFETY: element `i` of the three.
    unsafe { Ptr.write(p, i, { value, next, label: { len: value as u8, text }, done: i == 2 }) };
  }
  // SAFETY: it reads the list Velt wrote.
  const r = unsafe { sp_check(p, text) };
  // SAFETY: allocated above, not used after.
  unsafe { sp_free(p) };
  return r;
}

/** C's size of `Node` and the offsets of `next`, `label` and `done`. */
export function layout(): i64 {
  // SAFETY: it reads nothing.
  return unsafe { sp_layout() };
}
"#;

const MAIN: &str = r#"import { layout, readC, writeForC } from "sp";

function main() {
  console.log(layout());
  for (const line of readC()) {
    console.log(line);
  }
  console.log(writeForC());
}
"#;

/// `dir/sp`, its C library, and `dir/app` (`main`), which allows it. The library is unchecked:
/// the header check of structs is a later layer of #2187.
fn project(dir: &Path) -> PathBuf {
    let pkg = dir.join("sp");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(pkg.join("native")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = { name: \"sp\", version: \"0.1.0\", native: { c: { sp: { sources: [\"native/sp.c\"], unchecked: true } } } };\n",
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.velt"), LIB).unwrap();
    std::fs::write(pkg.join("native/sp.c"), SOURCE).unwrap();
    let app = dir.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n  allowNative: [\"sp\"],\n  dependencies: { sp: { path: \"../sp\" } },\n};\n",
    )
    .unwrap();
    std::fs::write(app.join("src/main.velt"), MAIN).unwrap();
    app
}

fn velt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env("VELT_HOME", home)
        .env("VELT_REGISTRY", home.join("registry"))
        .output()
        .unwrap()
}

/// A home whose zig cache is shared across tests, as tests/c_sources.rs's.
fn home(dir: &Path) -> PathBuf {
    let home = dir.join("home");
    let shared = std::env::temp_dir().join("velt-test-c-sources-zig-cache");
    std::fs::create_dir_all(&shared).unwrap();
    let cache = home.join("cache").join("zig");
    std::fs::create_dir_all(&cache).unwrap();
    let _ = std::os::unix::fs::symlink(&shared, cache.join("build-cache"));
    home
}

#[test]
fn structs_with_pointer_fields_cross_to_c_and_back() {
    let Some(_zig) = zig_env::zig("build C sources sharing structs with pointer fields") else {
        return;
    };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let home = home(dir.path());
    let app = project(dir.path());
    for args in [&["run"][..], &["run", "--release"]] {
        let o = velt(&app, &home, args);
        let (out, err) = (
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr),
        );
        // C's `sizeof` 40 and offsets 8, 16 and 32; C's list as Velt reads it; and the sum
        // 1 + 2 + 3 of the list C found as Velt wrote it.
        assert!(
            o.status.success() && out == "40081632\n10 velt false\n20 elt false\n30 lt true\n6\n",
            "velt {args:?}: exit {:?}\n--- stdout ---\n{out}--- stderr ---\n{err}",
            o.status
        );
    }
}
