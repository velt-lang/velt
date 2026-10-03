//! The type query for tools that reason about types ([`Analysis::type_of`]): a checked
//! expression's type as a [`TypeRef`] handle, viewed one level at a time as a [`TypeView`], so
//! lints match on structure instead of parsing display strings. Generic arguments are carried in
//! the handle and substituted lazily, so viewing the fields of `Box<Map<K, V>>` sees the map, and
//! recursive types are no problem.

use std::sync::Arc;

use velt_common::{FileId, Span};

use super::Analysis;
use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{AdtKind, DefId, FloatTy, IntTy, LitValue, TyId, TyKind};

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

/// What the queries need of each definition, captured when checking ends.
#[derive(Default)]
pub(crate) struct TypeDefs {
    defs: Vec<DefShape>,
}

enum DefShape {
    Adt {
        name: String,
        kind: AdtKind,
        is_std: bool,
        fields: Vec<(String, TyId, bool)>,
        /// Names of the methods the type declares itself (not inherited ones).
        own_methods: Vec<String>,
    },
    Iface {
        name: String,
        is_std: bool,
        fields: Vec<(String, TyId, bool)>,
        methods: Vec<String>,
    },
    Enum {
        name: String,
        is_std: bool,
        /// A union type: its members (each variant's payload).
        union: Option<Vec<TyId>>,
    },
    None,
}

impl TypeDefs {
    pub(crate) fn capture(cx: &Ctx) -> TypeDefs {
        let std_files: Vec<FileId> = cx
            .modules
            .iter()
            .filter(|m| m.is_std)
            .map(|m| m.file)
            .collect();
        let is_std = |span: Span| std_files.contains(&span.file);
        let fields = |fs: &[crate::defs::FieldInfo]| {
            fs.iter()
                .map(|f| (f.name.clone(), f.ty, f.optional))
                .collect()
        };
        let defs = cx
            .info
            .iter()
            .enumerate()
            .map(|(i, info)| match info {
                DefInfo::Adt(a) => {
                    let mut own_methods: Vec<String> = a
                        .methods
                        .iter()
                        .filter(|(_, m)| !m.is_static)
                        .filter(|(_, m)| cx.fn_info(m.def).owner == Some(DefId(i as u32)))
                        .map(|(n, _)| n.clone())
                        .collect();
                    own_methods.sort();
                    DefShape::Adt {
                        name: a.name.clone(),
                        kind: a.kind,
                        is_std: cx.modules.get(a.module).is_some_and(|m| m.is_std),
                        fields: fields(&a.fields),
                        own_methods,
                    }
                }
                DefInfo::Iface(x) => DefShape::Iface {
                    name: x.name.clone(),
                    is_std: cx.modules.get(x.module).is_some_and(|m| m.is_std),
                    fields: fields(&x.fields),
                    methods: x.methods.iter().map(|m| m.name.clone()).collect(),
                },
                DefInfo::Enum(e) => DefShape::Enum {
                    name: e.name.clone(),
                    is_std: is_std(e.span),
                    union: e.is_union.then(|| {
                        e.variants
                            .iter()
                            .filter_map(|v| v.payload.first().copied())
                            .collect()
                    }),
                },
                DefInfo::Fn(_) | DefInfo::Global(_) => DefShape::None,
            })
            .collect();
        TypeDefs { defs }
    }

    fn get(&self, d: DefId) -> &DefShape {
        self.defs.get(d.0 as usize).unwrap_or(&DefShape::None)
    }
}

impl Analysis {
    /// The type of the checked expression (or declared local) whose span is exactly `span`;
    /// `None` when nothing with that span was checked.
    pub fn type_of(&self, span: Span) -> Option<TypeRef> {
        // The first record: desugarings check synthetic expressions with the span of the
        // expression they stand for after it (`const { a = 1 } = p` reads `p.a` at `p`'s span).
        self.types
            .iter()
            .find(|(s, _, _)| *s == span)
            .map(|(_, ty, ctx)| TypeRef {
                ty: *ty,
                ctx: *ctx,
                env: None,
            })
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
        match self.type_defs.get(d) {
            DefShape::Adt {
                kind: AdtKind::Anon,
                ..
            } => TypeView::Record,
            DefShape::Adt {
                name, is_std: true, ..
            } if name == "Set" && args.len() == 1 => TypeView::Set(args[0].clone()),
            DefShape::Adt {
                name, is_std: true, ..
            } if name == "Map" && args.len() == 2 => {
                TypeView::Map(args[0].clone(), args[1].clone())
            }
            DefShape::Adt {
                name, kind, is_std, ..
            } => {
                let kind = match kind {
                    AdtKind::Struct => NamedKind::Struct,
                    _ => NamedKind::Class,
                };
                named(name, kind, *is_std)
            }
            DefShape::Iface { name, is_std, .. } => named(name, NamedKind::Interface, *is_std),
            DefShape::Enum {
                union: Some(members),
                ..
            } => TypeView::Union(members.iter().map(|m| in_def(*m)).collect()),
            DefShape::Enum { name, is_std, .. } => named(name, NamedKind::Enum, *is_std),
            DefShape::None => TypeView::Other,
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
        let fields = match self.type_defs.get(*d) {
            DefShape::Adt { fields, .. } | DefShape::Iface { fields, .. } => fields,
            _ => return vec![],
        };
        fields
            .iter()
            .map(|(name, ty, optional)| FieldView {
                name: name.clone(),
                ty: TypeRef {
                    ty: *ty,
                    ctx: 0,
                    env: Some(env.clone()),
                },
                optional: *optional,
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
        match self.type_defs.get(*d) {
            DefShape::Adt { own_methods, .. } => own_methods.iter().any(|m| m == name),
            DefShape::Iface { methods, .. } => methods.iter().any(|m| m == name),
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
