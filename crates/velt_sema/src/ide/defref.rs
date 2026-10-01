//! [`DefRef`]: a definition as editors see it (what it is, where it is declared, how to show
//! it), built from the recorder's [`Target`]s once checking is done.

use velt_common::Span;

use super::display::Names;
use super::record::{LocalTarget, Target};
use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnInfo, FnKind};
use crate::hir::{AdtKind, DefId};

/// What kind of definition a [`DefRef`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DefKind {
    Function,
    /// `declare function` (external symbol).
    ExternFunction,
    Method,
    StaticMethod,
    Getter,
    Constructor,
    Field,
    /// `static readonly` field.
    StaticField,
    /// Module-level `const`.
    Constant,
    Struct,
    Class,
    Interface,
    Enum,
    Variant,
    TypeAlias,
    /// `let` / `const` local, pattern binding, loop or catch variable, or `this`.
    Local,
    Parameter,
}

impl DefKind {
    /// Struct, class, interface, enum or type alias.
    pub fn is_type(self) -> bool {
        matches!(
            self,
            DefKind::Struct
                | DefKind::Class
                | DefKind::Interface
                | DefKind::Enum
                | DefKind::TypeAlias
        )
    }
}

/// A definition: its name, kind, declaring identifier and module, and a one-line description
/// (`function f(a: i64): string`, `(field) User.name: string`, `let x: i64`, …).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DefRef {
    pub name: String,
    pub kind: DefKind,
    /// The declaring identifier (`Span::DUMMY` for compiler-provided definitions).
    pub span: Span,
    /// Index of the declaring module in the `modules` given to `check_for_ide`.
    pub module: usize,
    pub detail: String,
}

impl DefRef {
    /// Do both denote the same definition? (Details of inherited members may be spelled with
    /// different generic arguments.)
    pub fn same_def(&self, other: &DefRef) -> bool {
        self.span == other.span && self.kind == other.kind && self.name == other.name
    }
}

/// Builds [`DefRef`]s from targets.
pub(super) struct Builder<'a, 'c, 'm> {
    pub cx: &'c Ctx<'m>,
    pub names: &'a Names,
    pub contexts: &'a [Vec<String>],
}

impl Builder<'_, '_, '_> {
    fn module_of(&self, span: Span) -> usize {
        self.cx
            .modules
            .iter()
            .position(|m| m.file == span.file)
            .unwrap_or(self.cx.root)
    }

    fn mk(&self, name: &str, kind: DefKind, span: Span, detail: String) -> DefRef {
        DefRef {
            name: name.to_string(),
            kind,
            span,
            module: self.module_of(span),
            detail,
        }
    }

    pub fn build(&self, t: &Target) -> Option<DefRef> {
        match t {
            Target::Def(d) => self.def(*d),
            Target::Field(d, i) => self.field(*d, *i as usize),
            Target::Variant(d, i) => self.variant(*d, *i as usize),
            Target::IfaceMethod(d, i) => self.iface_method(*d, *i as usize),
            Target::Alias(a) => {
                let decl = self.cx.aliases.get(*a as usize)?.decl;
                let name = &decl.name.name;
                Some(self.mk(
                    name,
                    DefKind::TypeAlias,
                    decl.name.span,
                    format!("type {name}"),
                ))
            }
            Target::Local(l) => Some(self.local(l)),
        }
    }

    fn local(&self, l: &LocalTarget) -> DefRef {
        let names = self
            .contexts
            .get(l.ctx as usize)
            .map_or(&[][..], |v| &v[..]);
        let ty = self.names.show_in(l.ty, names);
        let (kind, detail) = if l.is_param {
            (DefKind::Parameter, format!("(parameter) {}: {ty}", l.name))
        } else {
            let kw = if l.mutable { "let" } else { "const" };
            (DefKind::Local, format!("{kw} {}: {ty}", l.name))
        };
        self.mk(&l.name, kind, l.decl, detail)
    }

    fn def(&self, d: DefId) -> Option<DefRef> {
        let span = self.cx.def_spans[d.0 as usize];
        match &self.cx.info[d.0 as usize] {
            DefInfo::Fn(f) => Some(self.function(f)),
            DefInfo::Adt(a) => {
                let (kind, kw) = match a.kind {
                    AdtKind::Class => (DefKind::Class, "class"),
                    _ => (DefKind::Struct, "struct"),
                };
                let detail = format!("{kw} {}{}", a.name, generics(&a.generics.names));
                Some(self.mk(&a.name, kind, a.span, detail))
            }
            DefInfo::Enum(e) => {
                let variants: Vec<String> = e
                    .variants
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let explicit = e
                            .decl
                            .and_then(|d| d.variants.get(i))
                            .is_some_and(|x| x.discriminant.is_some());
                        match &v.str_value {
                            Some(s) => format!("{} = {s:?}", v.name),
                            None if explicit => format!("{} = {}", v.name, v.discriminant),
                            None => v.name.clone(),
                        }
                    })
                    .collect();
                let detail = format!(
                    "enum {}{} {{ {} }}",
                    e.name,
                    generics(&e.generics.names),
                    variants.join(", ")
                );
                Some(self.mk(&e.name, DefKind::Enum, e.span, detail))
            }
            DefInfo::Iface(i) => {
                let detail = format!("interface {}{}", i.name, generics(&i.generics.names));
                Some(self.mk(&i.name, DefKind::Interface, i.span, detail))
            }
            DefInfo::Global(g) => {
                let ty = self.names.show_in(g.ty, &[]);
                let (kind, detail) = match g.src.owner {
                    Some(_) => (DefKind::StaticField, format!("(static) {}: {ty}", g.name)),
                    None => (DefKind::Constant, format!("const {}: {ty}", g.name)),
                };
                let short = g.name.rsplit('.').next().unwrap_or(&g.name);
                Some(self.mk(short, kind, span, detail))
            }
        }
    }

    fn owner_name(&self, owner: Option<DefId>) -> String {
        match owner.map(|o| &self.cx.info[o.0 as usize]) {
            Some(DefInfo::Adt(a)) => format!("{}.", a.name),
            Some(DefInfo::Iface(i)) => format!("{}.", i.name),
            _ => String::new(),
        }
    }

    fn function(&self, f: &FnInfo) -> DefRef {
        let short = f
            .name
            .rsplit(['.', ':'])
            .next()
            .unwrap_or(&f.name)
            .to_string();
        let names = &f.generics.names;
        let ps: Vec<String> = f
            .params
            .iter()
            .map(|p| {
                let opt = if p.default.is_some() { "?" } else { "" };
                format!("{}{opt}: {}", p.name, self.names.show_in(p.ty, names))
            })
            .collect();
        // The (inferred or written) error type: hover shows what a call can throw.
        let throws = f
            .throws
            .map(|t| format!(" throws {}", self.names.show_in(t, names)))
            .unwrap_or_default();
        let ret = format!("{}{throws}", self.names.show_in(f.ret, names));
        let own = &names[names.len().min(owner_generics(self.cx, f))..];
        let sig = format!("{}({})", generics(own), ps.join(", "));
        let owner = self.owner_name(f.owner);
        let asyncness = if f.is_async { "async " } else { "" };
        let (kind, detail) = match f.kind {
            FnKind::Free => (
                DefKind::Function,
                format!("{asyncness}function {short}{sig}: {ret}"),
            ),
            FnKind::Extern => (
                DefKind::ExternFunction,
                format!("declare function {short}{sig}: {ret}"),
            ),
            FnKind::Static => (
                DefKind::StaticMethod,
                format!("(static) {owner}{short}{sig}: {ret}"),
            ),
            FnKind::Ctor => (
                DefKind::Constructor,
                format!(
                    "constructor {}({}){throws}",
                    owner.trim_end_matches('.'),
                    ps.join(", ")
                ),
            ),
            _ if f.is_getter => (DefKind::Getter, format!("(getter) {owner}{short}: {ret}")),
            _ => (
                DefKind::Method,
                format!("(method) {asyncness}{owner}{short}{sig}: {ret}"),
            ),
        };
        self.mk(&short, kind, f.name_span, detail)
    }

    fn field(&self, d: DefId, i: usize) -> Option<DefRef> {
        let (owner, names, f) = match &self.cx.info[d.0 as usize] {
            DefInfo::Adt(a) => (&a.name, &a.generics.names, a.fields.get(i)?),
            DefInfo::Iface(x) => (&x.name, &x.generics.names, x.fields.get(i)?),
            _ => return None,
        };
        let ty = self.names.show_in(f.ty, names);
        let detail = format!("(field) {owner}.{}: {ty}", f.name);
        Some(self.mk(&f.name, DefKind::Field, f.span, detail))
    }

    fn variant(&self, d: DefId, i: usize) -> Option<DefRef> {
        let e = self.cx.enum_info(d)?;
        let v = e.variants.get(i)?;
        let span = e
            .decl?
            .variants
            .iter()
            .find(|x| x.name.name == v.name)?
            .name
            .span;
        let detail = match &v.str_value {
            Some(s) => format!("{}.{} = {s:?}", e.name, v.name),
            None => format!("{}.{} = {}", e.name, v.name, v.discriminant),
        };
        Some(self.mk(&v.name, DefKind::Variant, span, detail))
    }

    fn iface_method(&self, d: DefId, i: usize) -> Option<DefRef> {
        let iface = self.cx.iface(d)?;
        let m = iface.methods.get(i)?;
        let mut names = iface.generics.names.clone();
        names.push(iface.name.clone());
        let ps: Vec<String> = m
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, self.names.show_in(p.ty, &names)))
            .collect();
        let ret = self.names.show_in(m.ret, &names);
        let (kind, detail) = if m.is_getter {
            (
                DefKind::Getter,
                format!("(getter) {}.{}: {ret}", iface.name, m.name),
            )
        } else {
            let sig = format!("({}): {ret}", ps.join(", "));
            (
                DefKind::Method,
                format!("(method) {}.{}{sig}", iface.name, m.name),
            )
        };
        Some(self.mk(&m.name, kind, m.span, detail))
    }
}

/// How many of `f`'s generics belong to its owner type (class / interface + `Self`).
fn owner_generics(cx: &Ctx, f: &FnInfo) -> usize {
    match f.owner.map(|o| &cx.info[o.0 as usize]) {
        Some(DefInfo::Adt(a)) => a.generics.len(),
        Some(DefInfo::Iface(i)) => i.generics.len() + 1,
        _ => 0,
    }
}

fn generics(names: &[String]) -> String {
    if names.is_empty() {
        String::new()
    } else {
        format!("<{}>", names.join(", "))
    }
}
