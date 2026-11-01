//! Phase 1: allocate a definition per module-level item, build module scopes, resolve imports
//! ([`super::imports`]) and collect the prelude's exports (visible in every module without an
//! import).

use rustc_hash::FxHashMap;

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::ItemDefs;
use crate::ctx::{Ctx, Item};
use crate::defs::{
    AdtInfo, AliasInfo, BodyState, DefInfo, EnumInfo, FnInfo, FnKind, FnSource, Generics, IfaceInfo,
};
use crate::hir::AdtKind;

/// Name of the drop-hook method, `[Symbol.dispose]` (docs/reference/memory.md "Resource cleanup").
pub(crate) const DISPOSE: &str = ast::SYMBOL_DISPOSE;

/// Name of the async cleanup method that `await using` awaits, `[Symbol.asyncDispose]`.
pub(crate) const ASYNC_DISPOSE: &str = ast::SYMBOL_ASYNC_DISPOSE;

pub(super) fn declare_all(cx: &mut Ctx, items: &mut ItemDefs) {
    // Generated modules are collected after the program's bodies are checked
    // (`collect_generated`): their code names the program's types by the instances they were
    // generated for, which only the checked program can tell.
    let ms: Vec<usize> = (0..cx.modules.len())
        .filter(|&m| !cx.is_generated_module(m))
        .collect();
    for &m in &ms {
        let module = &cx.modules[m];
        for item in &module.ast.items {
            declare_item(cx, m, item, items);
        }
    }
    super::nested::declare_nested(cx, items, &ms);
    for m in 0..cx.modules.len() {
        super::imports::resolve_imports(cx, m);
        super::imports::bind_jsx_runtime(cx, m);
    }
    collect_prelude(cx);
}

/// Declare the items of generated modules `ms`: functions, constants and type aliases (what a
/// generator's body and helpers declare); any other item is the generator's error.
pub(super) fn declare_generated(cx: &mut Ctx, ms: &[usize], items: &mut ItemDefs) {
    for &m in ms {
        let module = &cx.modules[m];
        for item in &module.ast.items {
            match &item.kind {
                ast::ItemKind::Function(_) | ast::ItemKind::TypeAlias(_) | ast::ItemKind::Var(_) => {
                    declare_item(cx, m, item, items)
                }
                _ => cx.err(
                    "generated code declares only functions, constants and type aliases (a bug in the generator)",
                    item.span,
                ),
            }
        }
    }
    super::nested::declare_nested(cx, items, ms);
    crate::generated::access::inherit_scopes(cx, ms);
}

pub(super) fn generic_names(gs: &[ast::GenericParam]) -> Generics {
    let mut g = Generics::default();
    for p in gs {
        g.push(&p.name.name);
    }
    g
}

/// A function placeholder; `sigs` fills in params/ret/generic bounds.
pub(crate) fn fn_placeholder<'m>(
    name: String,
    name_span: Span,
    span: Span,
    module: usize,
    kind: FnKind,
    source: Option<FnSource<'m>>,
) -> FnInfo<'m> {
    FnInfo {
        name,
        name_span,
        span,
        module,
        kind,
        generics: Generics::default(),
        this: None,
        params: vec![],
        ret: crate::hir::TyId(0),
        ret_span: None,
        ret_source: crate::defs::RetSource::Known,
        fixed_modes: matches!(
            kind,
            FnKind::Extern | FnKind::Closure | FnKind::IfaceDefault
        ),
        source,
        state: BodyState::Unchecked,
        throw_srcs: vec![],
        declared_throws: None,
        throws: None,
        local_kinds: vec![],
        owner: None,
        is_async: false,
        system: None,
        intrinsic: None,
        is_generator: false,
        is_async_gen: false,
        soft_moves: vec![],
        is_private: false,
        is_getter: false,
        inherits_this: false,
        escaping: false,
        keeps_fn_params: false,
        mutated_captures: vec![],
        soft_params: vec![],
        modified_params: 0,
        modified_captures: 0,
    }
}

fn declare_item<'m>(cx: &mut Ctx<'m>, m: usize, item: &'m ast::Item, items: &mut ItemDefs) {
    if let ast::ItemKind::Var(v) = &item.kind {
        if let Some((name, info)) = super::constants::global_info(cx, m, v) {
            let id = cx.alloc_def(name.span, info);
            items.push(id);
            bind(cx, m, item, name, Item::Def(id));
        }
        return;
    }
    let Some(name) = item_ident(item) else { return };
    let qual = cx.qualify(m, &name.name);
    let Some((name, it)) = new_def(cx, m, item, qual) else {
        return;
    };
    if let Item::Def(id) = it {
        items.push(id);
    }
    bind(cx, m, item, name, it);
}

/// The declared name of a function / type / alias item.
fn item_ident(item: &ast::Item) -> Option<&ast::Ident> {
    Some(match &item.kind {
        ast::ItemKind::Function(f) => &f.sig.name,
        ast::ItemKind::ExternFn(s) => &s.name,
        ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => &t.name,
        ast::ItemKind::Enum(e) => &e.name,
        ast::ItemKind::Interface(d) => &d.name,
        ast::ItemKind::TypeAlias(a) => &a.name,
        ast::ItemKind::Import(_)
        | ast::ItemKind::Extend(_)
        | ast::ItemKind::Var(_)
        | ast::ItemKind::Augment(_)
        | ast::ItemKind::Namespace(_) => return None,
    })
}

/// A definition (or type alias) for a function / type item named `q` (fully qualified), not
/// yet bound to a name.
pub(super) fn new_def<'m>(
    cx: &mut Ctx<'m>,
    m: usize,
    item: &'m ast::Item,
    q: String,
) -> Option<(&'m ast::Ident, Item)> {
    let (name, info): (&ast::Ident, DefInfo<'m>) = match &item.kind {
        ast::ItemKind::Import(_)
        | ast::ItemKind::Extend(_)
        | ast::ItemKind::Var(_)
        | ast::ItemKind::Augment(_)
        | ast::ItemKind::Namespace(_) => return None,
        ast::ItemKind::Function(f) => {
            let src = Some(FnSource::Decl(f));
            let info = fn_placeholder(q, f.sig.name.span, f.sig.span, m, FnKind::Free, src);
            (&f.sig.name, DefInfo::Fn(Box::new(info)))
        }
        ast::ItemKind::ExternFn(s) => {
            // Runtime functions take raw handles: only std may declare them (user code could
            // call them with forged ones).
            if s.name.name.starts_with("velt_rt_") && !cx.scopes[m].is_std {
                cx.err(
                    format!(
                        "`{}` is a runtime function: only the standard library may declare it",
                        s.name.name
                    ),
                    s.name.span,
                );
            }
            let mut info = fn_placeholder(
                s.name.name.clone(),
                s.name.span,
                s.span,
                m,
                FnKind::Extern,
                None,
            );
            if s.is_intrinsic {
                info.intrinsic = Some(crate::intrinsic_decls::declare(cx, m, s));
            }
            (&s.name, DefInfo::Fn(Box::new(info)))
        }
        ast::ItemKind::Struct(t) => (&t.name, adt_info(m, t, AdtKind::Struct, q)),
        ast::ItemKind::Class(t) => (&t.name, adt_info(m, t, AdtKind::Class, q)),
        ast::ItemKind::Enum(e) => (&e.name, enum_info(e, q)),
        ast::ItemKind::Interface(d) => (&d.name, iface_info(m, d, q)),
        ast::ItemKind::TypeAlias(a) => {
            let id = cx.aliases.len() as u32;
            cx.aliases.push(AliasInfo {
                module: m,
                decl: a,
                expanding: false,
                used: false,
                outer: vec![],
            });
            return Some((&a.name, Item::Alias(id)));
        }
    };
    let id = cx.alloc_def(name.span, info);
    if let ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) = &item.kind {
        super::constants::declare_statics(cx, m, id, t);
    }
    Some((name, Item::Def(id)))
}

fn bind(cx: &mut Ctx, m: usize, item: &ast::Item, name: &ast::Ident, it: Item) {
    if split_meanings(cx, m, item, name, it) {
        return;
    }
    if cx.scopes[m].items.contains_key(&name.name) {
        let mut d = Diagnostic::error(
            format!("the name `{}` is defined multiple times", name.name),
            name.span,
        );
        let earlier = match cx.scopes[m].items.get(&name.name) {
            Some(Item::Def(d)) => Some(&cx.info[d.0 as usize]),
            _ => None,
        };
        let earlier_iface = matches!(earlier, Some(DefInfo::Iface(_)));
        let earlier_class = matches!(earlier, Some(DefInfo::Adt(_)));
        let class = matches!(
            item.kind,
            ast::ItemKind::Class(_) | ast::ItemKind::Struct(_)
        );
        let iface = matches!(item.kind, ast::ItemKind::Interface(_));
        if (iface && earlier_class) || (class && earlier_iface) {
            // TypeScript also merges an interface with a class (#384); with a function or a
            // variable it is a separate meaning (`split_meanings`).
            d = d.with_note(format!(
                "Velt merges an interface only with other interface declarations: give `{0}` another name, or name the combination with an intersection (`type More = {0} & {{ … }}`)",
                name.name
            ));
        }
        let earlier_fn = matches!(cx.scopes[m].items.get(&name.name), Some(Item::Def(d)) if matches!(cx.info[d.0 as usize], DefInfo::Fn(_)));
        if matches!(item.kind, ast::ItemKind::Function(_)) && earlier_fn {
            d = d.with_note(
                "overloads with a body each (Velt's form) are not supported yet: write the signatures without bodies, then one implementation that takes every form (TypeScript's form)",
            );
        }
        cx.error(d);
        return;
    }
    cx.scopes[m].items.insert(name.name.clone(), it);
    if item.exported {
        cx.scopes[m].exports.insert(name.name.clone());
    }
}

/// `name` is declared as a type and as a value (`type guid = string` and `const guid = …`, as
/// TypeScript allows): the value keeps the name, the type moves to its type key
/// ([`crate::type_meanings`]), each exported if its own declaration is.
fn split_meanings(cx: &mut Ctx, m: usize, item: &ast::Item, name: &ast::Ident, it: Item) -> bool {
    let key = crate::type_meanings::type_key(&name.name);
    let scope = &cx.scopes[m];
    let Some(&earlier) = scope.items.get(&name.name) else {
        return false;
    };
    if scope.items.contains_key(&key) || !cx.separate_meanings(earlier, it) {
        return false;
    }
    let earlier_exported = scope.exports.contains(&name.name);
    let (ty, value) = match cx.meaning(it) {
        crate::type_meanings::Meaning::Type => ((it, item.exported), (earlier, earlier_exported)),
        _ => ((earlier, earlier_exported), (it, item.exported)),
    };
    let scope = &mut cx.scopes[m];
    scope.items.insert(name.name.clone(), value.0);
    scope.items.insert(key.clone(), ty.0);
    scope.type_keys = true;
    scope.exports.remove(&name.name);
    if value.1 {
        scope.exports.insert(name.name.clone());
    }
    if ty.1 {
        scope.exports.insert(key);
    }
    true
}

fn adt_info(m: usize, t: &ast::TypeDecl, kind: AdtKind, qual: String) -> DefInfo<'_> {
    DefInfo::Adt(Box::new(AdtInfo {
        name: t.name.name.clone(),
        qual_name: qual,
        kind,
        module: m,
        span: t.name.span,
        generics: generic_names(&t.generics),
        fields: vec![],
        own_fields_start: 0,
        redefined: vec![],
        base: None,
        methods: FxHashMap::default(),
        ctor: None,
        own_ctor: None,
        vtable: vec![],
        vslots: FxHashMap::default(),
        implements: vec![],
        has_dispose: t
            .methods
            .iter()
            .any(|m| !m.is_static && m.decl.sig.name.name == DISPOSE),
        statics: FxHashMap::default(),
        decl: Some(t),
    }))
}

fn enum_info(e: &ast::EnumDecl, qual: String) -> DefInfo<'_> {
    DefInfo::Enum(Box::new(EnumInfo {
        name: e.name.name.clone(),
        qual_name: qual,
        span: e.name.span,
        generics: Generics::default(),
        variants: vec![],
        is_union: false,
        decl: Some(e),
    }))
}

fn iface_info(m: usize, d: &ast::InterfaceDecl, qual: String) -> DefInfo<'_> {
    DefInfo::Iface(Box::new(IfaceInfo {
        name: d.name.name.clone(),
        qual_name: qual,
        module: m,
        span: d.name.span,
        generics: generic_names(&d.generics),
        fields: vec![],
        methods: vec![],
        parents: vec![],
        decl: Some(d),
    }))
}

fn collect_prelude(cx: &mut Ctx) {
    for m in 0..cx.modules.len() {
        if !cx.scopes[m].is_std
            || !cx.modules[m]
                .path
                .starts_with(velt_common::std_dir!(prelude))
        {
            continue;
        }
        // Re-exports included: a global module (`std/prelude/global/`) only re-exports.
        for (name, it) in super::exports::all_exports(cx, m).iter() {
            let it = *it;
            cx.prelude_type_keys |= crate::type_meanings::of_type_key(name).is_some();
            if let Some(prev) = cx.prelude.insert(name.clone(), it) {
                if prev != it {
                    let span = match it {
                        Item::Def(d) => cx.def_spans[d.0 as usize],
                        Item::Alias(_) => Span::DUMMY,
                    };
                    cx.diags.push(Diagnostic::error(
                        format!("prelude name `{name}` is exported by two modules"),
                        span,
                    ));
                }
            }
        }
    }
}
