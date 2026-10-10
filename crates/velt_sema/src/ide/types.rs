//! The type query for tools that reason about types ([`Analysis::type_of`]): a checked
//! expression's type as a [`TypeRef`] handle, viewed one level at a time as a [`TypeView`], so
//! lints match on structure instead of parsing display strings. Generic arguments are carried in
//! the handle and substituted lazily, so viewing the fields of `Box<Map<K, V>>` sees the map, and
//! recursive types are no problem.

use std::collections::HashMap;
use std::sync::Arc;

use velt_common::Span;

use super::display::TypeName;
use super::members::{Shape, TypeKind};
use super::{Analysis, DefKind};
use crate::hir::{DefId, FloatTy, IntTy, LitValue, TyId, TyKind};

/// A type recorded by [`Analysis::type_of`] (or reached from one through [`Analysis::view`] /
/// [`Analysis::fields`]). Opaque: inspect it with those queries.
#[derive(Clone, Debug)]
pub struct TypeRef {
    ty: TyId,
    /// Display context (generic parameter names) of `ty`.
    ctx: u32,
    /// The type arguments of the generic definition `ty` is written in (`Param(i)` is `env[i]`).
    env: Option<Arc<[TypeRef]>>,
}

/// One level of a type's structure.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum TypeView {
    /// A fixed-width integer (`i64`, `usize`, …; `IntTy::is_signed`, `IntTy::bits`).
    Int(IntTy),
    /// `f64` (`number`) or `f32`.
    Float(FloatTy),
    /// `boolean`.
    Bool,
    /// `string`.
    Str,
    /// A literal type (`"up"`, `42`, `true`).
    Literal(LiteralKind),
    /// `void`.
    Void,
    /// `never`.
    Never,
    /// `T | null`.
    Nullable(TypeRef),
    /// `T[]`.
    Array(TypeRef),
    /// `[A, B]`.
    Tuple(Vec<TypeRef>),
    /// The prelude's `Map<K, V>`.
    Map(TypeRef, TypeRef),
    /// The standard library's `Set<T>`.
    Set(TypeRef),
    /// `Promise<T>` (what it rejects with is not shown).
    Promise(TypeRef),
    /// `shared<T>`.
    Shared(TypeRef),
    /// A function value (a closure or a named function).
    Fn,
    /// A union of several types (`string | number`); `null` is a [`TypeView::Nullable`] around it.
    Union(Vec<TypeRef>),
    /// A declared class, struct, interface or enum, with its type arguments.
    Named(NamedType),
    /// An object type (`{ x: number }`): its fields are [`Analysis::fields`].
    Record,
    /// A generic parameter of the enclosing definition, by name.
    Param(String),
    /// A type after an error, or one tools have no view of.
    Other,
}

/// What a literal type's value is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiteralKind {
    Str,
    Int,
    Float,
    Bool,
}

/// A declared type, as [`TypeView::Named`] shows it.
#[derive(Clone, Debug)]
pub struct NamedType {
    /// The declared name (`User`, `Map` is [`TypeView::Map`]).
    pub name: String,
    pub kind: NamedKind,
    /// Declared in the standard library (the prelude's `Date`, `Error`, `JsonValue`, …).
    pub is_std: bool,
    pub args: Vec<TypeRef>,
}

/// The kind of a [`NamedType`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NamedKind {
    Class,
    Struct,
    Interface,
    Enum,
}

/// A field of a class, struct, interface or object type ([`Analysis::fields`]).
#[derive(Clone, Debug)]
pub struct FieldView {
    pub name: String,
    pub ty: TypeRef,
    /// Declared optional (`name?: T`) in a class, struct or interface. An object type doesn't
    /// keep it: `{ a?: T }` and `{ a: T | null }` are the same type.
    pub optional: bool,
}

impl Analysis {
    /// The type of the checked expression (or declared local) whose span is exactly `span`;
    /// `None` when nothing with that span was checked.
    pub fn type_of(&self, span: Span) -> Option<TypeRef> {
        let index = self.type_index.get_or_init(|| {
            // The first record: desugarings check synthetic expressions with the span of the
            // expression they stand for after it (`const { a = 1 } = p` reads `p.a` at `p`'s
            // span).
            let mut index = HashMap::with_capacity(self.types.len());
            for (i, (s, _, _)) in self.types.iter().enumerate() {
                index.entry(*s).or_insert(i);
            }
            index
        });
        let &(_, ty, ctx) = self.types.get(*index.get(&span)?)?;
        Some(TypeRef { ty, ctx, env: None })
    }

    /// `t` as source spells it (`Map<string, i64>`, `{ x: i64 }`).
    pub fn show_type(&self, t: &TypeRef) -> String {
        match &t.env {
            None => self.show(t.ty, t.ctx),
            Some(env) => self.names.show(t.ty, &|i| match env.get(i as usize) {
                Some(arg) => self.show_type(arg),
                None => format!("T{i}"),
            }),
        }
    }

    /// One level of `t`'s structure.
    pub fn view(&self, t: &TypeRef) -> TypeView {
        let t = self.resolve(t);
        let here = |ty: TyId| TypeRef {
            ty,
            ctx: t.ctx,
            env: t.env.clone(),
        };
        let list = |ts: &[TyId]| ts.iter().map(|x| here(*x)).collect::<Vec<_>>();
        match self.names.table.kind(t.ty) {
            TyKind::Int(i) => TypeView::Int(*i),
            TyKind::Float(f) => TypeView::Float(*f),
            TyKind::Bool => TypeView::Bool,
            TyKind::Str => TypeView::Str,
            TyKind::Unit => TypeView::Void,
            TyKind::Never => TypeView::Never,
            TyKind::Literal(v) => TypeView::Literal(literal_kind(v)),
            TyKind::Option(x) => TypeView::Nullable(here(*x)),
            TyKind::Array(x) => TypeView::Array(here(*x)),
            TyKind::Tuple(xs) => TypeView::Tuple(list(xs)),
            TyKind::Map(k, v) => TypeView::Map(here(*k), here(*v)),
            TyKind::Promise(x, _) => TypeView::Promise(here(*x)),
            TyKind::Shared(x) => TypeView::Shared(here(*x)),
            TyKind::FnPtr { .. } | TyKind::Closure(_) => TypeView::Fn,
            TyKind::Adt(d, args) | TyKind::Dyn(d, args) => self.view_def(*d, &list(args)),
            TyKind::Param(_) => TypeView::Param(self.show_type(&t)),
            TyKind::Error | TyKind::Result(..) => TypeView::Other,
        }
    }

    fn view_def(&self, d: DefId, args: &[TypeRef]) -> TypeView {
        let env: Arc<[TypeRef]> = args.into();
        let in_def = |ty: TyId| TypeRef {
            ty,
            ctx: 0,
            env: Some(env.clone()),
        };
        let named = |name: &str, kind, is_std: bool| {
            TypeView::Named(NamedType {
                name: name.to_string(),
                kind,
                is_std,
                args: args.to_vec(),
            })
        };
        let Some(tm) = self.members.types.get(&d) else {
            return TypeView::Other;
        };
        let name = match self.names.defs.get(d.0 as usize) {
            Some(TypeName::Named(name)) => name.as_str(),
            _ => "",
        };
        let is_std = tm.is_std;
        match &tm.kind {
            TypeKind::Anon => TypeView::Record,
            TypeKind::Class | TypeKind::Struct if is_std && name == "Set" && args.len() == 1 => {
                TypeView::Set(args[0].clone())
            }
            TypeKind::Class | TypeKind::Struct if is_std && name == "Map" && args.len() == 2 => {
                TypeView::Map(args[0].clone(), args[1].clone())
            }
            TypeKind::Struct => named(name, NamedKind::Struct, is_std),
            TypeKind::Class => named(name, NamedKind::Class, is_std),
            TypeKind::Interface => named(name, NamedKind::Interface, is_std),
            TypeKind::Union(members) => {
                TypeView::Union(members.iter().map(|m| in_def(*m)).collect())
            }
            TypeKind::Enum => named(name, NamedKind::Enum, is_std),
        }
    }

    /// The fields of a class (inherited ones first), struct, interface or object type, with
    /// the type's arguments substituted; empty for other types.
    pub fn fields(&self, t: &TypeRef) -> Vec<FieldView> {
        let t = self.resolve(t);
        let (TyKind::Adt(d, args) | TyKind::Dyn(d, args)) = self.names.table.kind(t.ty) else {
            return vec![];
        };
        let env: Arc<[TypeRef]> = args
            .iter()
            .map(|a| TypeRef {
                ty: *a,
                ctx: t.ctx,
                env: t.env.clone(),
            })
            .collect();
        let Some(tm) = self.members.types.get(d) else {
            return vec![];
        };
        tm.instance
            .iter()
            .filter(|m| m.def.kind == DefKind::Field)
            .filter_map(|m| match m.shape {
                Shape::Value(ty) => Some(FieldView {
                    name: m.name.clone(),
                    ty: TypeRef {
                        ty,
                        ctx: 0,
                        env: Some(env.clone()),
                    },
                    optional: m.optional,
                }),
                Shape::Method(..) => None,
            })
            .collect()
    }

    /// Whether the class or struct `t` declares instance method `name` itself (not inherited),
    /// or the interface `t` has it.
    pub fn declares_method(&self, t: &TypeRef, name: &str) -> bool {
        let t = self.resolve(t);
        let (TyKind::Adt(d, _) | TyKind::Dyn(d, _)) = self.names.table.kind(t.ty) else {
            return false;
        };
        let Some(tm) = self.members.types.get(d) else {
            return false;
        };
        let interface = matches!(tm.kind, TypeKind::Interface);
        tm.instance.iter().any(|m| {
            m.name == name
                && m.def.kind != DefKind::Field
                && (interface || m.declared_in == Some(*d))
        })
    }

    /// Whether `t` is the prelude's `Error` or a class that extends it.
    pub fn is_error(&self, t: &TypeRef) -> bool {
        let t = self.resolve(t);
        match self.names.table.kind(t.ty) {
            TyKind::Adt(d, _) | TyKind::Dyn(d, _) => self.members.errors.contains(d),
            _ => false,
        }
    }

    /// `t` with a generic parameter replaced by its argument, when it has one.
    fn resolve<'t>(&self, t: &'t TypeRef) -> std::borrow::Cow<'t, TypeRef> {
        let mut cur = std::borrow::Cow::Borrowed(t);
        while let TyKind::Param(i) = self.names.table.kind(cur.ty) {
            let Some(arg) = cur.env.as_ref().and_then(|env| env.get(*i as usize)) else {
                break;
            };
            cur = std::borrow::Cow::Owned(arg.clone());
        }
        cur
    }
}

fn literal_kind(v: &LitValue) -> LiteralKind {
    match v {
        LitValue::Str(_) => LiteralKind::Str,
        LitValue::Int(..) => LiteralKind::Int,
        LitValue::Float(..) => LiteralKind::Float,
        LitValue::Bool(_) => LiteralKind::Bool,
    }
}
