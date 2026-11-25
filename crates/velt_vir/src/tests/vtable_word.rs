//! The vtable word (docs/internals/contracts/rt_abi.md "The vtable word"): an object whose
//! class has a vtable gets that word from its construction only. `alloc_object_raw` (adt.rs)
//! stores the class's vtable, a static, right after the object's zero fill, and no other
//! statement of a lowered program stores into it: no method, function value or glue. The one
//! other write is a whole object copied from another of the same class (the clone glue of a
//! transfer's deep copy), which gives the word that class's vtable again. velt_opt's
//! `heap_sroa::devirt` reads that vtable as a constant where every allocation a local can hold
//! stored the same one (#1718), so a later store would make it call the wrong drop.
//!
//! The check runs over every function of a lowered program, std's glue included: a program with
//! subclasses whose methods write fields, disposal, closures, interface values, collections,
//! a transfer to another task and printing.

use crate::vir::{self, Const, Operand, Place, Proj, Rvalue, Stmt, Ty};

use super::lower_src;

/// Classes with subclasses and overrides (their objects carry a vtable), and the paths that
/// copy, transfer, print, compare and drop them.
const SRC: &str = r#"
interface HasArea {
  area(): number;
}

class Shape implements HasArea {
  name: string;
  hits = 0;
  constructor(name: string) {
    this.name = name;
  }
  area(): number {
    return 0;
  }
  touch(): void {
    this.hits++;
    this.name = this.name + "!";
  }
}

class Square extends Shape {
  side: number;
  constructor(side: number) {
    super("square");
    this.side = side;
  }
  override area(): number {
    return this.side * this.side;
  }
}

class Circle extends Shape {
  r: number;
  constructor(r: number) {
    super("circle");
    this.r = r;
  }
  override area(): number {
    return 3 * this.r * this.r;
  }
  override touch(): void {
    super.touch();
    this.r += 1;
  }
}

class Box<T> {
  item: T;
  constructor(item: T) {
    this.item = item;
  }
  get(): T {
    return this.item;
  }
}

class Tracked extends Shape {
  constructor() {
    super("tracked");
  }
  override area(): number {
    return 1;
  }
  [Symbol.dispose]() {
    console.log("dispose", this.name);
  }
}

function total(xs: Shape[]): number {
  let s = 0;
  for (const x of xs) {
    x.touch();
    s += x.area();
  }
  return s;
}

function viaInterface(h: HasArea): number {
  return h.area();
}

async function main() {
  const xs: Shape[] = [new Square(2), new Circle(1), new Shape("plain")];
  console.log(total(xs));
  const copy = [...xs];
  copy.push(new Square(3));
  console.log(total(copy), xs.length);
  const boxed = new Box<Shape>(new Circle(2));
  console.log(boxed.get().area(), viaInterface(boxed.get()));
  const byName = new Map<string, Shape>();
  for (const x of copy) {
    byName.set(x.name, x);
  }
  console.log(byName.size);
  const f = () => xs[0].area() + boxed.get().area();
  console.log(f());
  {
    using t = new Tracked();
    t.touch();
    console.log(t.area());
  }
  const sq = shared(new Square(5));
  const done = spawn(async () => sq.area());
  console.log(await done);
  console.log(xs[1], JSON.stringify(xs[0]));
  const weak = new WeakRef(xs[2]);
  console.log(weak.deref() !== undefined, xs[0] === copy[0]);
}
"#;

/// The stores into field 0 of an object whose class has a vtable that are not its
/// construction's, as "function block: statement", and how many constructions there are.
fn stray_stores(v: &vir::Program) -> (Vec<String>, usize) {
    let first = |p: &Place| match p.proj.as_slice() {
        [Proj::Deref(Ty::Agg(a)), Proj::Field(0)] => Some(*a),
        _ => None,
    };
    // Construction: in one block, `memset x, 0, size`, then (optionally) `o = x`, then
    // `(*o).0 = static#s`. Its aggregates are the classes with a vtable.
    let mut headed = std::collections::HashSet::new();
    let mut constructions = std::collections::HashSet::new();
    for (fi, f) in v.funcs.iter().enumerate() {
        for (bi, b) in f.blocks.iter().enumerate() {
            let mut filled = std::collections::HashSet::new();
            for (si, s) in b.stmts.iter().enumerate() {
                match s {
                    Stmt::MemSet {
                        dst: Operand::Copy(p),
                        ..
                    } if p.proj.is_empty() => {
                        filled.insert(p.local);
                    }
                    Stmt::Assign(d, Rvalue::Use(Operand::Copy(src)))
                        if d.proj.is_empty()
                            && src.proj.is_empty()
                            && filled.contains(&src.local) =>
                    {
                        filled.insert(d.local);
                    }
                    Stmt::Assign(d, Rvalue::Use(Operand::Const(Const::Static(_), Ty::Ptr)))
                        if filled.contains(&d.local) =>
                    {
                        if let Some(a) = first(d) {
                            headed.insert(a);
                            constructions.insert((fi, bi, si));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let mut stray = Vec::new();
    for (fi, f) in v.funcs.iter().enumerate() {
        for (bi, b) in f.blocks.iter().enumerate() {
            for (si, s) in b.stmts.iter().enumerate() {
                let Stmt::Assign(d, rv) = s else { continue };
                // A whole object copied from another of the same class (the clone glue's deep
                // copy, `_Gobjclone_*`): the word becomes that class's vtable again.
                let whole = match d.proj.as_slice() {
                    [Proj::Deref(Ty::Agg(a))] if headed.contains(a) => !matches!(
                        rv,
                        Rvalue::Use(Operand::Copy(src))
                            if src.proj.as_slice() == [Proj::Deref(Ty::Agg(*a))]
                    ),
                    _ => false,
                };
                let word = first(d).is_some_and(|a| headed.contains(&a));
                if (word && !constructions.contains(&(fi, bi, si))) || whole {
                    stray.push(format!("{} bb{bi}: {s:?}", f.symbol));
                }
            }
        }
    }
    (stray, constructions.len())
}

#[test]
fn only_construction_stores_an_objects_vtable_word() {
    let v = lower_src(SRC);
    let (stray, constructions) = stray_stores(&v);
    // The program makes objects of five classes with a vtable (Shape and its three subclasses,
    // and Box's instance has none): the check must have found their constructions.
    assert!(
        constructions >= 4,
        "{constructions} constructions found\n{v}"
    );
    assert!(
        stray.is_empty(),
        "stores into a vtable word:\n{}",
        stray.join("\n")
    );
}

#[test]
fn a_store_into_the_vtable_word_after_construction_is_reported() {
    // The check itself: the same program with one more store into a constructed object's
    // vtable word, after its construction.
    let mut v = lower_src(SRC);
    let (fi, bi, si, stmt) = v
        .funcs
        .iter()
        .enumerate()
        .find_map(|(fi, f)| {
            f.blocks.iter().enumerate().find_map(|(bi, b)| {
                b.stmts.iter().enumerate().find_map(|(si, s)| match s {
                    Stmt::Assign(d, Rvalue::Use(Operand::Const(Const::Static(_), Ty::Ptr)))
                        if matches!(
                            d.proj.as_slice(),
                            [Proj::Deref(Ty::Agg(_)), Proj::Field(0)]
                        ) =>
                    {
                        Some((fi, bi, si, s.clone()))
                    }
                    _ => None,
                })
            })
        })
        .expect("a construction");
    let Stmt::Assign(d, _) = stmt else {
        unreachable!()
    };
    let again = Stmt::Assign(d, Rvalue::Use(Operand::Const(Const::Int(0), Ty::Ptr)));
    let block = &mut v.funcs[fi].blocks[bi];
    block.stmts.insert(si + 1, again);
    if let Some(locs) = v.funcs[fi].locs.get_mut(bi) {
        if !locs.is_empty() {
            locs.insert(si + 1, None);
        }
    }
    let (stray, _) = stray_stores(&v);
    assert_eq!(stray.len(), 1, "{stray:?}");
}
