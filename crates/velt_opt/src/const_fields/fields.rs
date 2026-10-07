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
//!
//! Fields that are aggregates themselves are known too when every definition copies the same
//! known aggregate local into them (`o = { true, closure }`, an optional function value):
//! reads along a field path (`o.1.0`, `(*p).1.0`) and pointers to a field (`p = &o.1`, the
//! narrowed payload of `f: F | null`) see the inner aggregate's constants.

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
    /// Aggregate fields with known fields of their own: (field, what is known about it).
    pub nested: Vec<(u32, Known)>,
}

impl Known {
    /// Whether some known field is a function address (worth specializing callees for).
    pub fn has_code(&self) -> bool {
        self.fields
            .iter()
            .any(|(_, c, _)| matches!(c, Const::Func(_)))
            || self.nested.iter().any(|(_, k)| k.has_code())
    }

    fn nested_at(&self, field: u32) -> Option<&Known> {
        self.nested
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, k)| k)
    }

    /// What is known about the aggregate at the end of the field path `path`.
    fn at_path(&self, path: &[u32]) -> Option<&Known> {
        path.iter().try_fold(self, |k, &f| k.nested_at(f))
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
    /// Pointer locals assigned `&a` or `&a.f.g…` (with that field path); `None` for any
    /// other way the address was taken.
    pointers: Vec<Option<(Local, Vec<u32>)>>,
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
    let mut pointers: HashMap<Local, Vec<(Local, Vec<u32>)>> = HashMap::new();
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
        for (p, path) in &pointers[&a] {
            if let Some(inner) = k.at_path(path) {
                facts.pointers.insert(*p, inner.clone());
            }
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
        // Copies of candidates, whole or into a field of an aggregate rvalue; each counted
        // once. A local copied into itself depends on nothing it does not already know.
        let deps: HashSet<Local> = obs
            .defs
            .iter()
            .flat_map(|d| match d {
                Def::Copy(b) => vec![*b],
                Def::Aggregate(ops) => ops.iter().filter_map(copied_local).collect(),
                Def::Unknown => vec![],
            })
            .filter(|b| seen.contains_key(b))
            .collect();
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
                let path: Option<Vec<u32>> = q
                    .proj
                    .iter()
                    .map(|p| match p {
                        Proj::Field(f) => Some(*f),
                        _ => None,
                    })
                    .collect();
                let path = path.filter(|_| dst.proj.is_empty());
                obs.pointers.push(path.map(|path| (dst.local, path)));
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
        .flat_map(|obs| obs.pointers.iter().flatten().map(|(p, _)| *p))
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
) -> Option<Vec<(Local, Vec<u32>)>> {
    let mut out = Vec::with_capacity(obs.pointers.len());
    for p in &obs.pointers {
        let (p, path) = p.clone()?;
        let single = usage.is_register(p) && usage.get(p).defs == 1;
        let is_param = (p.0 as usize) < func.params.len();
        if !single || is_param || !read_only.contains(&p) {
            return None;
        }
        out.push((p, path));
    }
    Some(out)
}

/// What a definition stores into a field, when it is known.
#[derive(Clone, PartialEq)]
enum FieldValue {
    Scalar(Const, Ty),
    Agg(Known),
}

/// The local an operand copies whole, if any.
fn copied_local(op: &Operand) -> Option<Local> {
    match op {
        Operand::Copy(p) if p.proj.is_empty() => Some(p.local),
        _ => None,
    }
}

/// What a definition stores into field `f`, if it is known.
fn def_field(
    def: &Def,
    f: u32,
    decided: &HashMap<Local, Option<Known>>,
    id: AggId,
) -> Option<FieldValue> {
    match def {
        Def::Aggregate(ops) => match ops.get(f as usize)? {
            Operand::Const(c, t) => Some(FieldValue::Scalar(c.clone(), *t)),
            op => {
                let known = decided.get(&copied_local(op)?)?.as_ref()?;
                Some(FieldValue::Agg(known.clone()))
            }
        },
        Def::Copy(b) => {
            let known = decided.get(b)?.as_ref()?;
            if known.agg != id {
                return None;
            }
            match known.get(f) {
                Some((c, t)) => Some(FieldValue::Scalar(c.clone(), t)),
                None => known.nested_at(f).cloned().map(FieldValue::Agg),
            }
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
    let (mut fields, mut nested) = (Vec::new(), Vec::new());
    for (f, &(ty, _)) in layout.fields.iter().enumerate() {
        let f = f as u32;
        if obs.dirty.contains(&Some(f)) {
            continue;
        }
        let Some(v) = def_field(&obs.defs[0], f, decided, id) else {
            continue;
        };
        let same = obs.defs[1..]
            .iter()
            .all(|d| def_field(d, f, decided, id).as_ref() == Some(&v));
        match v {
            FieldValue::Scalar(c, t) if same && ty.is_scalar() && t == ty => {
                fields.push((f, c, ty))
            }
            FieldValue::Agg(k) if same && ty == Ty::Agg(k.agg) => nested.push((f, k)),
            _ => {}
        }
    }
    (!fields.is_empty() || !nested.is_empty()).then_some(Known {
        agg: id,
        fields,
        nested,
    })
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
    let (known, path) = match place.proj.as_slice() {
        [Proj::Deref(Ty::Agg(id)), path @ ..] => {
            let known = facts.pointers.get(&place.local)?;
            if known.agg != *id {
                return None;
            }
            (known, path)
        }
        path => (facts.direct.get(&place.local)?, path),
    };
    // A field path `.f`, `.f.g`, ..., through known aggregate fields to a scalar one.
    let (Proj::Field(field), outer) = path.split_last()? else {
        return None;
    };
    let mut known = known;
    for p in outer {
        let Proj::Field(f) = p else { return None };
        known = known.nested_at(*f)?;
    }
    known.get(*field).map(|(c, t)| (c.clone(), t))
}
