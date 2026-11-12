//! What a call can store (#1527, index_order.rs): an element read holds its base against an
//! index only when the code the index runs can store into a link of the base's place. Each
//! function's writes are summarized once for the program, with what the functions it calls
//! write: the module globals it assigns, the fields it assigns (by the root of a class
//! hierarchy, as `AssignedFields` keys them), whether it stores into an element of an array, and
//! `all` when it may run code not known here (a call through a function value, an interface or
//! a virtual slot, `any`, an `await`, which lets other tasks run, or a nested function that
//! assigns a variable it captured). A function that only counts calls (`calls++`) or computes a
//! value stores into no array's place. The hook methods the generated code calls with no call
//! in the program's own code (`[Symbol.dispose]` when a value is dropped, `toString`, `toJSON`,
//! the inspect hook when a value is turned into text) may run in any code: what they store is
//! added to what every piece of code may store.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_sema::hir::{self, AdtKind, Intrinsic, TyKind};

use super::index_order::root_of;
use super::module_state::{walk_expr_writes, walk_writes};
use super::FnLower;

/// What evaluating some code may store.
#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct Writes {
    /// Anything: code not known here may run.
    pub(super) all: bool,
    /// An element of some array.
    pub(super) element: bool,
    /// A field known only by its key at run time, or of a value that is not an object type.
    pub(super) any_field: bool,
    /// Module globals assigned as a whole.
    pub(super) globals: FxHashSet<hir::DefId>,
    /// Fields assigned: the definition (a class hierarchy's root) and the field's index.
    pub(super) fields: FxHashSet<(hir::DefId, u32)>,
}

impl Writes {
    /// Add what `o` may store; whether that changed anything.
    fn add(&mut self, o: &Writes) -> bool {
        if self.all {
            return false;
        }
        if o.all {
            *self = Writes {
                all: true,
                ..Writes::default()
            };
            return true;
        }
        let before = (
            self.element,
            self.any_field,
            self.globals.len(),
            self.fields.len(),
        );
        self.element |= o.element;
        self.any_field |= o.any_field;
        self.globals.extend(o.globals.iter().copied());
        self.fields.extend(o.fields.iter().copied());
        before
            != (
                self.element,
                self.any_field,
                self.globals.len(),
                self.fields.len(),
            )
    }

    /// The direct writes of the expression `e` (not into its children), and the functions it
    /// calls into `calls`; `captures`: the variables of an enclosing function it captured.
    fn direct(
        &mut self,
        hir: &hir::Program,
        e: &hir::Expr,
        captures: &[hir::LocalId],
        calls: &mut Vec<hir::DefId>,
    ) {
        use hir::ExprKind as H;
        match &e.kind {
            H::Assign { place, .. } | H::CompoundAssign { place, .. } => {
                self.place(hir, place, captures)
            }
            // A constructor's body and field initializers: whatever they run.
            H::New { .. } => self.all = true,
            H::Call { callee, args } => match callee {
                hir::Callee::Def(d, _) => match hir.def(*d) {
                    hir::Def::Fn(_) => calls.push(*d),
                    // The runtime's functions change arrays and objects in place only, through
                    // the pointers they are given.
                    _ => {
                        self.element = true;
                        self.any_field = true;
                    }
                },
                hir::Callee::Intrinsic(i) => self.intrinsic(*i, args),
                _ => self.all = true,
            },
            H::Await(_) => self.all = true,
            _ => {}
        }
    }

    /// A call of the intrinsic `i` with `args`: one that runs code (a generator resumed, a
    /// callback it calls) may store anything; one that changes a place it is passed (`pop`,
    /// `swap`, `remove` on an array) stores into that place's elements or fields.
    fn intrinsic(&mut self, i: Intrinsic, args: &[hir::Expr]) {
        use Intrinsic as I;
        let runs = matches!(
            i,
            I::SharedSet
                | I::ThreadLocalSet
                | I::MutexWith
                | I::LockRead
                | I::Attempt
                | I::Yield
                | I::YieldNow
                | I::YieldDelegate
                | I::GeneratorResume
                | I::GeneratorResumeWith
                | I::GeneratorThrow
                | I::GeneratorReturn
                | I::AsyncGeneratorResume
                | I::AsyncGeneratorReturn
                | I::AsyncGeneratorDispose
        );
        if runs || velt_sema::effects::runs_user_code(i) {
            self.all = true;
        } else if i == I::KeyWrite {
            self.any_field = true;
        } else if i == I::ArraySetLen || args.iter().any(velt_sema::effects::writes_place) {
            self.element = true;
            self.any_field = true;
        }
    }

    /// A store into `place`.
    fn place(&mut self, hir: &hir::Program, place: &hir::Expr, captures: &[hir::LocalId]) {
        use hir::ExprKind as H;
        let mut p = place;
        while let H::UnwrapSome(x, _) | H::UnwrapVariant { expr: x, .. } | H::Downcast(x) = &p.kind
        {
            p = x;
        }
        match &p.kind {
            H::Global(d) => {
                self.globals.insert(*d);
            }
            H::Local(l, _) => self.all |= captures.contains(l),
            H::Field { base, index, .. } => match hir.types.kind(base.ty) {
                TyKind::Adt(d, _) if !matches!(hir.def(*d), hir::Def::Adt(a) if a.kind == AdtKind::Anon) =>
                {
                    self.fields.insert((root_of(hir, *d), *index));
                }
                _ => self.any_field = true,
            },
            H::Index { .. } => self.element = true,
            _ => self.all = true,
        }
    }
}

/// What each function may store, with what the functions it calls store (a fixpoint over the
/// call graph, so recursion is covered), and what the hook methods store together.
#[derive(PartialEq, Eq)]
pub(super) struct FnWrites {
    fns: FxHashMap<hir::DefId, Writes>,
    hooks: Writes,
}

/// [`FnWrites`] of the program.
pub(super) fn fn_writes(hir: &hir::Program) -> FnWrites {
    let mut writes: FxHashMap<hir::DefId, Writes> = FxHashMap::default();
    let mut calls: FxHashMap<hir::DefId, Vec<hir::DefId>> = FxHashMap::default();
    let captures = |d: hir::DefId| -> Vec<hir::LocalId> {
        match hir.def(d) {
            hir::Def::Fn(f) => f.captures.iter().map(|c| c.inner).collect(),
            _ => vec![],
        }
    };
    let mut caps: FxHashMap<hir::DefId, Vec<hir::LocalId>> = FxHashMap::default();
    walk_writes(hir, &mut |d, e| {
        let c = caps.entry(d).or_insert_with(|| captures(d));
        let w = writes.entry(d).or_default();
        w.direct(hir, e, c, calls.entry(d).or_default());
    });
    for cs in calls.values_mut() {
        cs.sort_unstable();
        cs.dedup();
    }
    let mut changed = true;
    while changed {
        changed = false;
        for (d, cs) in &calls {
            let mut acc = writes.remove(d).unwrap_or_default();
            for c in cs {
                if let Some(cw) = writes.get(c) {
                    changed |= acc.add(cw);
                }
            }
            writes.insert(*d, acc);
        }
    }
    let mut hooks = Writes::default();
    for d in &hir.defs {
        if let hir::Def::Adt(a) = d {
            for h in [a.dispose, a.to_string, a.to_json, a.inspect]
                .into_iter()
                .flatten()
            {
                if let Some(w) = writes.get(&h) {
                    hooks.add(w);
                }
            }
        }
    }
    FnWrites { fns: writes, hooks }
}

impl FnLower<'_, '_> {
    /// What evaluating `e` may store (its own assignments to variables of this function aside:
    /// `index_base` looks at those itself).
    pub(super) fn writes_of(&mut self, e: &hir::Expr) -> Writes {
        let hir = self.cx.hir;
        let mut w = Writes::default();
        // A field read of a proxy, or an element read of an array view, may run any code.
        if self.reads_may_run_code() {
            w.all = true;
            return w;
        }
        let mut calls = vec![];
        walk_expr_writes(e, &mut |x| w.direct(hir, x, &[], &mut calls));
        let table = self.cx.fn_writes.get_or_insert_with(|| fn_writes(hir));
        for c in calls {
            if let Some(cw) = table.fns.get(&c) {
                w.add(cw);
            }
        }
        w.add(&table.hooks);
        w
    }
}
