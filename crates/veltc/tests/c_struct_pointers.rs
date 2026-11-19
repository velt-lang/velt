//! C structs with pointer fields crossing to C and back (#3094, docs/internals/design/c-ffi.md
//! "As built" 12): a package's C `sources` fill structs whose fields are pointers, Velt reads
//! them with `Ptr.read`, follows their pointers and writes structs with `Ptr.write` that C then
//! checks field by field, in debug (Cranelift) and release (LLVM) builds. A non-null pointer
//! field C left NULL stops the program at the read. These tests need `VELT_ZIG` as
//! tests/c_sources.rs's do.

use std::path::{Path, PathBuf};
use std::process::Output;

mod no_window;
#[allow(dead_code)]
mod reload_support;
#[allow(dead_code)]
mod runtime_support;
mod test_dir;
mod zig_env;

const HEADER: &str = r#"#include <stddef.h>
#include <stdint.h>

typedef struct Name { const char *text; size_t len; } Name;
typedef struct Node { struct Node *next; Name name; int32_t id; float weight; } Node;

const Node *rt_chain(void);
const char *rt_label(void);
int64_t rt_check(const Node *nodes, size_t count);
const Node *rt_broken(void);
"#;

/// A chain of three nodes in static memory, the last one's `next` NULL; a check of nodes Velt
/// wrote, returning which field is wrong (negative) or the sum of the ids; and a node whose
/// non-null `name.text` is NULL.
const SOURCE: &str = r#"#include "rt.h"
#include <string.h>

static const char LABEL[] = "velt";
static const Node C3 = { 0, { "gamma", 5 }, 3, 0.75f };
static const Node C2 = { (Node *)&C3, { "beta", 4 }, 2, 0.5f };
static const Node C1 = { (Node *)&C2, { "alpha", 5 }, 1, 0.25f };
static const Node BROKEN = { 0, { 0, 0 }, 9, 0.0f };

const Node *rt_chain(void) { return &C1; }

const char *rt_label(void) { return LABEL; }

int64_t rt_check(const Node *nodes, size_t count) {
  int64_t sum = 0;
  for (size_t i = 0; i < count; i++) {
    const Node *n = &nodes[i];
    if (i + 1 < count && n->next != &nodes[i + 1]) return -1;
    if (i + 1 == count && n->next != 0) return -2;
    if (n->name.text != LABEL) return -3;
    if (n->name.len != 4 || memcmp(n->name.text, "velt", 4) != 0) return -4;
    if (n->weight != (float)i + 0.5f) return -5;
    sum += n->id;
  }
  return sum;
}

const Node *rt_broken(void) { return &BROKEN; }
"#;

const LIB: &str = r#"import { Ptr, readUtf8 } from "velt/ffi";

export declare c struct Name {
  text: Ptr<u8>;
  len: usize;
}

export declare c struct Node {
  next: Ptr<Node> | null;
  name: Name;
  id: i32;
  weight: f32;
}

declare c("rt") function rt_chain(): Ptr<Node>;
declare c("rt") function rt_label(): Ptr<u8>;
declare c("rt") function rt_check(nodes: Ptr<Node>, count: usize): i64;
declare c("rt") function rt_broken(): Ptr<Node>;
declare c("c") function calloc(n: usize, size: usize): Ptr<Node> | null;
declare c("c") function free(p: Ptr<Node> | null): void;

/** The chain C built: each node's id, name and weight, following `next`. */
export function chain(): string[] {
  const out: string[] = [];
  // SAFETY: C returns its static chain, which lives for the program.
  let p: Ptr<Node> | null = unsafe { rt_chain() };
  while (p !== null) {
    // SAFETY: a node of the chain; its name is `len` bytes of UTF-8.
    const n = unsafe { Ptr.read(p, 0) };
    // SAFETY: as above.
    const name = unsafe { readUtf8(n.name.text, n.name.len) };
    out.push(`${n.id} ${name} ${n.weight}`);
    p = n.next;
  }
  return out;
}

/** `count` nodes written into C memory, linked in order, which C checks: the sum of the ids. */
export function written(count: usize): i64 {
  // SAFETY: zeroed memory for `count` nodes (32 bytes each), freed below.
  const nodes = unsafe { calloc(count, 32) };
  if (nodes === null) {
    return -100;
  }
  // SAFETY: C's static label.
  const label = unsafe { rt_label() };
  for (let i: usize = 0; i < count; i++) {
    // SAFETY: element `i + 1` is inside the block while `i + 1 < count`.
    const next = i + 1 < count ? unsafe { Ptr.at<Node>(Ptr.addressOf(nodes) + (i + 1) * 32) } : null;
    const node: Node = { next, name: { text: label, len: 4 }, id: (i as i32) * 10 + 1, weight: (i as f32) + 0.5 };
    // SAFETY: element `i` of the block.
    unsafe {
      Ptr.write(nodes, i, node);
    }
    ;
  }
  // SAFETY: the block holds `count` linked nodes.
  const sum = unsafe { rt_check(nodes, count) };
  // SAFETY: Velt's own read back of what it wrote.
  const back = unsafe { Ptr.read(nodes, count - 1) };
  // SAFETY: from `calloc`.
  unsafe {
    free(nodes);
  }
  ;
  return back.next === null ? sum : -200;
}

/** A node whose `name.text`, a non-null pointer, C left NULL. */
export function broken(): i32 {
  // SAFETY: C's static node, read whole.
  return unsafe { Ptr.read(rt_broken(), 0) }.id;
}
"#;

/// `dir/cchain`, its C library, and `dir/app` (`main`), which allows it.
fn project(dir: &Path, main: &str) -> PathBuf {
    let pkg = dir.join("cchain");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::create_dir_all(pkg.join("native")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = { name: \"cchain\", version: \"0.1.0\", native: { c: { rt: { sources: [\"native/rt.c\"], headers: [\"native/rt.h\"] }, c: { unchecked: true } } } };\n",
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.velt"), LIB).unwrap();
    std::fs::write(pkg.join("native/rt.h"), HEADER).unwrap();
    std::fs::write(pkg.join("native/rt.c"), SOURCE).unwrap();
    let app = dir.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n  allowNative: [\"cchain\"],\n  dependencies: { cchain: { path: \"../cchain\" } },\n};\n",
    )
    .unwrap();
    std::fs::write(app.join("src/main.velt"), main).unwrap();
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

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// A home whose zig cache is shared across tests, as tests/c_sources.rs's.
fn home(dir: &Path) -> PathBuf {
    let home = dir.join("home");
    let shared = std::env::temp_dir().join("velt-test-c-sources-zig-cache");
    std::fs::create_dir_all(&shared).unwrap();
    let cache = home.join("cache").join("zig");
    std::fs::create_dir_all(&cache).unwrap();
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(&shared, cache.join("build-cache"));
    home
}

const MAIN: &str = r#"import { chain, written } from "cchain";

function main() {
  for (const line of chain()) {
    console.log(line);
  }
  console.log(written(1), written(3));
}
"#;

#[test]
fn c_and_velt_hand_structs_with_pointer_fields_to_each_other() {
    let Some(_zig) = zig_env::zig("build C sources handing structs with pointers to Velt") else {
        return;
    };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let app = project(dir.path(), MAIN);
    let home = home(dir.path());
    for args in [&["run"][..], &["run", "--release"][..]] {
        let o = velt(&app, &home, args);
        assert!(o.status.success(), "velt {args:?}:\n{}", text(&o));
        assert_eq!(
            String::from_utf8_lossy(&o.stdout),
            "1 alpha 0.25\n2 beta 0.5\n3 gamma 0.75\n1 33\n",
            "velt {args:?}:\n{}",
            text(&o)
        );
    }
}

#[test]
fn a_non_null_pointer_field_c_left_null_stops_the_read() {
    let Some(_zig) = zig_env::zig("build C sources handing a struct with a NULL pointer to Velt")
    else {
        return;
    };
    reload_support::build_runtime();
    let dir = test_dir::TestDir::new();
    let main = "import { broken } from \"cchain\";\n\nfunction main() {\n  console.log(\"before\");\n  console.log(broken());\n}\n";
    let app = project(dir.path(), main);
    let home = home(dir.path());
    for args in [&["run"][..], &["run", "--release"][..]] {
        let o = velt(&app, &home, args);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(101), "velt {args:?}:\n{out}");
        assert_eq!(String::from_utf8_lossy(&o.stdout), "before\n", "{out}");
        assert!(
            out.contains("Ptr.read: field `name.text` of the `Node` read is NULL"),
            "velt {args:?}:\n{out}"
        );
    }
}
