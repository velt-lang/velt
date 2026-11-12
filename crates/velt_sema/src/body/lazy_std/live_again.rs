//! The live set walked again after a kept check's reload (crate `kept`).

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::DefId;

/// A kept check's reload (crate `kept`) checked the bodies of `defs` again, which the live set
/// walked as they were: the program's live code is walked again, `defs` included, and what it
/// reaches with it ([`super::super::reach`]). Whether that changed what is live, or what lowering is
/// told to build: anything new (std code checked or demanded late, a module it cannot load)
/// needs the cold check, which checks it in its place.
pub(crate) fn reach_again(cx: &mut Ctx, defs: &[DefId]) -> bool {
    if cx.lazy.modules.is_empty() {
        return cx.std_missed;
    }
    // The program's own code newly live (a closure the edit adds) is checked already; std's
    // is what the kept check may not have checked, and what lowering is told to build.
    let state = |cx: &Ctx| {
        let l = &cx.lazy.live;
        let late = &l.late;
        let std_code = |d: DefId| match &cx.info[d.0 as usize] {
            DefInfo::Fn(f) => cx.modules[f.module].is_std,
            DefInfo::Global(g) => cx.modules[g.module].is_std,
            _ => false,
        };
        let std_live = (l.live.iter()).filter(|&&d| std_code(d)).count();
        (
            (
                std_live,
                l.present,
                l.demanded,
                l.prints_float,
                l.waiting.len(),
            ),
            (cx.info.len(), cx.loads_added, cx.lazy.record_hooks),
            (late.defs.len(), late.constructs, late.record_conversion),
        )
    };
    let before = state(cx);
    let mut todo: Vec<DefId> = (cx.lazy.live.live.iter().copied())
        .filter(|&d| super::program_code(cx, d))
        .collect();
    todo.extend(defs.iter().copied());
    todo.sort_unstable_by_key(|d| d.0);
    todo.dedup();
    for d in todo {
        cx.lazy.live.live.insert(d);
        cx.lazy.live.todo.push(d);
    }
    super::super::reach(cx);
    state(cx) != before || cx.std_missed
}
