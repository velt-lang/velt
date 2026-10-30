//! The type model a generator reads (docs/internals/design/source-generators.md §4): a closed,
//! read-only description of the types a request reaches, with types referring to each other by
//! index so that recursive types are a graph. `velt/generate` (std/generate) declares the same
//! model as Velt classes; the driver sends it to the generator host.

use rustc_hash::FxHashMap;
use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{AdtKind, DefId, LitValue, TyId, TyKind};

#[derive(Clone, Debug, Default)]
pub struct Model {
    pub types: Vec<ModelType>,
    /// Where things are declared, by index (`ModelType::at`, `ModelField::at`): a generator
    /// reports a diagnostic at one of them.
    pub spans: Vec<Span>,
}

#[derive(Clone, Debug, Default)]
pub struct ModelType {
    /// `number`, `boolean`, `string`, `void`, `never`, `symbol`, `literal`, `enum`, `array`,
    /// `tuple`, `option`, `union`, `object`, `class`, `struct`, `interface`, `map`, `date`,
    /// `function`, `any`, `unknown`, `promise` or `opaque`.
    pub kind: &'static str,
    /// As messages show it (`User`, `Map<string, User[]>`).
    pub display: String,
    /// `<module path>::<name>` of a declared type, else empty.
    pub qualified_name: String,
    /// How generated code writes the type (empty when it cannot: a local class).
    pub spelling: String,
    /// `f64`, `i32`, … for numbers.
    pub repr: String,
    /// Element (`array`), payload (`option`, `promise`), key and value (`map`), elements
    /// (`tuple`), type arguments (`class`, `struct`, `interface`, `opaque`).
    pub args: Vec<u32>,
    /// The members of a `union`, in order.
    pub arms: Vec<u32>,
    /// Fields of `object`, `class` and `struct` types, inherited ones first.
    pub fields: Vec<ModelField>,
    /// A class's base class and the classes of the program that extend it (directly or not;
    /// generic subclasses are not listed yet).
    pub base: Option<u32>,
    pub subclasses: Vec<u32>,
    /// Members of an `enum`.
    pub members: Vec<ModelMember>,
    /// The value of a `literal` type, as JavaScript source (`"a"`, `1`, `true`).
    pub literal: String,
    pub at: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct ModelField {
    pub name: String,
    pub ty: u32,
    pub optional: bool,
    pub readonly: bool,
    /// `private` (declared so), or an ES private field (`#x`, whose name keeps the `#`).
    pub private: bool,
    pub es_private: bool,
    pub has_default: bool,
    pub at: u32,
}

#[derive(Clone, Debug, Default)]
pub struct ModelMember {
    pub name: String,
    /// The value as JavaScript source: a number, or a string literal for a string enum.
    pub value: String,
}

/// Builds one model for every type the requests reach.
pub(crate) struct Builder {
    pub model: Model,
    ids: FxHashMap<TyId, u32>,
    queue: Vec<TyId>,
}

impl Builder {
    pub fn new() -> Self {
        Builder {
            model: Model::default(),
            ids: FxHashMap::default(),
            queue: vec![],
        }
    }

    /// The id of `t`, describing it (and what it reaches) when it is new.
    pub fn add(&mut self, cx: &mut Ctx, t: TyId) -> u32 {
        let id = self.id(t);
        while let Some(t) = self.queue.pop() {
            let i = self.ids[&t] as usize;
            self.model.types[i] = self.describe(cx, t);
        }
        id
    }

    fn id(&mut self, t: TyId) -> u32 {
        if let Some(&id) = self.ids.get(&t) {
            return id;
        }
        let id = self.model.types.len() as u32;
        self.model.types.push(ModelType::default());
        self.ids.insert(t, id);
        self.queue.push(t);
        id
    }

    fn span(&mut self, s: Span) -> u32 {
        self.model.spans.push(s);
        self.model.spans.len() as u32 - 1
    }

    fn describe(&mut self, cx: &mut Ctx, t: TyId) -> ModelType {
        let mut m = ModelType {
            display: cx.display(t),
            spelling: super::spell::spell(cx, t).unwrap_or_default(),
            ..ModelType::default()
        };
        match cx.ty.kind(t).clone() {
            TyKind::Int(it) => {
                m.kind = "number";
                m.repr = super::spell::int_name(it).into();
            }
            TyKind::Float(ft) => {
                m.kind = "number";
                m.repr = super::spell::float_name(ft).into();
            }
            TyKind::Bool => m.kind = "boolean",
            TyKind::Str => m.kind = "string",
            TyKind::Unit => m.kind = "void",
            TyKind::Never => m.kind = "never",
            TyKind::Symbol => m.kind = "symbol",
            TyKind::Literal(l) => {
                m.kind = "literal";
                m.literal = match l {
                    LitValue::Str(s) => format!("{s:?}"),
                    LitValue::Int(_, v) => v.to_string(),
                    LitValue::Float(_, b) => format!("{}", f64::from_bits(b)),
                    LitValue::Bool(b) => b.to_string(),
                };
            }
            TyKind::Array(e) => {
                m.kind = "array";
                m.args = vec![self.id(e)];
            }
            TyKind::Tuple(ts) => {
                m.kind = "tuple";
                m.args = ts.iter().map(|t| self.id(*t)).collect();
            }
            TyKind::Option(x, _) => {
                m.kind = "option";
                m.args = vec![self.id(x)];
            }
            TyKind::Map(k, v) => {
                m.kind = "map";
                m.args = vec![self.id(k), self.id(v)];
            }
            TyKind::Promise(x, _) => {
                m.kind = "promise";
                m.args = vec![self.id(x)];
            }
            TyKind::FnPtr { .. } | TyKind::Closure(_) => m.kind = "function",
            TyKind::Dyn(d, args) => {
                let any = cx.any.as_ref().map(|a| (a.any, a.unknown));
                m.kind = match any {
                    Some((a, _)) if a == d => "any",
                    Some((_, u)) if u == d => "unknown",
                    _ => "interface",
                };
                if let Some(i) = cx.iface(d) {
                    m.qualified_name = i.qual_name.clone();
                    let s = i.span;
                    m.at = Some(self.span(s));
                }
                m.args = args.iter().map(|t| self.id(*t)).collect();
            }
            TyKind::Adt(d, args) => self.adt(cx, d, &args, &mut m),
            TyKind::Shared(_)
            | TyKind::Simd(_)
            | TyKind::Error
            | TyKind::Result(..)
            | TyKind::Param(_)
            | TyKind::Op(..) => m.kind = "opaque",
        }
        m
    }

    fn adt(&mut self, cx: &mut Ctx, d: DefId, args: &[TyId], m: &mut ModelType) {
        match &cx.info[d.0 as usize] {
            DefInfo::Adt(a) => {
                let std = cx.scopes[a.module].is_std;
                m.qualified_name = if a.kind == AdtKind::Anon {
                    String::new()
                } else {
                    a.qual_name.clone()
                };
                let fields: Vec<_> = (a.fields.iter())
                    .map(|f| {
                        let private = f.private_to.is_some() || f.name.starts_with('#');
                        (
                            f.name.clone(),
                            f.declared,
                            f.optional,
                            f.readonly,
                            private,
                            f.has_default,
                            f.span,
                        )
                    })
                    .collect();
                let (kind, base, span) = (a.kind, a.base, a.span);
                m.kind = match kind {
                    AdtKind::Anon => "object",
                    AdtKind::Struct => "struct",
                    AdtKind::Class => "class",
                };
                // Std types keep runtime state in private fields, which no generator may see.
                if std && fields.iter().any(|f| f.4) {
                    m.kind = if m.qualified_name.ends_with("::Date") {
                        "date"
                    } else {
                        "opaque"
                    };
                } else {
                    for (name, declared, optional, readonly, private, has_default, at) in fields {
                        let ty = cx.subst(declared, args);
                        let es_private = name.starts_with('#');
                        let field = ModelField {
                            ty: self.id(ty),
                            optional,
                            readonly,
                            private,
                            es_private,
                            has_default,
                            at: self.span(at),
                            name,
                        };
                        m.fields.push(field);
                    }
                }
                m.at = Some(self.span(span));
                m.args = args.iter().map(|t| self.id(*t)).collect();
                if let Some(b) = base {
                    let b = cx.subst(b, args);
                    m.base = Some(self.id(b));
                }
                if kind == AdtKind::Class && args.is_empty() {
                    for c in subclasses(cx, d) {
                        let t = cx.ty.intern(TyKind::Adt(c, vec![]));
                        let id = self.id(t);
                        m.subclasses.push(id);
                    }
                }
            }
            DefInfo::Enum(e) if e.is_union => {
                m.kind = "union";
                let payloads: Vec<TyId> = (e.variants.iter())
                    .filter_map(|v| v.payload.first().copied())
                    .collect();
                for p in payloads {
                    let p = cx.subst(p, args);
                    let id = self.id(p);
                    m.arms.push(id);
                }
            }
            DefInfo::Enum(e) => {
                m.kind = "enum";
                m.qualified_name = e.qual_name.clone();
                m.members = (e.variants.iter())
                    .map(|v| ModelMember {
                        name: v.name.clone(),
                        value: match &v.str_value {
                            Some(s) => format!("{s:?}"),
                            None => v.discriminant.to_string(),
                        },
                    })
                    .collect();
                let s = e.span;
                m.at = Some(self.span(s));
            }
            _ => m.kind = "opaque",
        }
    }
}

/// The non-generic classes of the program that extend class `d`, directly or not, in
/// definition order.
fn subclasses(cx: &Ctx, d: DefId) -> Vec<DefId> {
    let mut out = vec![];
    for (i, info) in cx.info.iter().enumerate() {
        let DefInfo::Adt(a) = info else { continue };
        if a.kind != AdtKind::Class || !a.generics.names.is_empty() || i == d.0 as usize {
            continue;
        }
        let mut base = a.base;
        let mut depth = 0;
        while let (Some(b), true) = (base, depth < 64) {
            depth += 1;
            let TyKind::Adt(bd, _) = *cx.ty.kind(b) else {
                break;
            };
            if bd == d {
                out.push(DefId(i as u32));
                break;
            }
            base = cx.adt(bd).and_then(|x| x.base);
        }
    }
    out
}
