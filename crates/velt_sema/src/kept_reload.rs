//! What a kept check's reload (crate `kept`, dev-warm-front-end.md step 3 "C3") checks before
//! it trusts the kept context with an edit: the functions edited, found by an unambiguous name;
//! that the edit moves no other definition; what the source rewrites and scans find of the
//! whole program; and what the other bodies' checks read of an edited function.

use std::fmt::{Debug, Write as _};
use std::hash::Hasher;

use velt_syntax::ast::{self, ItemKind};

use crate::ctx::Ctx;
use crate::hir::DefId;
use crate::SourceModule;

/// What the source rewrites and scans found of a program (`check_timed`), as hashes: of each
/// rewritten module's syntax tree, and of everything else they hand the check (the scans'
/// facts, the rewrites' side outputs and diagnostics). A reload whose edit changes any of them,
/// but for the edited modules' own trees, needs the cold check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Facts {
    pub modules: Vec<u64>,
    pub rest: u64,
}

/// Hashes what it is written (`Debug` output) without keeping it.
struct HashWrite(std::collections::hash_map::DefaultHasher);

impl std::fmt::Write for HashWrite {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.write(s.as_bytes());
        Ok(())
    }
}

/// The hash of `x`'s `Debug` output.
pub(crate) fn hash_of(x: &dyn Debug) -> u64 {
    let mut w = HashWrite(Default::default());
    let _ = write!(w, "{x:?}");
    w.0.finish()
}

impl Facts {
    pub(crate) fn of(modules: &[SourceModule], rest: &[&dyn Debug]) -> Facts {
        Facts {
            modules: modules.iter().map(|m| hash_of(&m.ast)).collect(),
            rest: hash_of(&rest),
        }
    }
}

/// The name the dev loop's change detection gives a method (`veltc` `dev::edits`): `T.m`, with
/// `get `, `set ` or `static ` before `m` for a getter, a setter or a static method, so that no
/// two members of a type share one unless they are overloads (which a reload refuses).
pub fn member_name(ty: &str, m: &ast::Method) -> String {
    let stat = if m.is_static { "static " } else { "" };
    let kind = match (m.is_getter, m.is_setter) {
        (true, _) => "get ",
        (_, true) => "set ",
        _ => "",
    };
    format!("{ty}.{stat}{kind}{}", m.decl.sig.name.name)
}

/// The function declared as `name` (`member_name`, `T.constructor` or a top-level function's
/// name) in the module at `module` (its canonical path); an error when no declaration or more
/// than one has that name.
pub(crate) fn find_decl(cx: &Ctx, module: &str, name: &str) -> Result<DefId, String> {
    let m = (0..cx.modules.len())
        .find(|&m| cx.modules[m].path == module)
        .ok_or_else(|| format!("no module `{module}`"))?;
    let (owner, member) = match name.split_once('.') {
        Some((t, mm)) => (t, Some(mm)),
        None => (name, None),
    };
    let mut found: Vec<&ast::FnDecl> = vec![];
    for item in &cx.modules[m].ast.items {
        match (&item.kind, member) {
            (ItemKind::Function(f), None) if f.sig.name.name == owner => found.push(f),
            (ItemKind::Class(t) | ItemKind::Struct(t), Some(mm)) if t.name.name == owner => {
                if mm == "constructor" {
                    found.extend(t.constructor.as_ref());
                }
                (t.methods.iter())
                    .filter(|x| member_name(owner, x) == name)
                    .for_each(|x| found.push(&x.decl));
            }
            _ => {}
        }
    }
    let decl = match found[..] {
        [d] => d,
        [] => return Err(format!("no `{name}` in `{module}`")),
        _ => return Err(format!("more than one `{name}` in `{module}`")),
    };
    (0..cx.defs.len())
        .map(|i| DefId(i as u32))
        .find(|&d| {
            matches!(cx.info[d.0 as usize], crate::defs::DefInfo::Fn(_))
                && matches!(cx.fn_info(d).source,
                    Some(crate::defs::FnSource::Decl(x)) if std::ptr::eq(x, decl))
        })
        .ok_or_else(|| format!("`{name}` has no definition"))
}

/// Where a function declaration is in its module: the item, and for a member of a class, struct
/// or extension, which one (`Ctor` or method `i`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Member {
    Item,
    Ctor,
    Method(usize),
}

/// The path of `decl` (by address) among `m`'s items.
pub(crate) fn decl_path(m: &ast::Module, decl: &ast::FnDecl) -> Option<(usize, Member)> {
    let same = |d: &ast::FnDecl| std::ptr::eq(d, decl);
    for (i, item) in m.items.iter().enumerate() {
        match &item.kind {
            ItemKind::Function(f) if same(f) => return Some((i, Member::Item)),
            ItemKind::Class(t) | ItemKind::Struct(t) => {
                if t.constructor.as_ref().is_some_and(same) {
                    return Some((i, Member::Ctor));
                }
                if let Some(k) = t.methods.iter().position(|x| same(&x.decl)) {
                    return Some((i, Member::Method(k)));
                }
            }
            ItemKind::Extend(e) => {
                if let Some(k) = e.methods.iter().position(|x| same(&x.decl)) {
                    return Some((i, Member::Method(k)));
                }
            }
            _ => {}
        }
    }
    None
}

/// The function declaration at `path` among `m`'s items (`decl_path`).
pub(crate) fn decl_at(m: &ast::Module, (i, member): (usize, Member)) -> Option<&ast::FnDecl> {
    decl_at_item(&m.items.get(i)?.kind, member)
}

fn decl_at_item(kind: &ItemKind, member: Member) -> Option<&ast::FnDecl> {
    match (kind, member) {
        (ItemKind::Function(f), Member::Item) => Some(f),
        (ItemKind::Class(t) | ItemKind::Struct(t), Member::Ctor) => t.constructor.as_ref(),
        (ItemKind::Class(t) | ItemKind::Struct(t), Member::Method(k)) => {
            Some(&t.methods.get(k)?.decl)
        }
        (ItemKind::Extend(e), Member::Method(k)) => Some(&e.methods.get(k)?.decl),
        _ => None,
    }
}

fn decl_at_item_mut(kind: &mut ItemKind, member: Member) -> Option<&mut ast::FnDecl> {
    match (kind, member) {
        (ItemKind::Function(f), Member::Item) => Some(f),
        (ItemKind::Class(t) | ItemKind::Struct(t), Member::Ctor) => t.constructor.as_mut(),
        (ItemKind::Class(t) | ItemKind::Struct(t), Member::Method(k)) => {
            Some(&mut t.methods.get_mut(k)?.decl)
        }
        (ItemKind::Extend(e), Member::Method(k)) => Some(&mut e.methods.get_mut(k)?.decl),
        _ => None,
    }
}

/// Does the edit from `old` to `new` (the same module before and after, the declarations at
/// `edited` changed) leave every other definition where it was? The kept context holds the
/// other definitions' checked code with their old positions, which lowering resolves against
/// the new text (`file:line:col` strings, closure names): an edit that changes a body's length
/// moves everything after it in its file, and needs the cold check.
pub(crate) fn others_stay(
    old: &ast::Module,
    new: &ast::Module,
    edited: &[(usize, Member)],
) -> bool {
    if old.items.len() != new.items.len() {
        return false;
    }
    old.items
        .iter()
        .zip(&new.items)
        .enumerate()
        .all(|(i, (a, b))| {
            let here: Vec<Member> = (edited.iter())
                .filter(|(at, _)| *at == i)
                .map(|(_, m)| *m)
                .collect();
            if here.is_empty() {
                return hash_of(a) == hash_of(b);
            }
            // The edited declarations as they were, in the new item: what is left to differ is
            // where the rest of the item is.
            let mut b = b.clone();
            for m in here {
                match (decl_at_item(&a.kind, m), decl_at_item_mut(&mut b.kind, m)) {
                    (Some(x), Some(y)) => *y = x.clone(),
                    _ => return false,
                }
            }
            b.span = a.span;
            hash_of(a) == hash_of(&b)
        })
}

/// What other bodies' checks read of a function that its own body can change: its result type
/// when inferred, and what it throws (its throw sources resolved, `throws::srcs_now`), which
/// decides whether a caller's `catch` can be reached. (Its parameters' modes and the rest come
/// from the passes, which a reload runs again over the whole program.)
pub(crate) fn signature(
    cx: &mut Ctx,
    def: DefId,
) -> (crate::hir::TyId, Option<crate::hir::TyId>, bool) {
    let srcs = cx.fn_info(def).throw_srcs.clone();
    let thrown = crate::throws::srcs_now(cx, &srcs);
    let f = cx.fn_info(def);
    (f.ret, thrown, f.inherits_this)
}

/// The rest of `crate::analyze` for a clone of a kept check whose edited bodies were checked
/// again (`kept`), and what the round loop of `check_timed` would do with it: the program, or why it
/// needs the cold check (another round, a generator, a diagnostic past the first `before`, which
/// the cold check reports).
pub(crate) fn reanalyze(
    mut cx: Ctx,
    opts: crate::CheckOptions,
    before: usize,
) -> Result<crate::hir::Program, String> {
    let root = cx.root;
    crate::analyze_after_bodies(&mut cx);
    let root_mod = cx.modules[root];
    let entry = crate::main_fn::check_main(&mut cx, root, root_mod, opts.require_main);
    crate::alias_expand::check_unused_aliases(&mut cx);
    if cx.diags.len() > before || crate::error_count(&cx) > 0 {
        let d = (cx.diags.get(before)).or_else(|| cx.diags.iter().find(|d| d.is_error()));
        return Err(format!(
            "it reports something: {}",
            d.map_or("", |d| &d.message)
        ));
    }
    if cx.union_views_grown().is_some() || cx.fn_sites.grown().is_some() {
        return Err("another round".into());
    }
    if cx.held_borrows && crate::retry_sharing(&cx) {
        return Err("the sharing retry".into());
    }
    let mut assumed = crate::body::lazy_std::Assumed::default();
    if crate::body::lazy_std::too_late(&mut cx, &mut assumed) {
        return Err("std it now needs".into());
    }
    crate::union_views::finish(&mut cx);
    cx.upcast_writes();
    if cx.checked_again(false).is_some() {
        return Err("another round".into());
    }
    let Ok(generated) = crate::generated::instances(&mut cx) else {
        return Err("generated code".into());
    };
    let (program, diags) = crate::program::build_checked(cx, entry, generated);
    match diags.len() > before {
        true => Err("it reports something".into()),
        false => Ok(program),
    }
}
