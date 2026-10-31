//! The type model a generator reads (docs/internals/design/source-generators.md §4): a closed,
//! read-only description of the types a request reaches, with types referring to each other by
//! index so that recursive types are a graph. `velt/generate` (std/generate) declares the same
//! model as Velt classes; the driver sends it to the generator host.

use rustc_hash::FxHashMap;
use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{AdtKind, DefId, LitValue, Nothing, TyId, TyKind};
use crate::hooks;

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
    /// `tuple`, `option`, `union`, `object`, `class`, `struct`, `interface`, `map`, `record`,
    /// `set`, `shared`, `date`, `function`, `generator`, `any`, `unknown`, `promise` or
    /// `opaque`.
    pub kind: &'static str,
    /// As messages show it (`User`, `Map<string, User[]>`).
    pub display: String,
    /// `<module path>::<name>` of a declared type, else empty.
    pub qualified_name: String,
    /// How generated code writes the type: a handle (`__type<"<instance>", <index>>`,
    /// [`super::handles`]) that names it in any check of the same program.
    pub spelling: String,
    /// `f64`, `i32`, … for numbers.
    pub repr: String,
    /// Element (`array`, `set`), payload (`option`, `promise`, `shared`), key and value (`map`,
    /// `record`), elements (`tuple`), type arguments (`class`, `struct`, `interface`, `opaque`).
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
    /// The value of a `literal` type, as JavaScript source (`"a"`, `1`, `true`); a string is
    /// also its JSON text. For an `option`: what it holds when empty (`null`, `undefined` or
    /// `null | undefined`).
    pub literal: String,
    /// The methods of a class that std's operations call by name (`toJSON`, `toString`,
    /// `[INSPECT]`), with their parameter count and result type, when callable as such.
    pub methods: Vec<ModelMethod>,
    /// Whether a value of the type may be a typed `Proxy` standing for it: a class or object
    /// type in a program that uses `Proxy` (velt_vir's `proxiable` decides which exactly).
    pub may_be_proxy: bool,
    /// A `function` with properties (`Object.assign(f, { … })`).
    pub has_properties: bool,
    /// A class used as a value (`typeof C`: its constructor and statics).
    pub class_value: bool,
    /// A dynamic type: `any`, `unknown`, and also `object` and `Function` (kind `interface`),
    /// whose values say at run time what they hold.
    pub dynamic: bool,
    /// For a std type whose structure the model does not show (`opaque`, `set`): the private
    /// field that keeps its state (a runtime handle), else empty.
    pub private_field: String,
    /// A struct's or class's constructor when it is not public (`private`, `protected`), else
    /// empty: decoding builds a value without running it, which such a type does not allow.
    pub ctor_access: &'static str,
    pub at: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct ModelMethod {
    pub name: String,
    /// Its parameters (`toJSON(key)` takes one).
    pub params: u32,
    /// Its result type.
    pub ret: u32,
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
    /// An object literal's getter (`get k() { … }`): `ty` is what it returns.
    pub getter: bool,
    /// A private field a std type declares (a runtime handle, also in a class that extends it).
    pub std_private: bool,
    /// A field the layout holds for an extension of the type (an `interface E extends T` with
    /// optional fields that shares `T`'s objects): read through the value seen as this type.
    pub via: Option<u32>,
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
    /// The sema type of each model type.
    tys: Vec<TyId>,
    queue: Vec<TyId>,
}

impl Builder {
    pub fn new() -> Self {
        Builder {
            model: Model::default(),
            ids: FxHashMap::default(),
            tys: vec![],
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

    /// The sema type of model type `i`.
    pub fn ty_of(&self, i: u32) -> TyId {
        self.tys[i as usize]
    }

    fn id(&mut self, t: TyId) -> u32 {
        if let Some(&id) = self.ids.get(&t) {
            return id;
        }
        let id = self.model.types.len() as u32;
        self.model.types.push(ModelType::default());
        self.ids.insert(t, id);
        self.tys.push(t);
        self.queue.push(t);
        id
    }

    fn span(&mut self, s: Span) -> u32 {
        self.model.spans.push(s);
        self.model.spans.len() as u32 - 1
    }

    fn describe(&mut self, cx: &mut Ctx, t: TyId) -> ModelType {
        // A class instance brand (`crate::brands`) is its class, a primitive's brand that
        // primitive: described as what it is, named as written.
        if let Some(b) = cx.brand_base(t).filter(|b| *b != t) {
            let mut m = self.describe(cx, b);
            m.display = cx.display(t);
            return m;
        }
        let mut m = ModelType {
            display: cx.display(t),
            class_value: cx.ctor_sig_own(t).is_some(),
            dynamic: cx.dynamic(t).is_some(),
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
                    LitValue::Str(s) => json_quote(&s),
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
            TyKind::Option(x, nothing) => {
                m.kind = "option";
                m.args = vec![self.id(x)];
                m.literal = match nothing {
                    Nothing::Null => "null",
                    Nothing::Undefined => "undefined",
                    Nothing::Both => "null | undefined",
                }
                .into();
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
            TyKind::Shared(x) => {
                m.kind = "shared";
                m.args = vec![self.id(x)];
            }
            TyKind::Simd(_)
            | TyKind::Error
            | TyKind::Result(..)
            | TyKind::Param(_)
            | TyKind::Op(..) => m.kind = "opaque",
        }
        m
    }

    fn adt(&mut self, cx: &mut Ctx, d: DefId, args: &[TyId], m: &mut ModelType) {
        // The prelude's collections are kinds of their own: their structure is their entries,
        // not their (private) fields.
        let collection = ["Map", "Record", "Set"]
            .into_iter()
            .find(|n| cx.prelude_adt(n) == Some(d) || (*n == "Set" && is_std_set(cx, d)));
        if let Some(name) = collection {
            m.kind = match name {
                "Map" => "map",
                "Record" => "record",
                _ => "set",
            };
            m.qualified_name = cx.adt(d).map(|a| a.qual_name.clone()).unwrap_or_default();
            m.args = args.iter().map(|t| self.id(*t)).collect();
            if name == "Set" {
                m.private_field = std_private_field(cx, d).unwrap_or_default();
            }
            return;
        }
        if cx.is_generator_class(d) {
            m.kind = "generator";
            m.qualified_name = cx.adt(d).map(|a| a.qual_name.clone()).unwrap_or_default();
            m.args = args.iter().map(|t| self.id(*t)).collect();
            return;
        }
        // A function with properties is a function (its properties are not data).
        let t = cx.ty.intern(TyKind::Adt(d, args.to_vec()));
        if cx.is_callable_object(t) {
            m.kind = "function";
            m.has_properties = true;
            return;
        }
        let getters = literal_getters(cx, d);
        match &cx.info[d.0 as usize] {
            DefInfo::Adt(a) => {
                let std = cx.keeps_handles(d);
                m.qualified_name = if a.kind == AdtKind::Anon {
                    String::new()
                } else {
                    a.qual_name.clone()
                };
                // An object type whose layout its extensions widen holds their fields too.
                let added = cx.layout_added_fields(d);
                let declared = a.fields.iter().map(|f| (f, None));
                let fields: Vec<_> = (declared.chain(added.iter().map(|(f, e)| (f, Some(*e)))))
                    .map(|(f, via)| {
                        let private = f.private_to.is_some() || f.name.starts_with('#');
                        // The prelude `Error`'s `#name` is no runtime handle.
                        let plain = cx.plain_private(f.private_to, &f.name);
                        let std_private =
                            !plain && f.private_to.is_some_and(|o| cx.keeps_handles(o));
                        (
                            f.name.clone(),
                            f.declared,
                            f.optional,
                            f.readonly,
                            private && !plain,
                            f.has_default,
                            f.span,
                            (via, std_private),
                        )
                    })
                    .collect();
                let (kind, base, span) = (a.kind, a.base, a.span);
                m.ctor_access = match cx.ctor_visibility(d) {
                    velt_syntax::ast::CtorVisibility::Public => "",
                    velt_syntax::ast::CtorVisibility::Private => "private",
                    velt_syntax::ast::CtorVisibility::Protected => "protected",
                };
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
                    let handle = fields.iter().find(|f| f.7 .1).map(|f| f.0.clone());
                    m.private_field = handle.unwrap_or_default();
                } else {
                    for (name, declared, optional, readonly, private, has_default, at, extra) in
                        fields
                    {
                        let (via, std_private) = extra;
                        let getter = getters.iter().find(|(n, _)| *n == name).map(|(_, r)| *r);
                        let ty = cx.subst(getter.unwrap_or(declared), args);
                        let es_private = name.starts_with('#');
                        let field = ModelField {
                            ty: self.id(ty),
                            optional,
                            readonly,
                            private,
                            es_private,
                            has_default,
                            getter: getter.is_some(),
                            std_private,
                            via: via.map(|e| {
                                let t = cx.ty.intern(TyKind::Adt(e, vec![]));
                                self.id(t)
                            }),
                            at: self.span(at),
                            name,
                        };
                        m.fields.push(field);
                    }
                }
                m.at = Some(self.span(span));
                m.may_be_proxy = kind != AdtKind::Struct && cx.prelude_adt("Proxy").is_some();
                m.args = args.iter().map(|t| self.id(*t)).collect();
                if let Some(b) = base {
                    let b = cx.subst(b, args);
                    m.base = Some(self.id(b));
                }
                if kind == AdtKind::Class {
                    for name in [hooks::TO_JSON, hooks::TO_STRING, hooks::INSPECT] {
                        let Some((def, _)) = hooks::hook(cx, d, args, name) else {
                            continue;
                        };
                        // A result the body gives is known once the body is checked: checked now
                        // (a deferred std method may not be yet), so that the model, and the
                        // fingerprints it gives, are the same whenever they are taken.
                        let at = cx.fn_info(def).name_span;
                        crate::body::returns::ret_of(cx, def, at);
                        let Some((def, ret)) = hooks::hook(cx, d, args, name) else {
                            continue;
                        };
                        let params = cx.fn_info(def).params.len() as u32;
                        let ret = self.id(ret);
                        m.methods.push(ModelMethod {
                            name: name.to_string(),
                            params,
                            ret,
                        });
                    }
                }
                if kind == AdtKind::Class {
                    for t in subclasses(cx, d, args) {
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
                            Some(s) => json_quote(s),
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

/// The first private field std type `d` declares (its state: a runtime handle, a collection's
/// storage), if any.
fn std_private_field(cx: &Ctx, d: DefId) -> Option<String> {
    let a = cx.adt(d)?;
    (a.fields.iter())
        .find(|f| {
            f.private_to.is_some_and(|o| cx.keeps_handles(o))
                && !cx.plain_private(f.private_to, &f.name)
        })
        .map(|f| f.name.clone())
}

/// Is `d` std's `Set` (velt/collections; a class whose field 0 is the `Map` of its items)?
fn is_std_set(cx: &Ctx, d: DefId) -> bool {
    cx.adt(d).is_some_and(|a| {
        cx.scopes[a.module].is_std
            && a.qual_name.ends_with("::Set")
            && a.fields.first().is_some_and(|f| f.name == "items")
    })
}

/// The getters of object literal type `d` (`get k() { … }`, held as a function in field `k`),
/// with what each returns (as `crate::any`'s `literal_accessors` finds them).
fn literal_getters(cx: &mut Ctx, d: DefId) -> Vec<(String, TyId)> {
    let Some(a) = cx.adt(d) else {
        return vec![];
    };
    let n = a.generics.names.len() as u32;
    let fields: Vec<(String, TyId)> = a.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
    let own: Vec<TyId> = (0..n).map(|k| cx.ty.intern(TyKind::Param(k))).collect();
    let owner = cx.ty.intern(TyKind::Adt(d, own));
    let Some(iface) = cx.impls.iter().find(|i| i.ty == owner).map(|i| i.iface) else {
        return vec![];
    };
    let Some(i) = cx.iface(iface) else {
        return vec![];
    };
    let mut out = vec![];
    for m in i.methods.iter().filter(|m| m.is_getter) {
        let held = fields.iter().find(|(f, _)| *f == m.name).map(|(_, t)| *t);
        if let Some(t) = held {
            if matches!(cx.ty.kind(t), TyKind::FnPtr { .. }) && t != m.ret {
                out.push((m.name.clone(), m.ret));
            }
        }
    }
    out
}

/// The classes of the program that extend class `d<args>`, directly or not, in definition
/// order, at the type arguments `d<args>` gives them (`class Mid<T> extends Base<T>` is
/// `Mid<i64>` under `Base<i64>`); a class whose type arguments do not follow from them
/// (`class Box<T> extends Base<i64>`) is left out.
fn subclasses(cx: &mut Ctx, d: DefId, args: &[TyId]) -> Vec<TyId> {
    let mut out = vec![];
    for i in 0..cx.info.len() {
        let c = DefId(i as u32);
        let n = match &cx.info[i] {
            DefInfo::Adt(a) if a.kind == AdtKind::Class && c != d => a.generics.names.len(),
            _ => continue,
        };
        let own: Vec<TyId> = (0..n as u32)
            .map(|k| cx.ty.intern(TyKind::Param(k)))
            .collect();
        let (mut cur, mut cur_args) = (c, own);
        let mut depth = 0;
        let pattern = loop {
            depth += 1;
            let Some(b) = cx.adt(cur).and_then(|a| a.base) else {
                break None;
            };
            let b = cx.subst(b, &cur_args);
            let TyKind::Adt(bd, bargs) = cx.ty.kind(b).clone() else {
                break None;
            };
            if bd == d {
                break Some(bargs);
            }
            if depth > 64 {
                break None;
            }
            (cur, cur_args) = (bd, bargs);
        };
        let Some(pattern) = pattern else {
            continue;
        };
        // Each of `c`'s type parameters as the argument of `d` it stands for.
        let mut solved: Vec<Option<TyId>> = vec![None; n];
        for (p, a) in pattern.iter().zip(args) {
            if let TyKind::Param(k) = *cx.ty.kind(*p) {
                if let Some(s) = solved.get_mut(k as usize) {
                    *s = Some(*a);
                }
            }
        }
        let Some(solved) = solved.into_iter().collect::<Option<Vec<TyId>>>() else {
            continue;
        };
        let same = pattern.len() == args.len()
            && pattern
                .iter()
                .zip(args)
                .all(|(p, a)| cx.subst(*p, &solved) == *a);
        if same {
            out.push(cx.ty.intern(TyKind::Adt(c, solved)));
        }
    }
    out
}

impl Model {
    /// A fingerprint of what a request for type arguments `roots` reads: the types they reach,
    /// renumbered in the order a walk from `roots` meets them, without positions (`at`), so it is
    /// the same in any model that holds them and changes when any of them does
    /// (docs/internals/design/source-generators.md §9, the offered modules). `extra` is mixed in
    /// (the generated function's signature).
    pub fn fingerprint(&self, roots: &[u32], extra: &str) -> String {
        let order = self.canonical_order(roots);
        let index: FxHashMap<u32, u32> = (order.iter().enumerate())
            .map(|(i, &t)| (t, i as u32))
            .collect();
        let n = |t: u32| index.get(&t).copied().unwrap_or(u32::MAX);
        let mut h = Fnv128::new();
        h.str(extra);
        for &r in roots {
            h.u32(n(r));
        }
        for &t in &order {
            let Some(t) = self.types.get(t as usize) else {
                h.str("?");
                continue;
            };
            h.str(t.kind);
            h.str(&t.display);
            h.str(&t.qualified_name);
            h.str(&t.repr);
            h.str(&t.literal);
            h.u32(u32::from(t.may_be_proxy));
            let flags = [t.has_properties, t.class_value, t.dynamic];
            h.u32(flags.iter().fold(0, |acc, &b| acc << 1 | u32::from(b)));
            h.str(&t.private_field);
            h.str(t.ctor_access);
            for (tag, list) in [("a", &t.args), ("u", &t.arms), ("s", &t.subclasses)] {
                h.str(tag);
                h.u32(list.len() as u32);
                for &x in list {
                    h.u32(n(x));
                }
            }
            h.str("b");
            h.u32(t.base.map_or(0, |b| n(b).wrapping_add(1)));
            h.u32(t.fields.len() as u32);
            for f in &t.fields {
                h.str(&f.name);
                h.u32(n(f.ty));
                let flags = [
                    f.optional,
                    f.readonly,
                    f.private,
                    f.es_private,
                    f.has_default,
                    f.getter,
                    f.std_private,
                ];
                h.u32(flags.iter().fold(0, |acc, &b| acc << 1 | u32::from(b)));
                h.u32(f.via.map_or(0, |v| n(v).wrapping_add(1)));
            }
            h.u32(t.methods.len() as u32);
            for m in &t.methods {
                h.str(&m.name);
                h.u32(m.params);
                h.u32(n(m.ret));
            }
            h.u32(t.members.len() as u32);
            for m in &t.members {
                h.str(&m.name);
                h.str(&m.value);
            }
        }
        format!("{:032x}", h.0)
    }

    /// The types `roots` reach, in the order a walk from them meets them (the roots first, then
    /// each type's arguments, arms, subclasses, base, field types and method results): the same
    /// in any model that holds them, so a position in it names a type across checks
    /// ([`super::handles`]).
    pub fn canonical_order(&self, roots: &[u32]) -> Vec<u32> {
        let mut order: Vec<u32> = vec![];
        let mut seen: rustc_hash::FxHashSet<u32> = rustc_hash::FxHashSet::default();
        let mut visit = |t: u32, order: &mut Vec<u32>| {
            if seen.insert(t) {
                order.push(t);
            }
        };
        for &r in roots {
            visit(r, &mut order);
        }
        let mut next = 0;
        while next < order.len() {
            let Some(t) = self.types.get(order[next] as usize) else {
                next += 1;
                continue;
            };
            next += 1;
            let refs = (t.args.iter().chain(&t.arms).chain(&t.subclasses))
                .copied()
                .chain(t.base)
                .chain(t.fields.iter().flat_map(|f| [Some(f.ty), f.via]).flatten())
                .chain(t.methods.iter().map(|m| m.ret));
            for r in refs {
                visit(r, &mut order);
            }
        }
        order
    }
}

/// `s` as a JSON string (also a JavaScript string literal): what `JSON.stringify(s)` writes.
fn json_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 128-bit FNV-1a: stable across runs and platforms (the fingerprints are stored).
struct Fnv128(u128);

impl Fnv128 {
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    fn new() -> Self {
        Fnv128(0x6c62_272e_07bb_0142_62b8_2175_6295_c58d)
    }

    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= u128::from(x);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }

    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.bytes(s.as_bytes());
    }
}
