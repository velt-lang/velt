//! The generated phase of a check (docs/internals/design/source-generators.md §6): the
//! generated modules the driver added ([`super::GenInput`]) are collected and checked once the
//! program's bodies are, when the instances they were generated for can be found again.
//!
//! Each instance a generator wrote names its types by handles ([`super::handles`]): positions
//! in the walk from the type arguments of a demanded instance, identified by its fingerprint.
//! So the phase computes the demand, walks each demanded instance's types, and then collects
//! and checks the generated modules whose instances are all demanded. Their code may demand
//! more instances (generated in a later round, whose module waits for that demand), so it
//! repeats until no module becomes ready. A module whose instances are not demanded any more
//! (offered from an earlier build for types that changed) is left out, and the instances its
//! code would have bound are missing, so they are generated again.

use rustc_hash::FxHashMap;

use super::{demand, handles, model, GenInstance};
use crate::ctx::{Ctx, Item};
use crate::hir::{DefId, TyId};

/// The key of each instance in `found` (its fingerprint, with `-<n>` for the `n`th other
/// instance of the same fingerprint, in demand order), with its walk's types.
pub(crate) fn keys(
    cx: &mut Ctx,
    builder: &mut model::Builder,
    found: &[demand::Instance],
) -> Vec<(String, Vec<u32>, Vec<TyId>)> {
    let mut sigs: FxHashMap<DefId, String> = FxHashMap::default();
    let mut seen: FxHashMap<String, u32> = FxHashMap::default();
    let mut out = vec![];
    for (g, targs, _) in found {
        let sig = match sigs.get(g) {
            Some(s) => s.clone(),
            None => {
                let s = super::gen_function(cx, *g).map(|f| f.signature());
                let s = s.unwrap_or_default();
                sigs.insert(*g, s.clone());
                s
            }
        };
        let (roots, tys) = handles::walk(builder, cx, targs);
        let fp = builder.model.fingerprint(&roots, &sig);
        let n = seen.entry(fp.clone()).or_insert(0);
        let key = match *n {
            0 => fp,
            n => format!("{fp}-{n}"),
        };
        *n += 1;
        out.push((key, roots, tys));
    }
    out
}

/// Collect, check and bind the generated modules of `cx.gen`, and, when the driver generates
/// during the check (`Ctx::generate`), the code of the instances still missing then.
pub(crate) fn run(cx: &mut Ctx) {
    let has_modules = cx.gen.as_ref().is_some_and(|g| !g.modules.is_empty());
    if !(has_modules || cx.generate.is_some()) || !cx.has_generated_fns() {
        return;
    }
    let mut pending: Vec<(usize, String)> = match &cx.gen {
        Some(gen) => (gen.modules.iter())
            .filter_map(|gm| Some((index(cx, &gm.path)?, gm.path.clone())))
            .collect(),
        None => vec![],
    };
    let mut builder = model::Builder::new();
    loop {
        let found = crate::timings::pass("generated::demand", || demand::compute(cx));
        let keyed = crate::timings::pass("generated::keys", || keys(cx, &mut builder, &found));
        for (key, _, tys) in keyed {
            cx.gen_binding.handles.walks.entry(key).or_insert(tys);
        }
        let gen = cx.gen.clone().unwrap_or_default();
        let walks = &cx.gen_binding.handles.walks;
        let demanded =
            |i: &&GenInstance| i.fingerprint.is_empty() || walks.contains_key(&i.fingerprint);
        let (ready, waiting): (Vec<_>, Vec<_>) = pending.into_iter().partition(|(_, path)| {
            (gen.instances.iter())
                .filter(|i| i.body_module == *path)
                .all(|i| demanded(&i))
        });
        pending = waiting;
        if !ready.is_empty() {
            let ms: Vec<usize> = ready.iter().map(|(m, _)| *m).collect();
            let defs = crate::timings::pass("generated::collect", || {
                crate::collect::collect_generated(cx, &ms)
            });
            crate::timings::pass("generated::check", || crate::body::check_generated(cx, &defs));
            for (_, path) in &ready {
                for inst in gen.instances.iter().filter(|i| i.body_module == *path) {
                    bind(cx, inst);
                }
            }
            continue;
        }
        // Nothing more to collect: the driver generates what is still missing, if it can now.
        let Some(generator) = cx.generate else {
            break;
        };
        let bound = std::mem::take(&mut cx.gen_binding.bound);
        let demand = super::demand_of(cx, &found, &bound);
        cx.gen_binding.bound = bound;
        match demand {
            Some(d) if !d.requests.is_empty() => {
                if !generate(cx, generator, &d, &mut pending) {
                    break;
                }
            }
            _ => break,
        }
    }
    if !pending.is_empty() {
        cx.gen_binding.stale = true;
    }
}

fn index(cx: &Ctx, path: &str) -> Option<usize> {
    cx.modules.iter().position(|m| m.path == path)
}

/// Have the driver generate the code `demand` asks for, and add its modules to the check
/// (pending collection): false when it could not, or made code that needs the source rewrites a
/// check makes before it starts (`needs_rewrite`), so the driver checks again with it.
fn generate<'m>(
    cx: &mut Ctx<'m>,
    generator: &'m super::Generator<'m>,
    demand: &super::Demand,
    pending: &mut Vec<(usize, String)>,
) -> bool {
    let Some(batch) = (generator.run)(demand) else {
        generator.state.borrow_mut().failed = true;
        return false;
    };
    if batch.sources.iter().any(|m| needs_rewrite(&m.ast)) {
        generator.state.borrow_mut().restart = true;
        return false;
    }
    for src in batch.sources {
        let declaring = (batch.input.modules.iter())
            .find(|g| g.path == src.path)
            .and_then(|g| index(cx, &g.declaring));
        let Some(declaring) = declaring else {
            generator.state.borrow_mut().restart = true;
            return false;
        };
        let path = src.path.clone();
        let Some(kept) = generator.keep(src, declaring) else {
            generator.state.borrow_mut().restart = true;
            return false;
        };
        let m = cx.add_module(kept, declaring);
        pending.push((m, path));
    }
    let gen = std::rc::Rc::make_mut(cx.gen.get_or_insert_with(Default::default));
    gen.extend(batch.input.clone());
    generator.state.borrow_mut().input.extend(batch.input);
    true
}

/// Whether generated module `m` holds something one of the source rewrites a check makes before
/// it starts would change (`crate::check_timed`): a closure, a nested declaration, `globalThis`,
/// `Object.defineProperty`, an item other than a function, a constant or a type alias, or two
/// declarations of one name. Such code is checked in a check of its own.
pub(crate) fn needs_rewrite(m: &velt_syntax::ast::Module) -> bool {
    use velt_syntax::ast::{ExprKind, ItemKind, StmtKind};
    use velt_syntax::visit::{walk_module, Visit};
    struct Finder(bool);
    impl<'a> Visit<'a> for Finder {
        fn stmt(&mut self, s: &'a velt_syntax::ast::Stmt) {
            self.0 |= matches!(s.kind, StmtKind::Item(_));
        }
        fn expr(&mut self, e: &'a velt_syntax::ast::Expr) {
            self.0 |= match &e.kind {
                ExprKind::Arrow { .. } | ExprKind::Function(_) => true,
                ExprKind::Ident(id) => id.name == "globalThis" || id.name == "defineProperty",
                ExprKind::Member { prop, .. } => {
                    prop.name == "defineProperty" || prop.name == "defineProperties"
                }
                _ => false,
            };
        }
    }
    let mut names = rustc_hash::FxHashSet::default();
    for item in &m.items {
        let name = match &item.kind {
            ItemKind::Function(f) => &f.sig.name.name,
            ItemKind::TypeAlias(a) => &a.name.name,
            ItemKind::Var(_) => continue,
            _ => return true,
        };
        if !names.insert(name.as_str()) {
            return true;
        }
    }
    let mut f = Finder(false);
    walk_module(m, &mut f);
    f.0
}

/// Bind instance `inst` (of a collected module) to its generated body.
fn bind(cx: &mut Ctx, inst: &GenInstance) {
    let index = |cx: &Ctx, path: &str| cx.modules.iter().position(|m| m.path == path);
    let def = |cx: &Ctx, m: usize, name: &str| match cx.scopes[m].items.get(name) {
        Some(Item::Def(d)) => Some(*d),
        _ => None,
    };
    let f = index(cx, &inst.module).and_then(|m| def(cx, m, &inst.function));
    let body = index(cx, &inst.body_module).and_then(|m| def(cx, m, &inst.body));
    let (Some(f), Some(body)) = (f, body) else {
        cx.err(
            format!(
                "ICE: generated code names an instance `{}` of `{}` that is not there",
                inst.body, inst.function
            ),
            velt_common::Span::DUMMY,
        );
        return;
    };
    let targs: Option<Vec<TyId>> = (inst.type_args.iter())
        .map(|h| {
            let (key, i) = handles::parse(h)?;
            cx.gen_binding.handles.get(&key, i)
        })
        .collect();
    match targs {
        Some(targs) => {
            cx.gen_binding.bound.insert((f, targs), body);
        }
        None => cx.gen_binding.stale = true,
    }
}
