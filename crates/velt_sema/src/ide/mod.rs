//! Queries for editors (docs/internals/contracts/sema_ide.md): [`check_for_ide`] checks a program like
//! [`crate::check`] — every body, even after errors, and without requiring `main` — while
//! recording side tables ([`record`]) of what each name denotes, the type of each expression and
//! where each local is visible. The resulting [`Analysis`] owns everything it needs, so the
//! queries (definition, type, scope, members, references) are plain lookups. Tools that reason
//! about types (lints) use the structured type query of [`types`] ([`Analysis::type_of`]).

mod defref;
mod display;
mod effects;
mod members;
pub(crate) mod record;
mod snapshot;
mod types;

use std::collections::HashMap;
use std::sync::OnceLock;

use velt_common::{Diagnostic, Diagnostics, FileId, Span};

use crate::hir::TyId;
use crate::SourceModule;

pub use defref::{DefKind, DefRef};
pub use effects::Mutation;
pub use types::{FieldView, LiteralKind, NamedKind, NamedType, TypeRef, TypeView};

/// The intrinsic tags of a JSX runtime: `(tag, field definition, attribute type)`, shared by the
/// files using that runtime.
type JsxTags = std::sync::Arc<[(String, DefRef, String)]>;

/// What a name denotes, the type of each expression, and the visible names — for one program.
pub struct Analysis {
    diagnostics: Diagnostics,
    /// Name uses and declarations → definition.
    refs: Vec<(Span, DefRef)>,
    /// Checked expressions and declared locals → type (display context index).
    types: Vec<(Span, TyId, u32)>,
    /// Generic parameter names per display context.
    contexts: Vec<Vec<String>>,
    /// Locals: file, visible range, name, definition.
    locals: Vec<(FileId, u32, u32, String, DefRef)>,
    /// Items declared inside blocks: file, visible range, name, definition.
    nested: Vec<(FileId, u32, u32, String, DefRef)>,
    /// Per module: its own items and imports.
    module_items: Vec<Vec<(String, DefRef)>>,
    /// Exported prelude items (visible everywhere).
    prelude: Vec<(String, DefRef)>,
    /// Module index of each file.
    files: HashMap<FileId, usize>,
    /// Type of each local / constant / field definition (by declaration span).
    def_types: HashMap<Span, (TyId, u32)>,
    /// Per file with a JSX runtime: its intrinsic tags.
    jsx_tags: HashMap<FileId, JsxTags>,
    /// Inferred throws and mutation of each named function (by declaring identifier).
    effects: HashMap<Span, effects::Effects>,
    names: display::Names,
    members: members::Members,
    /// Exact span → first index in `types`, built on the first [`Analysis::type_of`].
    type_index: OnceLock<HashMap<Span, usize>>,
    /// Exact span → first index in `refs`, built on the first [`Analysis::def_of`].
    ref_index: OnceLock<HashMap<Span, usize>>,
}

/// Check `modules` for an editor: like [`crate::check`], but errors never stop other items from
/// being checked, `modules[root]` need not define `main`, and no HIR is built.
pub fn check_for_ide(modules: &[SourceModule], root: usize) -> Analysis {
    std::thread::scope(|s| {
        let spawned = std::thread::Builder::new()
            .name("velt-sema-ide".into())
            .stack_size(crate::SEMA_STACK_BYTES)
            .spawn_scoped(s, || {
                check_on_current_thread(modules, root, crate::SEMA_STACK_BUDGET)
            });
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(_) => check_on_current_thread(modules, root, crate::FALLBACK_STACK_BUDGET),
        }
    })
}

fn check_on_current_thread(modules: &[SourceModule], root: usize, stack_budget: usize) -> Analysis {
    let lifted = crate::generic_arrows::lift(modules);
    let modules = lifted.as_ref().map_or(modules, |l| &l.modules[..]);
    let mut cx = crate::ctx::Ctx::new(modules, root.min(modules.len().saturating_sub(1)));
    cx.stack_budget = stack_budget;
    if let Some(l) = &lifted {
        cx.generic_arrow_fns = l.local_fns.clone();
        cx.generic_arrow_all = l.all_fns.clone();
    }
    cx.ide = Some(Box::default());
    if !modules.is_empty() {
        crate::analyze(&mut cx);
    }
    snapshot::build(cx)
}

/// A span contains an offset when the offset is inside it or right after its end (a cursor
/// just past an identifier still names it).
fn covers(s: Span, file: FileId, offset: u32) -> bool {
    s.file == file && s.lo <= offset && offset <= s.hi
}

/// The innermost (shortest) of `items` whose span covers the offset.
fn innermost<'a, T>(
    items: impl Iterator<Item = &'a (Span, T)>,
    file: FileId,
    offset: u32,
) -> Option<&'a T>
where
    T: 'a,
{
    items
        .filter(|(s, _)| covers(*s, file, offset))
        .min_by_key(|(s, _)| (s.hi - s.lo, u32::MAX - s.lo))
        .map(|(_, t)| t)
}

impl Analysis {
    /// Every diagnostic of the program (all modules), warnings included.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// The definition the name at `offset` of `file` denotes (a use or the declaration itself).
    pub fn def_at(&self, file: FileId, offset: u32) -> Option<DefRef> {
        innermost(self.refs.iter(), file, offset).cloned()
    }

    /// The definition the name whose span is exactly `span` denotes (a use or the declaration
    /// itself); `None` when no name with that span was recorded. Where several were, the first
    /// recorded wins, as with [`Analysis::def_at`].
    pub fn def_of(&self, span: Span) -> Option<DefRef> {
        let index = self.ref_index.get_or_init(|| {
            let mut index = HashMap::with_capacity(self.refs.len());
            for (i, (s, _)) in self.refs.iter().enumerate() {
                index.entry(*s).or_insert(i);
            }
            index
        });
        self.refs.get(*index.get(&span)?).map(|(_, d)| d.clone())
    }

    /// The type of the innermost expression (or declared local) at `offset`, as source would
    /// spell it.
    pub fn type_at(&self, file: FileId, offset: u32) -> Option<String> {
        let (ty, ctx) = self.type_id_at(file, offset)?;
        Some(self.show(ty, ctx))
    }

    fn type_id_at(&self, file: FileId, offset: u32) -> Option<(TyId, u32)> {
        self.types
            .iter()
            .filter(|(s, _, _)| covers(*s, file, offset))
            .min_by_key(|(s, _, _)| (s.hi - s.lo, u32::MAX - s.lo))
            .map(|(_, t, c)| (*t, *c))
    }

    fn show(&self, ty: TyId, ctx: u32) -> String {
        let names = self.contexts.get(ctx as usize).map_or(&[][..], |v| &v[..]);
        self.names.show_in(ty, names)
    }

    /// Every name visible at `offset` of `file`: locals (innermost first), block-level items,
    /// the module's items and imports, then prelude items. Shadowed names are omitted.
    pub fn scope_at(&self, file: FileId, offset: u32) -> Vec<(String, DefRef)> {
        let mut out: Vec<(String, DefRef)> = vec![];
        let mut push = |name: &str, d: &DefRef| {
            if !out.iter().any(|(n, _)| n == name) {
                out.push((name.to_string(), d.clone()));
            }
        };
        let visible = |(f, lo, hi, _, _): &&(FileId, u32, u32, String, DefRef)| {
            *f == file && *lo <= offset && offset <= *hi
        };
        let mut locals: Vec<_> = self.locals.iter().filter(visible).collect();
        locals.sort_by_key(|(_, lo, _, _, _)| u32::MAX - lo);
        for (_, _, _, name, d) in locals {
            push(name, d);
        }
        let mut nested: Vec<_> = self.nested.iter().filter(visible).collect();
        nested.sort_by_key(|(_, lo, hi, _, _)| hi - lo);
        for (_, _, _, name, d) in nested {
            push(name, d);
        }
        if let Some(&m) = self.files.get(&file) {
            // `ns.x` entries are namespace members ([`Analysis::namespace_members`]).
            for (name, d) in self.module_items[m]
                .iter()
                .filter(|(n, _)| !n.contains('.'))
            {
                push(name, d);
            }
        }
        for (name, d) in &self.prelude {
            push(name, d);
        }
        out
    }

    /// The exports of namespace import `ns` of `file` (`import * as ns from …`), by name.
    pub fn namespace_members(&self, file: FileId, ns: &str) -> Vec<(String, DefRef)> {
        let Some(&m) = self.files.get(&file) else {
            return vec![];
        };
        let prefix = format!("{ns}.");
        self.module_items[m]
            .iter()
            .filter_map(|(n, d)| Some((n.strip_prefix(&prefix)?.to_string(), d.clone())))
            .collect()
    }

    /// Members usable on the expression at `offset` of `file`: `(name, definition, type)`.
    /// On a value: its fields, getters and methods (instance members of its type, including
    /// inherited ones and interface defaults, and `extend` methods). On a type name: its static
    /// members (static methods and fields, enum variants).
    pub fn members_of_type_at(&self, file: FileId, offset: u32) -> Vec<(String, DefRef, String)> {
        if let Some(d) = self.def_at(file, offset) {
            if d.kind.is_type() {
                return self.members.statics(self, &d);
            }
        }
        match self.type_id_at(file, offset) {
            Some((ty, ctx)) => self.members.instance(self, ty, ctx),
            None => vec![],
        }
    }

    /// Members of a definition: the instance members of a local's / constant's / field's type,
    /// or the static members of a type.
    pub fn members_of(&self, def: &DefRef) -> Vec<(String, DefRef, String)> {
        if def.kind.is_type() {
            return self.members.statics(self, def);
        }
        match self.def_types.get(&def.span) {
            Some(&(ty, ctx)) => self.members.instance(self, ty, ctx),
            None => vec![],
        }
    }

    /// The intrinsic tags of the JSX runtime of `file` (the fields of `JSX.IntrinsicElements`):
    /// `(tag, definition, attribute type)`, sorted by tag; empty when the file has no JSX
    /// runtime. `members_of` on a tag's definition lists its attributes.
    pub fn jsx_intrinsics(&self, file: FileId) -> &[(String, DefRef, String)] {
        self.jsx_tags.get(&file).map_or(&[], |tags| tags)
    }

    /// What the function, method, constructor or closure-valued variable `def` throws
    /// (written or inferred), spelled as source; `None` when it throws nothing or `def` is not
    /// one.
    pub fn throws_of(&self, def: &DefRef) -> Option<&str> {
        self.effects.get(&def.span)?.throws.as_deref()
    }

    /// What the function, method or constructor `def` modifies (as inferred: `this`,
    /// parameters); `None` when `def` is not one (closures included: their parameters follow
    /// the callback convention, not inference).
    pub fn mutation_of(&self, def: &DefRef) -> Option<&Mutation> {
        self.effects.get(&def.span)?.mutation.as_ref()
    }

    /// Every span naming `def` (its declaration included), in source order.
    pub fn references(&self, def: &DefRef) -> Vec<Span> {
        let mut spans: Vec<Span> = self
            .refs
            .iter()
            .filter(|(_, d)| d.same_def(def))
            .map(|(s, _)| *s)
            .collect();
        spans.sort_by_key(|s| (s.file, s.lo, s.hi));
        spans.dedup();
        spans
    }
}
