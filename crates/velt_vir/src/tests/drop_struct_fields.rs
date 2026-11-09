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

/// Structs whose fields that lead back are null (nullable interface values, a nullable class)
/// and, last, one whose interface field holds a short chain.
const NULL_FIELDS: &str = "interface Side {
  id(): i64;
}

class Node implements Side {
  next: DynLink | null = null;
  id(): i64 {
    return 1;
  }
}

struct DynLink {
  id: i64;
  a: Side | null;
  b: Side | null;
  c: Node | null;
}

function make(i: i64): DynLink {
  return { id: i, a: null, b: null, c: null };
}

function main() {
  let sum: i64 = 0;
  for (let i: i64 = 0; i < 10; i++) {
    const l = make(i);
    sum += l.id;
  }
  console.log(`${sum}`);
}
";

#[test]
fn null_fields_drop_without_a_bracket() {
    // Ten structs, every field that leads back null: not one bracketed drop (the whole-value
    // bracket made one per struct; a bracket per field without the tag test, two per struct).
    let out = run_src(NULL_FIELDS);
    assert_eq!(out.stdout, "45\n");
    assert_eq!(out.drop_states, 0, "null fields were bracketed");
    // A field that holds something is bracketed: once for the link's `a`, once for the node's
    // `next`, whose struct's fields are null again.
    let src = NULL_FIELDS.replace(
        "  console.log(`${sum}`);",
        "  const n = new Node();\n  n.next = make(1);\n  const l: DynLink = { id: 2, a: n, b: null, c: null };\n  console.log(`${sum + l.id}`);",
    );
    let out = run_src(&src);
    assert_eq!(out.stdout, "47\n");
    assert_eq!(
        out.drop_states, 2,
        "a field that holds a value is not bracketed once"
    );
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

#[test]
fn a_chain_of_structs_through_their_own_array_drops_from_the_queue() {
    // Struct inside struct through an array, with no class on the way: only the struct's own
    // bracket bounds the nesting. 200 links, depth limit 4: most of them are queued.
    let src = "struct Tree {
  id: i64;
  kids: Tree[];
}

function trees(n: i64): Tree {
  let h: Tree = { id: 0, kids: [] };
  for (let i: i64 = 1; i < n; i++) {
    h = { id: i, kids: [h] };
  }
  return h;
}

function main() {
  {
    const t = trees(200);
    console.log(`${t.id}`);
  }
  console.log(\"ok\");
}
";
    let out = run_src(src);
    assert_eq!(out.stdout, "199\nok\n");
    assert!(
        out.drop_states >= 199,
        "the links are not bracketed: {} bracketed drops",
        out.drop_states
    );
}
