//! Turning a checked [`Ctx`] and its recorder into an owned [`Analysis`]: declarations are added
//! as references to themselves, every recorded target becomes a [`DefRef`] (built once per
//! definition), and the module / prelude / nested-item scopes and member tables are captured.

use std::collections::HashMap;

use velt_common::{FileId, Span};

use super::defref::{Builder, DefRef};
use super::display::Names;
use super::record::{Recorder, Target};
use super::{members, Analysis};
use crate::ctx::{Ctx, Item};
use crate::defs::{DefInfo, FnKind};
use crate::hir::{DefId, TyKind};

pub(super) fn build(mut cx: Ctx) -> Analysis {
    declarations(&mut cx);
    let raw_members = members::collect(&mut cx);
    let mut rec = *cx.ide.take().unwrap_or_default();
    let no_generics = rec
        .params
        .iter()
        .position(Vec::is_empty)
        .unwrap_or_else(|| {
            rec.params.push(vec![]);
            rec.params.len() - 1
        }) as u32;
    let names = Names::capture(&cx);
    let b = Builder {
        cx: &cx,
        names: &names,
        contexts: &rec.params,
    };
    let mut cache: HashMap<Target, Option<DefRef>> = HashMap::new();
    let mut resolve = |t: &Target| cache.entry(t.clone()).or_insert_with(|| b.build(t)).clone();
    let refs = rec
        .refs
        .iter()
        .filter(|(s, _)| *s != Span::DUMMY)
        .filter_map(|(s, t)| Some((*s, resolve(t)?)))
        .collect();
    let locals = rec
        .scopes
        .iter()
        .filter_map(|e| Some((e.file, e.lo, e.hi, e.name.clone(), resolve(&e.target)?)))
        .collect();
    let file_of = |m: usize| cx.modules[m].file;
    let nested = cx
        .nested
        .iter()
        .filter_map(|n| {
            let d = resolve(&Target::of_item(n.item))?;
            Some((file_of(n.module), n.lo, n.hi, n.name.clone(), d))
        })
        .collect();
    let module_items = (0..cx.modules.len())
        .map(|m| sorted_items(cx.scopes[m].items.iter(), &mut resolve))
        .collect();
    let prelude = sorted_items(cx.prelude.iter(), &mut resolve);
    let files = (0..cx.modules.len()).map(|m| (file_of(m), m)).collect();
    let def_types = def_types(&cx, &rec, no_generics);
    let jsx_tags = jsx_tags(&cx, &names, &mut resolve);
    let members = raw_members.finish(&b);
    let Recorder { types, params, .. } = rec;
    Analysis {
        diagnostics: std::mem::take(&mut cx.diags),
        refs,
        types,
        contexts: params,
        locals,
        nested,
        module_items,
        prelude,
        files,
        def_types,
        jsx_tags,
        names,
        members,
    }
}

/// Per file with a JSX runtime: the fields of its `JSX.IntrinsicElements` (tag, definition,
/// attribute type), sorted by tag.
fn jsx_tags(
    cx: &Ctx,
    names: &Names,
    resolve: &mut impl FnMut(&Target) -> Option<DefRef>,
) -> HashMap<FileId, Vec<(String, DefRef, String)>> {
    let mut out = HashMap::new();
    for (&m, provider) in &cx.jsx_providers {
        let Some(p) = provider else { continue };
        let TyKind::Adt(d, _) = cx.ty.kind(p.intrinsics) else {
            continue;
        };
        let Some(adt) = cx.adt(*d) else { continue };
        let mut tags: Vec<(String, DefRef, String)> = adt
            .fields
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                let def = resolve(&Target::Field(*d, i as u32))?;
                let ty = names.show_in(f.ty, &adt.generics.names);
                Some((f.name.clone(), def, ty))
            })
            .collect();
        tags.sort_by(|a, b| a.0.cmp(&b.0));
        out.insert(cx.modules[m].file, tags);
    }
    out
}

fn sorted_items<'a>(
    items: impl Iterator<Item = (&'a String, &'a Item)>,
    resolve: &mut impl FnMut(&Target) -> Option<DefRef>,
) -> Vec<(String, DefRef)> {
    let mut out: Vec<(String, DefRef)> = items
        .filter_map(|(n, it)| Some((n.clone(), resolve(&Target::of_item(*it))?)))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Types of locals (by declaring identifier), constants, static fields and the fields of
/// non-generic structs, classes, object types and interfaces.
fn def_types(cx: &Ctx, rec: &Recorder, no_generics: u32) -> HashMap<Span, (crate::hir::TyId, u32)> {
    let mut out = HashMap::new();
    // Scope entries cover locals never used by name (such as `this` while typing `this.`).
    let targets = rec.refs.iter().map(|(_, t)| t);
    for t in targets.chain(rec.scopes.iter().map(|e| &e.target)) {
        if let Target::Local(l) = t {
            out.insert(l.decl, (l.ty, l.ctx));
        }
    }
    for d in &cx.info {
        let fields = match d {
            DefInfo::Global(g) => {
                out.insert(g.span, (g.ty, no_generics));
                continue;
            }
            DefInfo::Adt(a) if a.generics.names.is_empty() => &a.fields,
            DefInfo::Iface(x) if x.generics.names.is_empty() => &x.fields,
            _ => continue,
        };
        for f in fields.iter().filter(|f| f.span != Span::DUMMY) {
            out.entry(f.span).or_insert((f.ty, no_generics));
        }
    }
    out
}

/// Every declaration names itself: items, members, variants, aliases.
fn declarations(cx: &mut Ctx) {
    let mut refs: Vec<(Span, Target)> = vec![];
    for (i, info) in cx.info.iter().enumerate() {
        let d = DefId(i as u32);
        let span = cx.def_spans[i];
        match info {
            // Synthesized methods (forwarders, getters, trampolines) and closures have no name
            // of their own in the source.
            DefInfo::Fn(f) if f.source.is_none() && f.kind != FnKind::Extern => {}
            DefInfo::Fn(f) => refs.push((f.name_span, Target::Def(d))),
            DefInfo::Adt(a) if a.decl.is_some() => {
                refs.push((span, Target::Def(d)));
                let own = a.own_fields_start;
                for (k, f) in a.fields.iter().enumerate().skip(own) {
                    refs.push((f.span, Target::Field(d, k as u32)));
                }
            }
            DefInfo::Enum(e) => {
                if let Some(decl) = e.decl {
                    refs.push((span, Target::Def(d)));
                    for (k, v) in decl.variants.iter().enumerate() {
                        refs.push((v.name.span, Target::Variant(d, k as u32)));
                    }
                }
            }
            DefInfo::Iface(x) if x.decl.is_some() => {
                refs.push((span, Target::Def(d)));
                let own_fields = x.decl.map_or(0, |dd| dd.fields.len());
                for (k, f) in x.fields.iter().enumerate().take(own_fields) {
                    refs.push((f.span, Target::Field(d, k as u32)));
                }
                let own_methods = x.decl.map_or(0, |dd| dd.methods.len());
                for k in 0..own_methods.min(x.methods.len()) {
                    refs.push((x.methods[k].span, Target::IfaceMethod(d, k as u32)));
                }
            }
            DefInfo::Global(_) => refs.push((span, Target::Def(d))),
            _ => {}
        }
    }
    for (a, alias) in cx.aliases.iter().enumerate() {
        refs.push((alias.decl.name.span, Target::Alias(a as u32)));
    }
    if let Some(r) = &mut cx.ide {
        r.refs.extend(refs);
    }
}
