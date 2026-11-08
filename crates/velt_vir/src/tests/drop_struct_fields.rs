//! The drop of a struct value whose fields may lead back to its own type (#543,
//! `lower/glue/drop_depth.rs`): each such field is bracketed for the runtime's depth limit, as a
//! class's are, so a null field drops without the bracket, and long chains through struct values
//! still drop from the queue (the interpreter's limit is 4) without leaks.

use super::{lower_src, run_src};
use crate::vir::{self, Callee, Terminator};

/// A struct that reaches an interface value through a nullable `shared` field, dropped with that
/// field null in a loop.
const NULL_FIELD: &str = "interface Side {
  poke(): void;
}

class Holder {
  side: Side | null = null;
}

struct WithSide {
  handle: u64;
  side: shared<Mutex<Holder>> | null;
}

function use1(s: WithSide): u64 {
  return s.handle + 1;
}

function main() {
  let sum: u64 = 0;
  for (let i: u64 = 0; i < 3; i++) {
    sum += use1({ handle: i, side: null });
  }
  sum += use1({ handle: 10, side: shared(new Mutex(new Holder())) });
  console.log(`${sum}`);
}
";

/// The drop glue of the type named `name`.
fn drop_glue<'a>(v: &'a vir::Program, name: &str) -> &'a vir::Function {
    let sym = format!("_Gdrop_{}{name}", name.len());
    v.funcs
        .iter()
        .find(|f| f.symbol == sym)
        .unwrap_or_else(|| panic!("no `{sym}` in:\n{v}"))
}

/// Does block `b` end in a call of the runtime's drop state?
fn calls_drop_state(v: &vir::Program, b: &vir::BasicBlock) -> bool {
    matches!(&b.term, Terminator::Call { callee: Callee::Extern(e), .. }
        if v.externs[e.0 as usize].symbol == "velt_rt_drop_state")
}

#[test]
fn a_null_field_drops_without_the_bracket() {
    let v = lower_src(NULL_FIELD);
    let glue = drop_glue(&v, "WithSide");
    assert!(
        !calls_drop_state(&v, &glue.blocks[0]),
        "the drop brackets the whole struct before looking at its field:\n{v}"
    );
    assert!(
        glue.blocks.iter().any(|b| calls_drop_state(&v, b)),
        "the field that reaches an interface is not bracketed:\n{v}"
    );
    assert_eq!(run_src(NULL_FIELD).stdout, "17\n");
}

#[test]
fn chains_through_struct_fields_drop_from_the_queue() {
    // Through a class field and through an interface value, 200 links each.
    let src = "interface Side {
  id(): i64;
}

class Node implements Side {
  next: Link | null = null;
  id(): i64 {
    return 1;
  }
}

struct Link {
  id: i64;
  node: Node | null;
}

struct DynLink {
  id: i64;
  side: Side | null;
}

class DynNode implements Side {
  next: DynLink;
  constructor(next: DynLink) {
    this.next = next;
  }
  id(): i64 {
    return this.next.id + 1;
  }
}

function main() {
  let head: Link = { id: 0, node: null };
  for (let i: i64 = 1; i < 200; i++) {
    const n = new Node();
    n.next = head;
    head = { id: i, node: n };
  }
  let d: DynLink = { id: 0, side: null };
  for (let i: i64 = 1; i < 200; i++) {
    d = { id: i, side: new DynNode(d) };
  }
  console.log(`${head.id} ${d.id} ${d.side!.id()}`);
}
";
    assert_eq!(run_src(src).stdout, "199 199 199\n");
}
