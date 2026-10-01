//! Constant fields of aggregate locals, and the pointer locals that alias them.
//!
//! A field of an aggregate local `a` is *constant* when every whole assignment of `a` is an
//! `Aggregate` rvalue with the same constant operand for that field and nothing else writes
//! the field. Taking `a`'s address is allowed only as `p = &a` into a single-definition
//! pointer local `p` that is read-only (see `readonly`): then no write can reach `a` through
//! memory, and reads `(*p as agg).f` see the constant too. Reads before `a`'s first
//! assignment would read uninitialized memory, which lowering never does.
//!
//! This is what closures need: a function value is `{ code, env }` built once, passed by
//! pointer, and called through `(*p).0`; knowing `code` lets constfold devirtualize the call.

use std::collections::{HashMap, HashSet};

use velt_vir::vir::{
    AggId, AggLayout, Const, Function, Local, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty,
};

use super::readonly::ReadOnly;
use crate::locals::Usage;
use crate::visit::{derefs, stmt_operands_mut, term_operands_mut};

/// Known constant fields of an aggregate of type `agg`, sorted by field index.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Known {
    /// The aggregate type.
    pub agg: AggId,
    /// (field, value, field type).
    pub fields: Vec<(u32, Const, Ty)>,
}

impl Known {
    /// Whether some known field is a function address (worth specializing callees for).
    pub fn has_code(&self) -> bool {
        self.fields
            .iter()
            .any(|(_, c, _)| matches!(c, Const::Func(_)))
    }

    fn get(&self, field: u32) -> Option<(&Const, Ty)> {
        self.fields
            .iter()
            .find(|(f, _, _)| *f == field)
            .map(|(_, c, t)| (c, *t))
    }
}

/// Facts for one function: aggregate locals with constant fields, and pointer locals whose
/// pointee is such an aggregate.
#[derive(Default)]
pub(crate) struct Facts {
    /// Aggregate local → its constant fields.
    pub direct: HashMap<Local, Known>,
    /// Pointer local → constant fields of what it points to.
    pub pointers: HashMap<Local, Known>,
}

/// A whole assignment of an aggregate local.
enum Def {
    /// `a = agg { ops }`.
    Aggregate(Vec<Operand>),
    /// `a = b` from another aggregate local.
    Copy(Local),
    /// Anything else (call result, copy from memory, …).
    Unknown,
}

/// Per-aggregate-local observations gathered in one scan.
#[derive(Default)]
struct Observed {
    /// Every whole assignment.
    defs: Vec<Def>,
    /// Fields written partially (`None` in the set: something unknown was written).
    dirty: Vec<Option<u32>>,
    /// Pointer locals assigned `&a`; `None` for any other way the address was taken.
    pointers: Vec<Option<Local>>,
}

/// Analyze `func`.
pub(crate) fn analyze(aggs: &[AggLayout], func: &Function, ro: &ReadOnly) -> Facts {
    let mut seen: HashMap<Local, Observed> = HashMap::new();
    for (i, l) in func.locals.iter().enumerate().skip(func.params.len()) {
        if matches!(l.ty, Ty::Agg(_)) {
            seen.insert(Local(i as u32), Observed::default());
        }
    }
    for block in &func.blocks {
        for s in &block.stmts {
            observe_stmt(s, &mut seen);
        }
        if let Terminator::Call { dest: Some(d), .. } = &block.term {
            observe_write(d, None, &mut seen);
        }
    }
    let usage = Usage::of(func);
    let read_only = read_only_pointers(func, ro, &seen);
    // Locals whose address escapes (or whose fields are rewritten wholesale) are unknown.
    let mut pointers: HashMap<Local, Vec<Local>> = HashMap::new();
    seen.retain(|&a, obs| {
        let keep = !obs.dirty.contains(&None) && !obs.defs.is_empty();
        match safe_pointers(func, &usage, &read_only, obs) {
            Some(ps) if keep => {
                pointers.insert(a, ps);
                true
            }
            _ => false,
        }
    });
    let known = solve(aggs, func, &seen);
    let mut facts = Facts::default();
    for (a, k) in known {
        for &p in &pointers[&a] {
            facts.pointers.insert(p, k.clone());
        }
        facts.direct.insert(a, k);
    }
    facts
}

/// Constant fields of every candidate, following copies between candidates: a local is
/// decided once all the locals it copies from are, so copy chains resolve in order (cycles
/// stay undecided, i.e. unknown). Kahn's algorithm over the copy edges, so long copy chains
/// cost one visit per local instead of one pass over all candidates per link.
fn solve(
    aggs: &[AggLayout],
    func: &Function,
    seen: &HashMap<Local, Observed>,
) -> HashMap<Local, Known> {
    let mut pending: HashMap<Local, usize> = HashMap::new();
    let mut dependents: HashMap<Local, Vec<Local>> = HashMap::new();
    for (&a, obs) in seen {
        let deps = obs.defs.iter().filter_map(|d| match d {
            Def::Copy(b) if seen.contains_key(b) => Some(*b),
            _ => None,
        });
        let mut count = 0;
        for b in deps {
            dependents.entry(b).or_default().push(a);
            count += 1;
        }
        pending.insert(a, count);
    }
    let mut ready: Vec<Local> = pending
        .iter()
        .filter_map(|(&a, &c)| (c == 0).then_some(a))
        .collect();
    let mut decided: HashMap<Local, Option<Known>> = HashMap::new();
    while let Some(a) = ready.pop() {
        let Ty::Agg(id) = func.locals[a.0 as usize].ty else {
            continue;
        };
        let known = aggs
            .get(id.0 as usize)
            .and_then(|layout| constant_fields(id, layout, &seen[&a], &decided));
        decided.insert(a, known);
        for d in dependents.get(&a).into_iter().flatten() {
            let c = pending.get_mut(d).expect("ICE: dependent is a candidate");
            *c -= 1;
            if *c == 0 {
                ready.push(*d);
            }
        }
    }
    decided
        .into_iter()
        .filter_map(|(a, k)| k.map(|k| (a, k)))
        .collect()
}

fn observe_stmt(s: &Stmt, seen: &mut HashMap<Local, Observed>) {
    let Stmt::Assign(dst, rv) = s else { return };
    if let Rvalue::AddrOf(q) = rv {
        if let Some(obs) = seen.get_mut(&q.local) {
            if !derefs(q) {
                let whole = q.proj.is_empty() && dst.proj.is_empty();
                obs.pointers.push(whole.then_some(dst.local));
            }
        }
    }
    observe_write(dst, Some(rv), seen);
}

/// Record a write of `dst` (with `rv` for assignments, `None` for call results).
fn observe_write(dst: &Place, rv: Option<&Rvalue>, seen: &mut HashMap<Local, Observed>) {
    let Some(obs) = seen.get_mut(&dst.local) else {
        return;
    };
    if derefs(dst) {
        // Writes memory reached through a pointer stored in the aggregate, not the aggregate.
        return;
    }
    match (dst.proj.first(), rv) {
        (None, Some(Rvalue::Aggregate(_, ops))) => obs.defs.push(Def::Aggregate(ops.clone())),
        (None, Some(Rvalue::Use(Operand::Copy(src)))) if src.proj.is_empty() => {
            obs.defs.push(Def::Copy(src.local))
        }
        (None, _) => obs.defs.push(Def::Unknown),
        (Some(Proj::Field(f)), _) => obs.dirty.push(Some(*f)),
        (Some(_), _) => obs.dirty.push(None),
    }
}

/// The locals holding some candidate's address that are only read through (one scan for all).
fn read_only_pointers(
    func: &Function,
    ro: &ReadOnly,
    seen: &HashMap<Local, Observed>,
) -> HashSet<Local> {
    let candidates: Vec<Local> = seen
        .values()
        .flat_map(|obs| obs.pointers.iter().flatten().copied())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let verdicts = ro.read_only_locals(func, &candidates, true);
    candidates
        .into_iter()
        .zip(verdicts)
        .filter_map(|(p, ok)| ok.then_some(p))
        .collect()
}

/// The pointer locals holding the aggregate's address, if every one of them is safe.
fn safe_pointers(
    func: &Function,
    usage: &Usage,
    read_only: &HashSet<Local>,
    obs: &Observed,
) -> Option<Vec<Local>> {
    let mut out = Vec::with_capacity(obs.pointers.len());
    for p in &obs.pointers {
        let p = (*p)?;
        let single = usage.is_register(p) && usage.get(p).defs == 1;
        let is_param = (p.0 as usize) < func.params.len();
        if !single || is_param || !read_only.contains(&p) {
            return None;
        }
        out.push(p);
    }
    Some(out)
}

/// The constant a definition stores into field `f`, if any.
fn def_field(
    def: &Def,
    f: u32,
    decided: &HashMap<Local, Option<Known>>,
    id: AggId,
) -> Option<(Const, Ty)> {
    match def {
        Def::Aggregate(ops) => match ops.get(f as usize)? {
            Operand::Const(c, t) => Some((c.clone(), *t)),
            Operand::Copy(_) => None,
        },
        Def::Copy(b) => {
            let known = decided.get(b)?.as_ref()?;
            if known.agg != id {
                return None;
            }
            known.get(f).map(|(c, t)| (c.clone(), t))
        }
        Def::Unknown => None,
    }
}

fn constant_fields(
    id: AggId,
    layout: &AggLayout,
    obs: &Observed,
    decided: &HashMap<Local, Option<Known>>,
) -> Option<Known> {
    let arity_ok = obs.defs.iter().all(|d| match d {
        Def::Aggregate(ops) => ops.len() == layout.fields.len(),
        Def::Copy(_) => true,
        Def::Unknown => false,
    });
    if !arity_ok {
        return None;
    }
    let mut fields = Vec::new();
    for (f, &(ty, _)) in layout.fields.iter().enumerate() {
        let f = f as u32;
        if !ty.is_scalar() || obs.dirty.contains(&Some(f)) {
            continue;
        }
        let Some((c, t)) = def_field(&obs.defs[0], f, decided, id) else {
            continue;
        };
        let same = obs.defs[1..]
            .iter()
            .all(|d| def_field(d, f, decided, id).is_some_and(|(c2, t2)| c2 == c && t2 == t));
        if same && t == ty {
            fields.push((f, c, ty));
        }
    }
    (!fields.is_empty()).then_some(Known { agg: id, fields })
}

/// Replace reads of known fields by their constants; returns whether anything changed.
pub(crate) fn rewrite(func: &mut Function, facts: &Facts) -> bool {
    if facts.direct.is_empty() && facts.pointers.is_empty() {
        return false;
    }
    let mut changed = false;
    let mut replace = |op: &mut Operand| {
        if let Some((c, ty)) = known_read(op, facts) {
            *op = Operand::Const(c, ty);
            changed = true;
        }
    };
    for block in &mut func.blocks {
        for s in &mut block.stmts {
            stmt_operands_mut(s, &mut replace);
        }
        term_operands_mut(&mut block.term, &mut replace);
    }
    changed
}

/// The constant an operand reads, if it reads a known field.
fn known_read(op: &Operand, facts: &Facts) -> Option<(Const, Ty)> {
    let Operand::Copy(place) = op else {
        return None;
    };
    let (known, field) = match place.proj.as_slice() {
        [Proj::Field(f)] => (facts.direct.get(&place.local)?, *f),
        [Proj::Deref(Ty::Agg(id)), Proj::Field(f)] => {
            let known = facts.pointers.get(&place.local)?;
            if known.agg != *id {
                return None;
            }
            (known, *f)
        }
        _ => return None,
    };
    known.get(field).map(|(c, t)| (c.clone(), t))
}
