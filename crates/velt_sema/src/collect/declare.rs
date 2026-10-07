//! Phase 1: allocate a definition per module-level item, build module scopes, resolve imports
//! ([`super::imports`]) and collect the prelude's exports (visible in every module without an
//! import).

use std::collections::HashMap;

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
    for m in 0..cx.modules.len() {
        let module = &cx.modules[m];
        for item in &module.ast.items {
            declare_item(cx, m, item, items);
        }
    }
    super::nested::declare_nested(cx, items);
    for m in 0..cx.modules.len() {
        super::imports::resolve_imports(cx, m);
        super::imports::bind_jsx_runtime(cx, m);
    }
    collect_prelude(cx);
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
        ret_inferred_int: false,
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
        is_generator: false,
        is_async_gen: false,
        soft_moves: vec![],
        is_private: false,
        is_getter: false,
        escaping: false,
        keeps_fn_params: false,
        soft_params: vec![],
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
        ast::ItemKind::Import(_) | ast::ItemKind::Extend(_) | ast::ItemKind::Var(_) => return None,
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
        ast::ItemKind::Import(_) | ast::ItemKind::Extend(_) | ast::ItemKind::Var(_) => return None,
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
            let info = fn_placeholder(
                s.name.name.clone(),
                s.name.span,
                s.span,
                m,
                FnKind::Extern,
                None,
            );
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
    if cx.scopes[m].items.contains_key(&name.name) {
        cx.err(
            format!("the name `{}` is defined multiple times", name.name),
            name.span,
        );
        return;
    }
    cx.scopes[m].items.insert(name.name.clone(), it);
    if item.exported {
        cx.scopes[m].exports.insert(name.name.clone());
    }
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
        base: None,
        methods: HashMap::new(),
        ctor: None,
        own_ctor: None,
        vtable: vec![],
        vslots: HashMap::new(),
        implements: vec![],
        has_dispose: t
            .methods
            .iter()
            .any(|m| !m.is_static && m.decl.sig.name.name == DISPOSE),
        statics: HashMap::new(),
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
        if !cx.scopes[m].is_std || !cx.modules[m].path.starts_with("std/prelude/") {
            continue;
        }
        // Re-exports included: a global module (`std/prelude/global/`) only re-exports.
        for (name, it) in super::exports::all_exports(cx, m) {
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
