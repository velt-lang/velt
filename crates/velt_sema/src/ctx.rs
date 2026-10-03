//! Whole-program checking context shared by all passes: definitions, module scopes, types.

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Diagnostics, Span};

use crate::defs::{
    AdtInfo, AliasInfo, DefInfo, EnumInfo, Extension, FnInfo, GlobalInfo, IfaceInfo,
};
use crate::hir::{AdtKind, Def, DefId, ImplDef, TyId, TyKind};
use crate::types::{int_name, Types};
use crate::SourceModule;

/// A name visible at module level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Def(DefId),
    Alias(u32),
}

#[derive(Default)]
pub(crate) struct ModuleScope {
    /// Own items and imports.
    pub items: HashMap<String, Item>,
    /// Names declared with `export` (lists and re-exports: `crate::collect::exports`).
    pub exports: HashSet<String>,
    /// `import * as ns`: namespace name → module index (its exports are bound as `ns.x`).
    pub namespaces: HashMap<String, usize>,
    /// Names bound by `import type` (types only: using one as a value is an error).
    pub type_only: HashSet<String>,
    /// `std/...` module: may call `__intrinsic_*`.
    pub is_std: bool,
}

pub(crate) struct Ctx<'m> {
    pub modules: &'m [SourceModule],
    pub root: usize,
    pub ty: Types,
    /// Final HIR definitions, indexed by `DefId` (filled at the end).
    pub defs: Vec<Option<Def>>,
    /// Sema information per `DefId`.
    pub info: Vec<DefInfo<'m>>,
    pub def_spans: Vec<Span>,
    pub scopes: Vec<ModuleScope>,
    /// Exports of `std/prelude/*` modules, visible everywhere.
    pub prelude: HashMap<String, Item>,
    pub aliases: Vec<AliasInfo<'m>>,
    pub extensions: Vec<Extension>,
    pub impls: Vec<ImplDef>,
    /// `impls` by interface (`Ctx::find_impl`).
    pub impl_index: crate::infer::ImplIndex,
    /// Anonymous object types by shape.
    /// Anonymous object defs by shape: field names, types and `readonly` flags, in order.
    pub anon: HashMap<Vec<(String, TyId, bool)>, DefId>,
    /// Object type defs replaced before lowering (`crate::readonly`): anonymous ones with
    /// `readonly` fields → their twin without, and field-only interfaces' object types → the
    /// anonymous object type of their fields. With a template, the twin's type arguments are
    /// the template's types with the original arguments substituted (a param order change).
    pub readonly_twins: HashMap<DefId, (DefId, Option<Vec<TyId>>)>,
    /// Field-only interface → its object type's def (`collect::field_only`).
    pub field_only: HashMap<DefId, DefId>,
    /// The reverse of `field_only`.
    pub field_only_of: HashMap<DefId, DefId>,
    /// Every declared type's fields are resolved (`collect::shapes`); before that, a utility
    /// type (`crate::utility_types`) may only read the fields of types already shaped.
    pub shapes_done: bool,
    /// Union enums by canonical member list (`crate::unions`).
    pub unions: HashMap<Vec<TyId>, DefId>,
    /// Names of type aliases for structural types (`type Shape = A | B`), for messages.
    pub alias_names: HashMap<TyId, String>,
    /// (class, method key) of every `override` of a generic method (no vtable slot).
    pub generic_overrides: Vec<(DefId, String)>,
    /// Names of `Param(i)` in the definition being checked (for messages).
    pub display_params: Vec<String>,
    /// Functions whose parameter defaults are checked (or being checked): they are checked on
    /// first use, since a field default or constant may call with fewer arguments.
    pub defaults_checked: HashSet<DefId>,
    /// Types whose field defaults are checked (or being checked): checked up front, or on first
    /// use by a `new` or struct literal in a default checked before them.
    pub field_defaults_checked: HashSet<DefId>,
    /// Closure counters per top-level function name.
    pub closure_counts: HashMap<String, u32>,
    /// Every function-like def, in creation order.
    pub fn_defs: Vec<DefId>,
    /// Named functions used as values (`FnRef`) with their type arguments, checked after
    /// ownership inference.
    pub fn_values: Vec<(DefId, Vec<TyId>, Span)>,
    /// Dispatch groups for error types (`crate::throws`), built on first use.
    pub groups: Option<crate::throws::Groups>,
    /// Error types committed to while checking bodies, re-checked after inference.
    pub throw_checks: Vec<crate::throws::ThrowCheck>,
    /// Items declared inside blocks, visible by name within their block.
    pub nested: Vec<crate::collect::NestedItem>,
    /// For each nested definition: names bound in its enclosing functions (for the
    /// "nested functions cannot capture" error).
    pub nested_locals: HashMap<DefId, Vec<String>>,
    /// Name spans of the nested functions made from local generic arrows (`generic_arrows`).
    pub generic_arrow_fns: HashSet<Span>,
    /// Name spans of every function made from a generic arrow, module-level ones included
    /// (diagnostics show them as arrows).
    pub generic_arrow_all: HashSet<Span>,
    /// The JSX runtime of each module that uses JSX, resolved on first use (`None` after its
    /// errors were reported).
    pub jsx_providers: HashMap<usize, Option<std::rc::Rc<crate::body::expr::jsx::Provider>>>,
    /// JSX component adapters, checked after ownership inference.
    pub jsx_adapters: Vec<crate::body::expr::jsx::Adapter>,
    /// Side tables for [`crate::ide`] (`None` when compiling).
    pub ide: Option<Box<crate::ide::record::Recorder>>,
    /// Memoized `Ctx::is_shared_value` answers (asked for every local of every body).
    pub shared_memo: HashMap<TyId, bool>,
    /// Set while [`Ctx::match_context`] runs (`crate::infer`).
    pub matching_context: bool,
    /// Function bodies being checked, outermost first (a return type inferred from a body that
    /// is on this stack refers to itself: `body::returns`).
    pub checking: Vec<DefId>,
    /// Signature comparisons waiting for inferred result types (`body::returns`).
    pub ret_checks: Vec<crate::defs::RetCheck>,
    /// Uses of functions whose return types are being inferred (`body::recursion`).
    pub rec: crate::body::recursion::RecState,
    pub diags: Diagnostics,
}

impl<'m> Ctx<'m> {
    pub fn new(modules: &'m [SourceModule], root: usize) -> Self {
        Ctx {
            modules,
            root,
            ty: Types::new(),
            defs: vec![],
            info: vec![],
            def_spans: vec![],
            scopes: modules
                .iter()
                .map(|m| ModuleScope {
                    is_std: m.is_std,
                    ..Default::default()
                })
                .collect(),
            prelude: HashMap::new(),
            aliases: vec![],
            extensions: vec![],
            impls: vec![],
            impl_index: Default::default(),
            anon: HashMap::new(),
            readonly_twins: HashMap::new(),
            field_only: HashMap::new(),
            field_only_of: HashMap::new(),
            shapes_done: false,
            unions: HashMap::new(),
            alias_names: HashMap::new(),
            generic_overrides: vec![],
            display_params: vec![],
            defaults_checked: HashSet::new(),
            field_defaults_checked: HashSet::new(),
            closure_counts: HashMap::new(),
            fn_defs: vec![],
            fn_values: vec![],
            groups: None,
            throw_checks: vec![],
            nested: vec![],
            nested_locals: HashMap::new(),
            generic_arrow_fns: HashSet::new(),
            generic_arrow_all: HashSet::new(),
            jsx_providers: HashMap::new(),
            jsx_adapters: vec![],
            ide: None,
            shared_memo: HashMap::new(),
            matching_context: false,
            checking: vec![],
            ret_checks: vec![],
            rec: Default::default(),
            diags: vec![],
        }
    }

    pub fn error(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    pub fn err(&mut self, msg: impl Into<String>, span: Span) {
        self.diags.push(Diagnostic::error(msg, span));
    }

    pub fn alloc_def(&mut self, span: Span, info: DefInfo<'m>) -> DefId {
        let id = DefId(self.defs.len() as u32);
        if matches!(info, DefInfo::Fn(_)) {
            self.fn_defs.push(id);
        }
        self.defs.push(None);
        self.info.push(info);
        self.def_spans.push(span);
        id
    }

    pub fn fn_info(&self, d: DefId) -> &FnInfo<'m> {
        match &self.info[d.0 as usize] {
            DefInfo::Fn(f) => f,
            _ => panic!("ICE: def {d:?} is not a function"),
        }
    }

    pub fn fn_info_mut(&mut self, d: DefId) -> &mut FnInfo<'m> {
        match &mut self.info[d.0 as usize] {
            DefInfo::Fn(f) => f,
            _ => panic!("ICE: def {d:?} is not a function"),
        }
    }

    pub fn try_fn(&self, d: DefId) -> Option<&FnInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Fn(f) => Some(f),
            _ => None,
        }
    }

    pub fn adt(&self, d: DefId) -> Option<&AdtInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Adt(a) => Some(a),
            _ => None,
        }
    }

    /// The visibility of the constructor `new C(...)` of class `d` runs: its own, or the one it
    /// inherits from the nearest base class declaring one (public without any).
    pub fn ctor_visibility(&self, d: DefId) -> velt_syntax::ast::CtorVisibility {
        self.adt(d)
            .and_then(|a| a.ctor)
            .and_then(|c| self.fn_info(c).owner)
            .and_then(|owner| self.adt(owner)?.decl)
            .map_or_else(Default::default, |decl| decl.ctor_visibility)
    }

    /// The public static methods of class `d` that return a `d` (its factories), as
    /// `` `C.of(...)` or `C.parse(...)` ``; `None` without any.
    pub fn factories(&self, d: DefId) -> Option<String> {
        let a = self.adt(d)?;
        let mut names: Vec<String> = a
            .methods
            .iter()
            .filter(|(_, m)| m.is_static && !self.fn_info(m.def).is_private)
            .filter(|(_, m)| {
                matches!(self.ty.kind(self.fn_info(m.def).ret), TyKind::Adt(r, _) if *r == d)
            })
            .map(|(name, _)| format!("`{}.{name}(...)`", a.name))
            .collect();
        names.sort();
        let last = names.pop()?;
        Some(match names.is_empty() {
            true => last,
            false => format!("{} or {last}", names.join(", ")),
        })
    }

    /// A note saying how code outside class `d` creates one: `new`, its factories, or (for a
    /// class of the user's own with a private or protected constructor and no factory) adding
    /// a factory.
    pub fn creation_note(&self, d: DefId) -> String {
        use velt_syntax::ast::CtorVisibility;
        let Some(a) = self.adt(d) else {
            return String::new();
        };
        let name = &a.name;
        let visibility = self.ctor_visibility(d);
        if visibility == CtorVisibility::Public {
            return format!("create it with `new {name}(...)`");
        }
        if let Some(factories) = self.factories(d) {
            return format!("create it with {factories}");
        }
        match visibility {
            _ if self.scopes[a.module].is_std => {
                format!("`{name}` values come from the functions of the module that declares it")
            }
            CtorVisibility::Protected => {
                format!("add a static factory method to `{name}`, or construct a subclass")
            }
            _ => format!("add a static factory method to `{name}` and call that"),
        }
    }

    pub fn adt_mut(&mut self, d: DefId) -> &mut AdtInfo<'m> {
        match &mut self.info[d.0 as usize] {
            DefInfo::Adt(a) => a,
            _ => panic!("ICE: def {d:?} is not an ADT"),
        }
    }

    pub fn enum_info(&self, d: DefId) -> Option<&EnumInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Enum(e) => Some(e),
            _ => None,
        }
    }

    pub fn iface(&self, d: DefId) -> Option<&IfaceInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Iface(i) => Some(i),
            _ => None,
        }
    }

    pub fn global(&self, d: DefId) -> Option<&GlobalInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Global(g) => Some(g),
            _ => None,
        }
    }

    /// Is `t` a class instance type? Returns (class def, type args).
    pub fn class_of(&self, t: TyId) -> Option<(DefId, Vec<TyId>)> {
        match self.ty.kind(t) {
            TyKind::Adt(d, args) if self.adt(*d).is_some_and(|a| a.kind == AdtKind::Class) => {
                Some((*d, args.clone()))
            }
            _ => None,
        }
    }

    /// Base class type of class instance type `t` (with `t`'s type args substituted).
    pub fn base_of(&mut self, t: TyId) -> Option<TyId> {
        let (d, args) = self.class_of(t)?;
        let base = self.adt(d)?.base?;
        Some(self.ty.subst(base, &args))
    }

    /// Is class `sub` class `sup` or one of its (transitive) subclasses?
    pub fn class_extends(&self, sub: DefId, sup: DefId) -> bool {
        let mut cur = Some(sub);
        for _ in 0..64 {
            match cur {
                Some(d) if d == sup => return true,
                Some(d) => {
                    cur = self
                        .adt(d)
                        .and_then(|a| a.base)
                        .and_then(|b| self.class_of(b))
                        .map(|(d, _)| d)
                }
                None => return false,
            }
        }
        false
    }

    /// Name lookup at module level: own items and imports, then the prelude.
    pub fn lookup_item(&self, module: usize, name: &str) -> Option<Item> {
        self.scopes[module]
            .items
            .get(name)
            .or_else(|| self.prelude.get(name))
            .copied()
    }

    /// Name lookup at a source position of `module`: the innermost nested item whose block
    /// contains `at`, then module level.
    pub fn lookup_item_at(&self, module: usize, name: &str, at: Span) -> Option<Item> {
        let file = self.modules[module].file;
        let nested = self
            .nested
            .iter()
            .filter(|n| n.module == module && n.name == name && at.file == file)
            .filter(|n| n.lo <= at.lo && at.hi <= n.hi)
            .min_by_key(|n| n.hi - n.lo);
        match nested {
            Some(n) => Some(n.item),
            None => self.lookup_item(module, name),
        }
    }

    /// The item a written type path names: `T`, or `ns.T` through namespace import `ns`.
    pub fn lookup_path_at(
        &self,
        module: usize,
        path: &[velt_syntax::ast::Ident],
        at: Span,
    ) -> Option<Item> {
        match path {
            [name] => self.lookup_item_at(module, &name.name, at),
            [ns, name] if self.scopes[module].namespaces.contains_key(&ns.name) => {
                let key = format!("{}.{}", ns.name, name.name);
                self.scopes[module].items.get(&key).copied()
            }
            _ => None,
        }
    }

    /// A string or `string | null`: a value whose copy is a refcount increment at most, so moves
    /// of it are soft (`crate::ownership::strings`).
    pub fn is_string_value(&self, t: TyId) -> bool {
        t == self.ty.str_ || self.ty.opt_payload(t) == Some(self.ty.str_)
    }

    /// Semantics stage 2: a non-Copy value that is *shared* — another reference to the same
    /// value — where its place stays in use or cannot be moved from, instead of being moved
    /// (strings, objects, arrays, maps, closures). Promises have one owner (`await` takes the
    /// result out), so values that hold one are moved as before.
    pub fn is_shared_value(&mut self, t: TyId) -> bool {
        if let Some(&b) = self.shared_memo.get(&t) {
            return b;
        }
        let b = !self.is_copy(t) && !self.holds_promise(t, 0);
        self.shared_memo.insert(t, b);
        b
    }

    fn holds_promise(&mut self, t: TyId, depth: u32) -> bool {
        if depth > 8 {
            return false;
        }
        let parts: Vec<TyId> = match self.ty.kind(t).clone() {
            TyKind::Promise(..) => return true,
            TyKind::Array(e) | TyKind::Option(e) => vec![e],
            TyKind::Tuple(ts) => ts,
            TyKind::Adt(d, args) => {
                let tys: Vec<TyId> = match &self.info[d.0 as usize] {
                    DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
                    DefInfo::Enum(e) => e.variants.iter().flat_map(|v| v.payload.clone()).collect(),
                    _ => vec![],
                };
                tys.into_iter().map(|f| self.ty.subst(f, &args)).collect()
            }
            _ => vec![],
        };
        parts.into_iter().any(|p| self.holds_promise(p, depth + 1))
    }

    /// Ownership: can values of this type be duplicated bitwise?
    pub fn is_copy(&mut self, t: TyId) -> bool {
        self.is_copy_depth(t, 0)
    }

    fn is_copy_depth(&mut self, t: TyId, depth: u32) -> bool {
        if depth > 32 {
            return false;
        }
        match self.ty.kind(t).clone() {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Literal(_)
            | TyKind::Unit
            | TyKind::Never
            | TyKind::Error => true,
            TyKind::Tuple(ts) => ts.iter().all(|t| self.is_copy_depth(*t, depth + 1)),
            TyKind::Option(t) => self.is_copy_depth(t, depth + 1),
            TyKind::Result(a, b) => {
                self.is_copy_depth(a, depth + 1) && self.is_copy_depth(b, depth + 1)
            }
            // Copying a mutex would duplicate its lock word.
            TyKind::Adt(d, _) if Some(d) == self.mutex_ty() => false,
            TyKind::Adt(d, args) => {
                // Objects (structs included, semantics stage 2) are references: never Copy.
                let tys: Vec<TyId> = match &self.info[d.0 as usize] {
                    DefInfo::Enum(e) => e
                        .variants
                        .iter()
                        .flat_map(|v| v.payload.iter().copied())
                        .collect(),
                    _ => return false,
                };
                tys.into_iter().all(|f| {
                    let f = self.ty.subst(f, &args);
                    self.is_copy_depth(f, depth + 1)
                })
            }
            _ => false,
        }
    }

    /// Human-readable type for diagnostics.
    pub fn display(&self, t: TyId) -> String {
        self.display_in(t, &self.display_params)
    }

    /// Display `t` with `Param(i)` named `names[i]`.
    pub(crate) fn display_in(&self, t: TyId, names: &[String]) -> String {
        let list = |s: &Self, ts: &[TyId]| {
            ts.iter()
                .map(|t| s.display_in(*t, names))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let with_args = |s: &Self, name: &str, args: &[TyId]| {
            if args.is_empty() {
                name.to_string()
            } else {
                format!("{name}<{}>", list(s, args))
            }
        };
        if let Some(n) = self.alias_names.get(&t) {
            return n.clone();
        }
        match self.ty.kind(t) {
            TyKind::Int(i) => int_name(*i).to_string(),
            TyKind::Float(crate::hir::FloatTy::F32) => "f32".into(),
            TyKind::Float(crate::hir::FloatTy::F64) => "f64".into(),
            TyKind::Bool => "boolean".into(),
            TyKind::Str => "string".into(),
            TyKind::Unit => "void".into(),
            TyKind::Never => "never".into(),
            TyKind::Error => "_".into(),
            TyKind::Literal(v) => crate::literals::display_lit(v),
            TyKind::Array(e) => match self.ty.kind(*e) {
                TyKind::Option(_) | TyKind::FnPtr { .. } => {
                    format!("({})[]", self.display_in(*e, names))
                }
                _ if self.union_def(*e).is_some() => format!("({})[]", self.display_in(*e, names)),
                _ => format!("{}[]", self.display_in(*e, names)),
            },
            TyKind::Map(k, v) => format!(
                "Map<{}, {}>",
                self.display_in(*k, names),
                self.display_in(*v, names)
            ),
            TyKind::Tuple(ts) => format!("[{}]", list(self, ts)),
            TyKind::Option(t) => format!("{} | null", self.display_in(*t, names)),
            TyKind::Result(a, b) => with_args(self, "Result", &[*a, *b]),
            TyKind::Promise(t, e) if *e == self.ty.never => with_args(self, "Promise", &[*t]),
            TyKind::Promise(t, e) => with_args(self, "Promise", &[*t, *e]),
            TyKind::Shared(t) => with_args(self, "shared", &[*t]),
            TyKind::FnPtr {
                params,
                ret,
                throws,
            } => {
                let ps: Vec<String> = params.iter().map(|p| self.display_in(*p, names)).collect();
                let th = if *throws == self.ty.never {
                    String::new()
                } else {
                    format!(" throws {}", self.display_in(*throws, names))
                };
                format!(
                    "({}) => {}{th}",
                    ps.join(", "),
                    self.display_in(*ret, names)
                )
            }
            TyKind::Adt(d, args) => match &self.info[d.0 as usize] {
                DefInfo::Adt(a) if a.kind == AdtKind::Anon => {
                    // Field types are written over the anon def's own parameters, which
                    // `args` binds to the parameters in scope.
                    let bound: Vec<String> =
                        args.iter().map(|t| self.display_in(*t, names)).collect();
                    let fs: Vec<String> = a
                        .fields
                        .iter()
                        .map(|f| format!("{}: {}", f.name, self.display_in(f.ty, &bound)))
                        .collect();
                    format!("{{ {} }}", fs.join("; "))
                }
                DefInfo::Adt(a) => with_args(self, &a.name, args),
                DefInfo::Enum(e) if e.is_union => self.display_union(*d, args, names),
                DefInfo::Enum(e) => with_args(self, &e.name, args),
                _ => "unknown".into(),
            },
            TyKind::Dyn(d, args) => match self.iface(*d) {
                Some(i) => with_args(self, &i.name, args),
                None => "unknown".into(),
            },
            TyKind::Closure(d) => match self.try_fn(*d) {
                Some(f) => {
                    let ps: Vec<String> = f
                        .params
                        .iter()
                        .map(|p| format!("{}: {}", p.name, self.display_in(p.ty, names)))
                        .collect();
                    format!("({}) => {}", ps.join(", "), self.display_in(f.ret, names))
                }
                None => "unknown".into(),
            },
            TyKind::Param(n) => names
                .get(*n as usize)
                .cloned()
                .unwrap_or_else(|| format!("T{n}")),
        }
    }

    /// Fully qualified name of a module-level item.
    pub fn qualify(&self, module: usize, name: &str) -> String {
        if module == self.root {
            name.to_string()
        } else {
            format!("{}::{}", self.modules[module].path, name)
        }
    }
}
