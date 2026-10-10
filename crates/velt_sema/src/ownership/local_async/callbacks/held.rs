//! Closures held by one local: reached only when that local is (`held_closures`).

use super::*;

/// Closure literals stored straight into an object or array held by one local of the function
/// making them (`other.onChange = (v) => …`, `cbs.push(() => …)`, `const o = { f: () => … }`,
/// or through a setter, `e.on(() => …)` with `on(f) { this.listeners.push(f); }`), by literal
/// node, with that local's node: when the local never leaves the function (it is not stored,
/// captured, returned or passed to a function of the program, only to the standard library's
/// and as `this` of methods that keep it too), only that local reaches the closure, so it is
/// reached exactly when the local is, not by its type. Also returns, per closure, the nodes of
/// all the locals it is stored into that way (where it is stored, whether or not they leave).
#[allow(clippy::type_complexity)]
pub(super) fn held_closures(
    cx: &Ctx,
    g: &Graph,
    flags: &[u8],
) -> (HashMap<usize, usize>, HashMap<DefId, Vec<usize>>) {
    let mut outs: HashMap<usize, Vec<usize>> = HashMap::new();
    for (m, srcs) in g.srcs.iter().enumerate() {
        for &(n, _) in srcs {
            outs.entry(n).or_default().push(m);
        }
    }
    let setters = setters(cx);
    let parents: HashSet<DefId> = g.parent.values().copied().collect();
    let mut out = HashMap::new();
    let mut homes: HashMap<DefId, Vec<usize>> = HashMap::new();
    for (i, def) in cx.defs.iter().enumerate() {
        let d = DefId(i as u32);
        let Some(Def::Fn(f)) = def else { continue };
        if in_std(cx, d) || !parents.contains(&d) {
            continue;
        }
        let mut sites = vec![];
        walk::block(&f.body.block, &mut Sites(cx, &setters, &mut sites));
        for (c, x) in sites {
            let (Some(&lit), Some(&local)) =
                (g.ids.get(&Node::Lit(c)), g.ids.get(&Node::Local(d, x)))
            else {
                continue;
            };
            homes.entry(c).or_default().push(local);
            if (x.0 as usize) < f.params.len() {
                continue;
            }
            if flags[local] & STORED == 0
                && stays(cx, g, &outs, flags, &setters, local, &mut HashSet::new())
                && stays(cx, g, &outs, flags, &setters, lit, &mut HashSet::new())
            {
                out.insert(lit, local);
            }
        }
    }
    (out, homes)
}

/// Do the values of node `n` go only to parameters of standard library functions, to setter
/// parameters (which keep them in their `this`), and as `this` to methods of the program where
/// they stay too?
fn stays(
    cx: &Ctx,
    g: &Graph,
    outs: &HashMap<usize, Vec<usize>>,
    flags: &[u8],
    setters: &HashMap<DefId, Vec<usize>>,
    n: usize,
    seen: &mut HashSet<usize>,
) -> bool {
    if !seen.insert(n) || seen.len() > 256 {
        return seen.len() <= 256;
    }
    outs.get(&n)
        .into_iter()
        .flatten()
        .all(|&m| match g.nodes[m] {
            Node::Local(f, _) if in_std(cx, f) => true,
            Node::Local(f, l) => match &cx.defs[f.0 as usize] {
                Some(Def::Fn(ff)) if ff.self_ty.is_some() && ff.captures.is_empty() => {
                    let k = ff.params.iter().position(|p| p.local == l);
                    (k.is_some_and(|k| setters.get(&f).is_some_and(|ks| ks.contains(&k))))
                        || (k == Some(0)
                            && flags[m] & STORED == 0
                            && stays(cx, g, outs, flags, setters, m, seen))
                }
                _ => false,
            },
            _ => false,
        })
}

/// The methods of the program that only keep some of their parameters in what `this` holds
/// (`on(f) { this.listeners.push(f); }`, `set cb(f) { this.f = f; }`), with those parameters'
/// positions (`this` is 0).
fn setters(cx: &Ctx) -> HashMap<DefId, Vec<usize>> {
    let mut out = HashMap::new();
    for (i, def) in cx.defs.iter().enumerate() {
        let d = DefId(i as u32);
        let Some(Def::Fn(f)) = def else { continue };
        if f.self_ty.is_none() || !f.captures.is_empty() || f.params.len() < 2 || in_std(cx, d) {
            continue;
        }
        let this = f.params[0].local;
        // Per parameter: its uses, and those that keep it in `this`.
        let mut uses: HashMap<LocalId, (u32, u32)> = HashMap::new();
        let mut kept = |x: &Expr| {
            if let E::Local(l, _) = x.kind {
                uses.entry(l).or_default().1 += 1;
            }
        };
        let mut all = vec![];
        each_expr(&f.body.block, &mut |e: &Expr| match &e.kind {
            E::Local(l, _) => all.push(*l),
            E::Assign { place, value }
                if root(place) == Some(this) && !matches!(place.kind, E::Local(..)) =>
            {
                kept(value)
            }
            E::Call { callee, args }
                if std_method(cx, callee) && args.first().and_then(root) == Some(this) =>
            {
                args[1..].iter().for_each(&mut kept)
            }
            _ => {}
        });
        for l in all {
            uses.entry(l).or_default().0 += 1;
        }
        let ks: Vec<usize> = (1..f.params.len())
            .filter(|&k| {
                let p = &f.params[k];
                types::fn_like(cx, p.ty)
                    && uses
                        .get(&p.local)
                        .is_some_and(|(n, kept)| *n == *kept && *kept > 0)
            })
            .collect();
        if !ks.is_empty() {
            out.insert(d, ks);
        }
    }
    out
}

/// The closure literals stored straight into what a local holds, with the local (see
/// [`held_closures`]).
struct Sites<'a, 'm>(
    &'a Ctx<'m>,
    &'a HashMap<DefId, Vec<usize>>,
    &'a mut Vec<(DefId, LocalId)>,
);

impl Visit for Sites<'_, '_> {
    fn stmt(&mut self, s: &Stmt) {
        let S::Let {
            local,
            init: Some(init),
        } = &s.kind
        else {
            return;
        };
        if let E::AdtLit { fields: xs, .. } | E::ArrayLit(xs) | E::New { args: xs, .. } = &init.kind
        {
            for x in xs {
                if let E::Closure(c) = x.kind {
                    self.2.push((c, *local));
                }
            }
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            E::Assign { place, value } => {
                if let (E::Closure(c), false) = (&value.kind, matches!(place.kind, E::Local(..))) {
                    if let Some(x) = root(place) {
                        self.2.push((*c, x));
                    }
                }
            }
            E::Call { callee, args } => {
                let held: Vec<usize> = match callee {
                    _ if std_method(self.0, callee) => (1..args.len()).collect(),
                    Callee::Def(g, _) => self.1.get(g).cloned().unwrap_or_default(),
                    _ => vec![],
                };
                if let Some(x) = args.first().and_then(root).filter(|_| !held.is_empty()) {
                    for k in held {
                        if let Some(E::Closure(c)) = args.get(k).map(|a| &a.kind) {
                            self.2.push((*c, x));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// A method of the standard library (`push`, `set`), which may keep its arguments in what its
/// receiver holds but nowhere else.
fn std_method(cx: &Ctx, callee: &Callee) -> bool {
    match callee {
        Callee::Def(g, _) => {
            in_std(cx, *g)
                && matches!(&cx.defs[g.0 as usize], Some(Def::Fn(gf)) if gf.self_ty.is_some())
        }
        Callee::Intrinsic(Intrinsic::ArrayPush) => true,
        _ => false,
    }
}

/// The local a place is part of: `x`, `x.f`, `x[i]`, `x.f[i].g`.
pub(super) fn root(e: &Expr) -> Option<LocalId> {
    match &e.kind {
        E::Local(l, _) => Some(*l),
        E::Field { base, .. } | E::Index { base, .. } => root(base),
        _ => None,
    }
}
