//! The vtable word (docs/internals/contracts/rt_abi.md "The vtable word"): an object whose
//! class has a vtable gets that word from its construction only. `alloc_object_raw` (adt.rs)
//! stores the class's vtable, a static, right after the object's zero fill, and no other
//! statement of a lowered program stores into it: no method, function value or glue. The one
//! other write is a whole object copied from another of the same class (the clone glue,
//! `obj_clone_body`: `clone()`, a transfer's deep copy), which gives the word
//! that class's vtable again. velt_opt's `heap_sroa::devirt` reads that vtable as a constant
//! where every allocation a local can hold stored the same one (#1718), so another write would
//! make it call the wrong drop.
//!
//! The check runs over every function of a lowered program, std's glue included. The classes
//! with a vtable are those lowering gives a header (`has_header`: every class of a hierarchy
//! with a base class or a vtable), found from the HIR and matched to their aggregates by name
//! (`<class> object`), whether or not the program builds them. It reports:
//! - an assignment to a place whose last pointer step is to such an object, followed only by
//!   projections at offset 0 (`Field(0)`, `Cast`): a store into the word, however long the
//!   place before it (`p.q.<word>`), unless it is the construction's one store;
//! - an assignment to a whole such object, unless it copies a whole object of the same class;
//! - a `memset` or memory copy into a local that holds a constructed object of such a class in
//!   that function, other than the construction's zero fill.
//!
//! Not covered: a store through a pointer cast to another type (`*(o as u64)`) and a fill or
//! copy into a pointer the function did not see constructed; lowering writes object fields
//! through the object's aggregate, and fills only the objects it allocates.

use std::collections::{HashMap, HashSet};

use velt_sema::hir;

use crate::vir::{self, AggId, Const, Operand, Place, Proj, Rvalue, Stmt, Ty};

use super::{lower_ok, std_src};

/// Classes with a vtable (subclasses, overrides, an abstract base only subclasses of are built),
/// and the paths that copy, clone, transfer, print, compare, view and drop them.
const SRC: &str = r#"
interface VwArea {
  area(): number;
}

class VwShape implements VwArea {
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

class VwSquare extends VwShape {
  side: number;
  constructor(side: number) {
    super("square");
    this.side = side;
  }
  override area(): number {
    return this.side * this.side;
  }
}

class VwCircle extends VwShape {
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

abstract class VwBase {
  n = 0;
  abstract f(): number;
  bump(): void {
    this.n++;
  }
}

class VwLeaf extends VwBase {
  parent: weak VwLeaf | null = null;
  f(): number {
    return this.n;
  }
}

class VwBox<T> {
  item: T;
  constructor(item: T) {
    this.item = item;
  }
  get(): T {
    return this.item;
  }
}

class VwTracked extends VwShape {
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

function total(xs: VwShape[]): number {
  let s = 0;
  for (const x of xs) {
    x.touch();
    s += x.area();
  }
  return s;
}

function viaInterface(h: VwArea): number {
  return h.area();
}

function* shapes(): Generator<VwShape> {
  yield new VwSquare(1);
  yield new VwCircle(2);
}

function keyCount(r: Record<string, unknown>): number {
  return Object.keys(r).length;
}

async function main() {
  const xs: VwShape[] = [new VwSquare(2), new VwCircle(1), new VwShape("plain")];
  console.log(total(xs));
  const copy = [...xs];
  copy.push(new VwSquare(3));
  console.log(total(copy), xs.length);
  const boxed = new VwBox<VwShape>(new VwCircle(2));
  console.log(boxed.get().area(), viaInterface(boxed.get()));
  const byName = new Map<string, VwShape>();
  for (const x of copy) {
    byName.set(x.name, x);
  }
  console.log(byName.size);
  const f = () => xs[0].area() + boxed.get().area();
  console.log(f());
  {
    using t = new VwTracked();
    t.touch();
    console.log(t.area());
  }
  const leaf = new VwLeaf();
  const base: VwBase = leaf;
  base.bump();
  const child = new VwLeaf();
  child.parent = leaf;
  console.log(base.f(), child.parent !== null);
  let g = 0;
  for (const s of shapes()) {
    g += s.area();
  }
  console.log(g);
  const cloned = xs[0].clone();
  const again = xs[1].clone();
  const plain = structuredClone({ id: 1, names: ["a"] });
  console.log(cloned.area(), again.area(), plain.id);
  console.log(keyCount({ shape: xs[2], n: 1 }));
  const sq = shared(new VwSquare(5));
  const done = spawn(async () => sq.area());
  console.log(await done);
  console.log(xs[1], JSON.stringify(xs[0]));
  const weak = new WeakRef(xs[2]);
  console.log(weak.deref() !== undefined, xs[0] === copy[0]);
}
"#;

/// The classes of `SRC` with a vtable (`VwBox` has none).
const HEADED: [&str; 6] = [
    "VwShape",
    "VwSquare",
    "VwCircle",
    "VwBase",
    "VwLeaf",
    "VwTracked",
];

fn lowered() -> (hir::Program, vir::Program) {
    let h = std_src::check_src(SRC);
    let v = lower_ok(&h);
    (h, v)
}

/// The aggregates of the classes lowering gives a vtable header (`has_header`), by name, and the
/// names that are ambiguous (a class with a header and one without share them).
fn headed_aggs(h: &hir::Program, v: &vir::Program) -> (HashSet<AggId>, Vec<String>) {
    let class = |d: hir::DefId| match h.def(d) {
        hir::Def::Adt(a) if a.kind == hir::AdtKind::Class => Some(a),
        _ => None,
    };
    let root = |mut d: hir::DefId| {
        while let Some(b) = class(d).and_then(|a| a.base) {
            match h.types.kind(b) {
                hir::TyKind::Adt(bd, _) => d = *bd,
                _ => break,
            }
        }
        d
    };
    let defs = (0..h.defs.len() as u32).map(hir::DefId);
    let roots: HashSet<hir::DefId> = defs
        .clone()
        .filter(|&d| class(d).is_some_and(|a| a.base.is_some() || !a.vtable.is_empty()))
        .map(root)
        .collect();
    let short = |n: &str| n.rsplit([':', '.', '/']).next().unwrap_or(n).to_string();
    let (mut headed, mut plain) = (HashSet::new(), HashSet::new());
    for d in defs {
        if let Some(a) = class(d) {
            match roots.contains(&root(d)) {
                true => headed.insert(short(&a.name)),
                false => plain.insert(short(&a.name)),
            };
        }
    }
    let (mut aggs, mut ambiguous) = (HashSet::new(), Vec::new());
    for (i, a) in v.aggs.iter().enumerate() {
        let Some(n) = a.name.strip_suffix(crate::OBJECT_AGG_SUFFIX) else {
            continue;
        };
        if headed.contains(n) && plain.contains(n) {
            ambiguous.push(n.to_string());
        } else if headed.contains(n) {
            aggs.insert(AggId(i as u32));
        }
    }
    (aggs, ambiguous)
}

/// The object `p` writes into, if its last pointer step is to an object of `headed`: `Some((a,
/// true))` for the whole object, `Some((a, false))` for its word (only offset-0 projections
/// after the step).
fn written(p: &Place, headed: &HashSet<AggId>) -> Option<(AggId, bool)> {
    let i = p
        .proj
        .iter()
        .rposition(|j| matches!(j, Proj::Deref(Ty::Agg(_))))?;
    let Proj::Deref(Ty::Agg(a)) = p.proj[i] else {
        return None;
    };
    if !headed.contains(&a) {
        return None;
    }
    let rest = &p.proj[i + 1..];
    if rest.is_empty() {
        return Some((a, true));
    }
    let at_zero = rest
        .iter()
        .all(|j| matches!(j, Proj::Field(0) | Proj::Cast(_)));
    at_zero.then_some((a, false))
}

/// What the check found: the stray writes ("function bbN: statement"), the constructions and
/// the classes they build.
struct Found {
    stray: Vec<String>,
    constructions: usize,
    built: HashSet<AggId>,
}

fn check(v: &vir::Program, headed: &HashSet<AggId>) -> Found {
    let mut found = Found {
        stray: Vec::new(),
        constructions: 0,
        built: HashSet::new(),
    };
    for f in &v.funcs {
        // Constructions: in one block, `memset x, 0, n`, `o = x` (any number of copies), then
        // one `(*o).0 = static#s`: the object's first and only store into its word there.
        let (mut allowed, mut fills, mut objects) =
            (HashSet::new(), HashSet::new(), HashSet::new());
        let mut next_id = 0usize;
        for (bi, b) in f.blocks.iter().enumerate() {
            let mut object_of: HashMap<vir::Local, usize> = HashMap::new();
            // Objects filled and not stored into yet, with the fill's statement.
            let mut unstored: HashMap<usize, usize> = HashMap::new();
            for (si, s) in b.stmts.iter().enumerate() {
                match s {
                    Stmt::MemSet {
                        dst: Operand::Copy(p),
                        ..
                    } if p.proj.is_empty() => {
                        next_id += 1;
                        object_of.insert(p.local, next_id);
                        unstored.insert(next_id, si);
                    }
                    Stmt::Assign(d, Rvalue::Use(Operand::Copy(src)))
                        if d.proj.is_empty() && src.proj.is_empty() =>
                    {
                        match object_of.get(&src.local).copied() {
                            Some(id) => object_of.insert(d.local, id),
                            None => object_of.remove(&d.local),
                        };
                    }
                    Stmt::Assign(d, Rvalue::Use(Operand::Const(Const::Static(_), Ty::Ptr))) => {
                        let first = match d.proj.as_slice() {
                            [Proj::Deref(Ty::Agg(a)), Proj::Field(0)] => Some(*a),
                            _ => None,
                        };
                        let id = object_of.get(&d.local).copied();
                        if let (Some(a), Some(id)) = (first, id) {
                            let fill = unstored.remove(&id).filter(|_| headed.contains(&a));
                            if let Some(fill) = fill {
                                // The fill counts as the construction's only now.
                                fills.insert((bi, fill));
                                allowed.insert((bi, si));
                                found.built.insert(a);
                                found.constructions += 1;
                                let names = object_of.iter().filter(|(_, &o)| o == id);
                                objects.extend(names.map(|(&l, _)| l));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        for (bi, b) in f.blocks.iter().enumerate() {
            for (si, s) in b.stmts.iter().enumerate() {
                let stray = match s {
                    Stmt::Assign(d, rv) => match written(d, headed) {
                        // A whole object of the same class copied over it (the clone glue).
                        Some((a, true)) => !matches!(
                            rv,
                            Rvalue::Use(Operand::Copy(src))
                                if src.proj.last() == Some(&Proj::Deref(Ty::Agg(a)))
                        ),
                        Some((_, false)) => !allowed.contains(&(bi, si)),
                        None => false,
                    },
                    Stmt::MemSet { dst, .. }
                    | Stmt::MemCopy { dst, .. }
                    | Stmt::MemCopyDyn { dst, .. } => {
                        let into_object = matches!(
                            dst,
                            Operand::Copy(p) if p.proj.is_empty() && objects.contains(&p.local)
                        );
                        into_object && !fills.contains(&(bi, si))
                    }
                    _ => false,
                };
                if stray {
                    found.stray.push(format!("{} bb{bi}: {s:?}", f.symbol));
                }
            }
        }
    }
    found
}

#[test]
fn only_construction_stores_an_objects_vtable_word() {
    let (h, v) = lowered();
    let (headed, ambiguous) = headed_aggs(&h, &v);
    // Every class of `SRC` with a vtable is checked, `VwBase` (never built itself) included,
    // and `VwBox` (no vtable) is not.
    let names: HashSet<String> = headed
        .iter()
        .map(|a| v.aggs[a.0 as usize].name.clone())
        .collect();
    for c in HEADED {
        let name = format!("{c}{}", crate::OBJECT_AGG_SUFFIX);
        assert!(names.contains(&name), "{c} is not checked: {names:?}");
    }
    assert!(!names.iter().any(|n| n.starts_with("VwBox ")), "{names:?}");
    assert!(
        !ambiguous.iter().any(|n| n.starts_with("Vw")),
        "{ambiguous:?}"
    );
    let found = check(&v, &headed);
    // The constructions of the five classes `SRC` builds (VwBase never is).
    let built: HashSet<String> = found
        .built
        .iter()
        .map(|a| v.aggs[a.0 as usize].name.clone())
        .filter(|n| n.starts_with("Vw"))
        .collect();
    assert_eq!(
        built.len(),
        5,
        "{built:?} of {} constructions",
        found.constructions
    );
    assert!(
        found.stray.is_empty(),
        "stores into a vtable word:\n{}",
        found.stray.join("\n")
    );
}

/// The position of the first construction's store into an object's word in `v`: (function,
/// block, statement, the stored place).
fn construction(v: &vir::Program, headed: &HashSet<AggId>) -> (usize, usize, usize, Place) {
    v.funcs
        .iter()
        .enumerate()
        .find_map(|(fi, f)| {
            f.blocks.iter().enumerate().find_map(|(bi, b)| {
                b.stmts.iter().enumerate().find_map(|(si, s)| match s {
                    Stmt::Assign(d, Rvalue::Use(Operand::Const(Const::Static(_), Ty::Ptr))) => {
                        match d.proj.as_slice() {
                            [Proj::Deref(Ty::Agg(a)), Proj::Field(0)] if headed.contains(a) => {
                                Some((fi, bi, si, d.clone()))
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                })
            })
        })
        .expect("a construction")
}

/// `stmt` inserted at statement `si` of block `bi` of function `fi`.
fn insert(v: &mut vir::Program, (fi, bi, si): (usize, usize, usize), stmt: Stmt) {
    v.funcs[fi].blocks[bi].stmts.insert(si, stmt);
    if let Some(locs) = v.funcs[fi].locs.get_mut(bi) {
        if !locs.is_empty() {
            locs.insert(si, None);
        }
    }
}

/// The check run on `SRC` lowered with one more statement right after a construction's store,
/// made from that store's place and stored value: the stray writes it reports.
fn reported_after_construction(make: impl Fn(&Place, &Stmt) -> Stmt) -> Vec<String> {
    let (h, mut v) = lowered();
    let (headed, _) = headed_aggs(&h, &v);
    let (fi, bi, si, place) = construction(&v, &headed);
    let store = v.funcs[fi].blocks[bi].stmts[si].clone();
    insert(&mut v, (fi, bi, si + 1), make(&place, &store));
    check(&v, &headed).stray
}

fn null() -> Rvalue {
    Rvalue::Use(Operand::Const(Const::Int(0), Ty::Ptr))
}

#[test]
fn a_store_into_the_word_after_construction_is_reported() {
    let stray = reported_after_construction(|p, _| Stmt::Assign(p.clone(), null()));
    assert_eq!(stray.len(), 1, "{stray:?}");
}

#[test]
fn a_second_vtable_store_in_the_construction_is_reported() {
    // The construction's own store again: a construction stores its object's word once.
    let stray = reported_after_construction(|_, store| store.clone());
    assert_eq!(stray.len(), 1, "{stray:?}");
}

#[test]
fn a_store_into_the_word_at_the_end_of_a_longer_place_is_reported() {
    // `o.<field 1>.<word>`: the word of an object reached through another one's field.
    let stray = reported_after_construction(|p, _| {
        let Proj::Deref(agg) = p.proj[0] else {
            unreachable!()
        };
        let place = Place {
            local: p.local,
            proj: vec![
                Proj::Deref(agg),
                Proj::Field(1),
                Proj::Deref(agg),
                Proj::Field(0),
            ],
        };
        Stmt::Assign(place, null())
    });
    assert_eq!(stray.len(), 1, "{stray:?}");
}

#[test]
fn a_fill_or_a_copy_over_a_constructed_object_is_reported() {
    let fill = reported_after_construction(|p, _| Stmt::MemSet {
        dst: Operand::Copy(Place::local(p.local)),
        byte: Operand::Const(Const::Int(0), Ty::U8),
        len: Operand::Const(Const::Int(8), Ty::U64),
    });
    assert_eq!(fill.len(), 1, "{fill:?}");
    let copy = reported_after_construction(|p, _| Stmt::MemCopy {
        dst: Operand::Copy(Place::local(p.local)),
        src: Operand::Copy(Place::local(p.local)),
        size: 8,
    });
    assert_eq!(copy.len(), 1, "{copy:?}");
}

#[test]
fn a_store_into_the_word_of_a_class_never_built_is_reported() {
    // `VwBase.bump` writes `this.n` through `VwBase`'s aggregate, a class only subclasses of are
    // built: the same store at field 0 is reported.
    let (h, mut v) = lowered();
    let (headed, _) = headed_aggs(&h, &v);
    let fi = v
        .funcs
        .iter()
        .position(|f| f.symbol.contains("VwBase") && f.symbol.contains("bump"))
        .unwrap_or_else(|| panic!("no VwBase.bump in:\n{v}"));
    let (bi, si, mut place) = v.funcs[fi]
        .blocks
        .iter()
        .enumerate()
        .find_map(|(bi, b)| {
            b.stmts.iter().enumerate().find_map(|(si, s)| match s {
                Stmt::Assign(d, _) => match d.proj.as_slice() {
                    [Proj::Deref(Ty::Agg(a)), Proj::Field(_)] if headed.contains(a) => {
                        Some((bi, si, d.clone()))
                    }
                    _ => None,
                },
                _ => None,
            })
        })
        .unwrap_or_else(|| panic!("no field store in {}", v.funcs[fi].symbol));
    if let Some(Proj::Field(k)) = place.proj.last_mut() {
        *k = 0;
    }
    insert(&mut v, (fi, bi, si + 1), Stmt::Assign(place, null()));
    let stray = check(&v, &headed).stray;
    assert_eq!(stray.len(), 1, "{stray:?}");
}
