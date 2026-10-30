//! Declarations printed as TypeScript (`.d.ts`), for the `velt` type package ([`crate::dts`]):
//! the declaration [`crate::sig`] prints in Velt, in the types `tsc` knows. Velt's numeric types
//! are `number`, `bool` is `boolean`, `shared<T>` is `T` and `Promise<T, E>` is `Promise<T>`
//! (TypeScript types no errors: `throws` clauses are left out). A parameter with a default is
//! optional, and a declaration has no body or initializer.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use velt_syntax::ast::{self, GenericParam, Item, ItemKind, TypeExpr, TypeExprKind};

use crate::sig::Printer;

/// Velt's numeric types, all `number` in TypeScript.
pub(crate) const NUMBERS: &[&str] = &[
    "i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32", "u64", "usize", "f32", "f64", "number",
];

/// Velt's types whose second type argument is the error type, which TypeScript leaves out
/// (`Promise<T, E>` is `Promise<T>`; TypeScript's iterators' second argument is the value
/// returned at the end).
const ERROR_CARRYING: &[&str] = &[
    "Promise",
    "Iterator",
    "Iterable",
    "AsyncIterator",
    "AsyncIterable",
    "IterableIterator",
    "IteratorObject",
    "AsyncIterableIterator",
    "Generator",
    "AsyncGenerator",
    "PromiseSettledResult",
    "PromiseWithResolvers",
];

/// Prints TypeScript declarations of one source file.
pub(crate) struct TsPrinter<'a> {
    pub(crate) p: &'a Printer<'a>,
    /// The doc comment (Markdown) of the declaration starting at a byte offset; empty if none.
    pub(crate) doc: &'a dyn Fn(u32) -> String,
    /// The type names the declarations printed so far refer to (a qualified name's first part).
    pub(crate) refs: RefCell<BTreeMap<String, usize>>,
    /// The type parameters of the signatures being printed (not references to types).
    pub(crate) scope: RefCell<Vec<String>>,
    /// Type names printed as something else (an extension's type parameters in the `this`
    /// form's conditional types: `infer T`, `unknown`).
    pub(crate) subst: RefCell<HashMap<String, String>>,
}

/// A top-level declaration in TypeScript.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TsDecl {
    /// The declared name (an item exported under another name keeps it here).
    pub name: String,
    /// The declaration without `export` or `declare` (`function f(x: number): string;`, `class
    /// C { … }`), its doc comment above it; for an extension, the members of the interface that
    /// adds them to its target (`interface Array<T> { … }`). Empty for what TypeScript cannot
    /// declare.
    pub text: String,
    /// The type names it refers to (generic parameters excluded), each with the most type
    /// arguments it is given.
    pub refs: Vec<(String, usize)>,
    /// Whether an interface may merge into it: a class, struct or interface (also a built-in
    /// type: `type i8x16 = i8x16`, an opaque class here).
    pub merges: bool,
    /// For an extension: its target and its members in both forms ([`TsExtension`]); `text` is
    /// empty.
    pub ext: Option<TsExtension>,
}

/// An extension's methods, to be added to its target's interface. They merge as written when
/// the extension's type parameters are the target's (`extend<T> Array<T>`); otherwise (`extend
/// Array<string>`, `extend<T> Array<T[]>`) each method takes a `this` parameter of the target
/// type it extends, as TypeScript's own `flat` does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TsExtension {
    /// The target interface (`Array`; `String`, `Number` and `Boolean` for Velt's primitives).
    pub target: String,
    /// Its type arguments as TypeScript spells them (`["T[]"]`).
    pub args: Vec<String>,
    /// The members.
    pub members: Vec<TsExtMember>,
}

/// A member of an extension, in both forms.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TsExtMember {
    /// A getter or setter (a property: TypeScript's own interfaces may declare it already).
    pub accessor: bool,
    /// As written, the extension's type parameters taken as the target's (indented, its doc
    /// comment above).
    pub direct: String,
    /// In the `this` form; `None` when there is none (a setter). A getter is a property of a
    /// conditional type on the target's first type parameter, written [`FIRST_PARAM`] here.
    pub this_form: Option<String>,
}

/// Stands for the target's first type parameter in [`TsExtension::this_form`].
pub const FIRST_PARAM: &str = "\u{0}P0\u{0}";

impl TsPrinter<'_> {
    /// `item` (declared as `name`) as TypeScript.
    pub(crate) fn decl(&self, item: &Item, name: &str) -> TsDecl {
        self.refs.borrow_mut().clear();
        let mut ext = None;
        let (body, generics): (String, Vec<String>) = match &item.kind {
            ItemKind::Function(f) => {
                let sigs = match f.overloads.is_empty() {
                    true => std::slice::from_ref(&f.sig),
                    false => &f.overloads[..],
                };
                let lines: Vec<String> = sigs
                    .iter()
                    .map(|s| format!("function {};", self.fn_sig(s)))
                    .collect();
                (lines.join("\n"), names_of(&f.sig.generics))
            }
            ItemKind::Struct(t) | ItemKind::Class(t) => {
                (self.class(t, name), names_of(&t.generics))
            }
            ItemKind::Interface(i) => (self.interface(i, name), names_of(&i.generics)),
            ItemKind::Enum(e) => {
                let variants: Vec<String> = e
                    .variants
                    .iter()
                    .map(|v| {
                        let text = self.p.flat(v.span);
                        let text = text.trim_end_matches(',');
                        indented(&self.doc_comment(v.span.lo), &format!("{text},"))
                    })
                    .collect();
                (format!("enum {name} {{\n{}}}", variants.concat()), vec![])
            }
            // `type i8x16 = i8x16`: a type the compiler provides, opaque in TypeScript.
            ItemKind::TypeAlias(t) if is_self_alias(&t.ty, name) => (
                format!("class {name} {{\n  private constructor();\n}}"),
                vec![],
            ),
            ItemKind::TypeAlias(t) => (
                format!(
                    "type {name}{} = {};",
                    self.generics(&t.generics),
                    self.ty(&t.ty)
                ),
                names_of(&t.generics),
            ),
            ItemKind::Var(v) => (self.constant(v, name), vec![]),
            ItemKind::Extend(e) => {
                ext = self.extension(e);
                (String::new(), names_of(&e.generics))
            }
            ItemKind::Import(_)
            | ItemKind::ExternFn(_)
            | ItemKind::Augment(_)
            | ItemKind::Namespace(_) => return TsDecl::default(),
        };
        let refs = self
            .refs
            .borrow()
            .iter()
            .filter(|(r, _)| !generics.contains(r) && r.as_str() != name)
            .map(|(r, n)| (r.clone(), *n))
            .collect();
        let merges = match &item.kind {
            ItemKind::Struct(_) | ItemKind::Class(_) | ItemKind::Interface(_) => true,
            ItemKind::TypeAlias(t) => is_self_alias(&t.ty, name),
            _ => false,
        };
        let doc = match item.kind {
            ItemKind::Extend(_) => String::new(),
            _ => self.doc_comment(item.span.lo),
        };
        TsDecl {
            name: name.to_string(),
            text: format!("{doc}{body}"),
            refs,
            merges,
            ext,
        }
    }

    fn class(&self, t: &ast::TypeDecl, name: &str) -> String {
        let mut members: Vec<String> = t
            .fields
            .iter()
            .filter(|f| !f.is_private)
            .map(|f| indented(&self.doc_comment(f.span.lo), &self.field(f)))
            .collect();
        match (&t.constructor, t.ctor_visibility) {
            (_, ast::CtorVisibility::Private) => {
                members.push(indented("", "private constructor();"))
            }
            (Some(ctor), vis) => {
                let params: Vec<String> = ctor.sig.params.iter().map(|p| self.param(p)).collect();
                let vis = match vis {
                    ast::CtorVisibility::Protected => "protected ",
                    _ => "",
                };
                let text = format!("{vis}constructor({});", params.join(", "));
                members.push(indented(&self.doc_comment(ctor.sig.span.lo), &text));
            }
            (None, _) => {}
        }
        members.extend(
            t.methods
                .iter()
                .filter(|m| !m.is_private)
                .map(|m| self.method_in(m, &t.generics)),
        );
        let keyword = if t.is_abstract {
            "abstract class"
        } else {
            "class"
        };
        let extends: Vec<TypeExpr> = t.extends.iter().cloned().collect();
        format!(
            "{} {{\n{}}}",
            self.type_header(keyword, name, &t.generics, &extends, &t.implements),
            members.concat()
        )
    }

    fn interface(&self, i: &ast::InterfaceDecl, name: &str) -> String {
        let mut members: Vec<String> = i
            .fields
            .iter()
            .map(|f| indented(&self.doc_comment(f.span.lo), &self.field(f)))
            .collect();
        for m in &i.methods {
            let text = self.accessor(&m.sig, false, m.is_getter, m.is_setter, false);
            members.push(indented(&self.doc_comment(m.sig.span.lo), &text));
        }
        for c in &i.call_sigs {
            let sig = self.fn_sig(&c.sig);
            let call = sig.strip_prefix(c.sig.name.name.as_str()).unwrap_or(&sig);
            let text = match c.construct {
                true => format!("new {call};"),
                false => format!("{call};"),
            };
            members.push(indented(&self.doc_comment(c.span.lo), &text));
        }
        format!(
            "{} {{\n{}}}",
            self.type_header("interface", name, &i.generics, &i.extends, &[]),
            members.concat()
        )
    }

    pub(crate) fn method(&self, m: &ast::Method) -> String {
        self.method_in(m, &[])
    }

    /// A method of a type with type parameters `generics` (which a static one declares too).
    fn method_in(&self, m: &ast::Method, generics: &[GenericParam]) -> String {
        let sigs = match m.decl.overloads.is_empty() {
            true => std::slice::from_ref(&m.decl.sig),
            false => &m.decl.overloads[..],
        };
        let outer = if m.is_static { generics } else { &[] };
        let lines: Vec<String> = sigs
            .iter()
            .map(|s| {
                let flags = (m.is_static, m.is_getter, m.is_setter, m.is_abstract);
                self.accessor_in(s, flags, outer)
            })
            .collect();
        indented(&self.doc_comment(m.decl.sig.span.lo), &lines.join("\n  "))
    }

    /// A method or accessor: `[static ][abstract ]name(params): R;`, `get name(): T;`,
    /// `set name(v: T);`.
    pub(crate) fn accessor(
        &self,
        sig: &ast::FnSig,
        is_static: bool,
        getter: bool,
        setter: bool,
        is_abstract: bool,
    ) -> String {
        self.accessor_in(sig, (is_static, getter, setter, is_abstract), &[])
    }

    /// [`Self::accessor`], flags `(static, getter, setter, abstract)`, with type parameters
    /// `outer` first ([`Self::fn_sig_with`]).
    fn accessor_in(
        &self,
        sig: &ast::FnSig,
        (is_static, getter, setter, is_abstract): (bool, bool, bool, bool),
        outer: &[GenericParam],
    ) -> String {
        let mut out = String::new();
        if is_static {
            out.push_str("static ");
        }
        if is_abstract {
            out.push_str("abstract ");
        }
        if setter {
            let params: Vec<String> = sig.params.iter().map(|p| self.param(p)).collect();
            return format!("{out}set {}({});", sig.name.name, params.join(", "));
        }
        if getter {
            out.push_str("get ");
        }
        format!("{out}{};", self.fn_sig_with(sig, outer))
    }

    /// `const NAME: T;`: an untyped constant shows a literal initializer (`const N = 10;`), or
    /// the class it constructs.
    fn constant(&self, v: &ast::VarDecl, name: &str) -> String {
        if let Some(ty) = &v.ty {
            return format!("const {name}: {};", self.ty(ty));
        }
        let init = v
            .init
            .as_ref()
            .map(|e| self.p.flat(e.span))
            .unwrap_or_default();
        if is_literal(&init) {
            return format!("const {name} = {init};");
        }
        let class = init
            .strip_prefix("new ")
            .and_then(|rest| rest.split(['(', '<']).next())
            .filter(|c| !c.is_empty() && c.chars().all(|c| c.is_alphanumeric() || c == '_'));
        match class {
            Some(class) => {
                self.refer(class, 0);
                format!("const {name}: {class};")
            }
            None => format!("const {name}: unknown;"),
        }
    }

    /// The doc comment above `lo` as a `/** … */` block, or nothing.
    pub(crate) fn doc_comment(&self, lo: u32) -> String {
        let doc = (self.doc)(lo);
        let doc = doc.trim();
        if doc.is_empty() {
            return String::new();
        }
        let mut out = "/**\n".to_string();
        for line in doc.lines() {
            // `*/` would end the comment early.
            let line = line.replace("*/", "*\\/");
            match line.is_empty() {
                true => out.push_str(" *\n"),
                false => out.push_str(&format!(" * {line}\n")),
            }
        }
        out.push_str(" */\n");
        out
    }

    /// `name<T>(params): R` (no keyword or modifiers).
    pub(crate) fn fn_sig(&self, sig: &ast::FnSig) -> String {
        self.fn_sig_with(sig, &[])
    }

    /// [`Self::fn_sig`] with type parameters `outer` first (a static method's class's: TypeScript's
    /// static members can't use the class's own), except those the signature declares again.
    pub(crate) fn fn_sig_with(&self, sig: &ast::FnSig, outer: &[GenericParam]) -> String {
        self.fn_sig_this(sig, outer, None)
    }

    /// [`Self::fn_sig_with`] with a `this: T` parameter first (`this_ty`).
    pub(crate) fn fn_sig_this(
        &self,
        sig: &ast::FnSig,
        outer: &[GenericParam],
        this_ty: Option<&str>,
    ) -> String {
        let outer: Vec<GenericParam> = outer
            .iter()
            .filter(|o| sig.generics.iter().all(|g| g.name.name != o.name.name))
            .cloned()
            .collect();
        let mark = self.enter(&outer);
        self.enter(&sig.generics);
        let mut params: Vec<String> = this_ty.map(|t| format!("this: {t}")).into_iter().collect();
        params.extend(sig.params.iter().map(|p| self.param(p)));
        let ret = match &sig.ret {
            Some(ret) => self.ty(ret),
            None if sig.is_async => "Promise<void>".to_string(),
            None => "void".to_string(),
        };
        // Of the outer type parameters, those the signature uses.
        let text = format!("{} {ret}", params.join(" "));
        let mut generics: Vec<GenericParam> = outer
            .into_iter()
            .filter(|o| mentions(&text, &o.name.name))
            .collect();
        generics.extend(sig.generics.iter().cloned());
        let generics = self.generics(&generics);
        self.scope.borrow_mut().truncate(mark);
        format!("{}{generics}({}): {ret}", sig.name.name, params.join(", "))
    }

    /// Bring type parameters `generics` into scope; returns what to truncate the scope to after.
    pub(crate) fn enter(&self, generics: &[GenericParam]) -> usize {
        let mut scope = self.scope.borrow_mut();
        let mark = scope.len();
        scope.extend(generics.iter().map(|g| g.name.name.clone()));
        mark
    }

    /// Record a reference to type `name` with `arity` type arguments, unless it is a type
    /// parameter in scope.
    pub(crate) fn refer(&self, name: &str, arity: usize) {
        if self.scope.borrow().iter().any(|s| s == name) {
            return;
        }
        let mut refs = self.refs.borrow_mut();
        let n = refs.entry(name.to_string()).or_insert(0);
        *n = (*n).max(arity);
    }

    pub(crate) fn param(&self, p: &ast::Param) -> String {
        // A destructured parameter is a plain one in a declaration.
        let name = match &p.pattern {
            Some(_) => "arg".to_string(),
            None => p.name.name.clone(),
        };
        if p.rest {
            return format!("...{name}: {}", self.ty(&p.ty));
        }
        if p.optional {
            return format!("{name}?: {}", self.ty_optional(&p.ty));
        }
        let q = if p.default.is_some() { "?" } else { "" };
        format!("{name}{q}: {}", self.ty(&p.ty))
    }

    /// `<T, U extends A & B, V = D>`, or nothing.
    pub(crate) fn generics(&self, generics: &[GenericParam]) -> String {
        if generics.is_empty() {
            return String::new();
        }
        let params: Vec<String> = generics
            .iter()
            .map(|g| {
                let mut out = g.name.name.clone();
                if !g.bounds.is_empty() {
                    let bounds: Vec<String> = g.bounds.iter().map(|b| self.operand(b)).collect();
                    out.push_str(&format!(" extends {}", bounds.join(" & ")));
                }
                if let Some(default) = &g.default {
                    out.push_str(&format!(" = {}", self.ty(default)));
                }
                out
            })
            .collect();
        format!("<{}>", params.join(", "))
    }

    /// `class Name<T> extends Base implements A, B` (a struct is a class; also `interface`).
    pub(crate) fn type_header(
        &self,
        keyword: &str,
        name: &str,
        generics: &[GenericParam],
        extends: &[TypeExpr],
        implements: &[TypeExpr],
    ) -> String {
        let mut out = format!("{keyword} {name}{}", self.generics(generics));
        for (word, list) in [("extends", extends), ("implements", implements)] {
            if !list.is_empty() {
                let tys: Vec<String> = list.iter().map(|t| self.operand(t)).collect();
                out.push_str(&format!(" {word} {}", tys.join(", ")));
            }
        }
        out
    }

    /// A field: `[static ][readonly ]name[?]: T;`.
    pub(crate) fn field(&self, f: &ast::Field) -> String {
        let mut out = String::new();
        if f.is_static {
            out.push_str("static ");
        }
        if f.readonly {
            out.push_str("readonly ");
        }
        let ty = match f.optional {
            true => self.ty_optional(&f.ty),
            false => self.ty(&f.ty),
        };
        let q = if f.optional { "?" } else { "" };
        out.push_str(&format!("{}{q}: {ty};", f.name.name));
        out
    }

    /// A type in a position that accepts unions.
    pub(crate) fn ty(&self, t: &TypeExpr) -> String {
        match &t.kind {
            TypeExprKind::Named { path, args } => self.named(path, args),
            TypeExprKind::Array(elem) => format!("{}[]", self.operand(elem)),
            TypeExprKind::Tuple(elems) => {
                let elems: Vec<String> = elems.iter().map(|e| self.ty(e)).collect();
                format!("[{}]", elems.join(", "))
            }
            TypeExprKind::Function {
                params, ret, rest, ..
            } => {
                let mut params: Vec<String> = params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| self.fn_type_param(i, p))
                    .collect();
                if let Some(last) = params.last_mut().filter(|_| *rest) {
                    last.insert_str(0, "...");
                }
                format!("({}) => {}", params.join(", "), self.ty(ret))
            }
            TypeExprKind::GenericFunction { type_params, ty } => {
                let mark = self.enter(type_params);
                let out = format!("{}{}", self.generics(type_params), self.ty(ty));
                self.scope.borrow_mut().truncate(mark);
                out
            }
            TypeExprKind::Construct { ty, is_abstract } => {
                let kw = if *is_abstract { "abstract new" } else { "new" };
                format!("{kw} {}", self.ty(ty))
            }
            TypeExprKind::TupleRest(ty) => format!("...{}", self.ty(ty)),
            TypeExprKind::TupleOptional(ty) => format!("{}?", self.ty(ty)),
            TypeExprKind::Union(members) => {
                let members: Vec<String> = members.iter().map(|m| self.operand(m)).collect();
                members.join(" | ")
            }
            TypeExprKind::Intersection(members) => {
                let members: Vec<String> = members.iter().map(|m| self.operand(m)).collect();
                members.join(" & ")
            }
            TypeExprKind::Indexed { object, key } => {
                format!("{}[{}]", self.operand(object), self.ty(key))
            }
            TypeExprKind::Object(fields) => {
                let fields: Vec<String> = fields.iter().map(|f| self.object_field(f)).collect();
                match fields.is_empty() {
                    true => "{}".into(),
                    false => format!("{{ {} }}", fields.join("; ")),
                }
            }
            TypeExprKind::IndexSig {
                fields,
                key,
                value,
                readonly,
            } => {
                let mut parts: Vec<String> = fields.iter().map(|f| self.object_field(f)).collect();
                let readonly = if *readonly { "readonly " } else { "" };
                parts.push(format!(
                    "{readonly}[{}: {}]: {}",
                    key.name.name,
                    self.ty(&key.ty),
                    self.ty(value)
                ));
                format!("{{ {} }}", parts.join("; "))
            }
            TypeExprKind::KeyOf(inner) => format!("keyof {}", self.operand(inner)),
            // The rest is spelled as in TypeScript already (literal, conditional, mapped and
            // template literal types, `typeof`, `null`, predicates).
            _ => self.p.ty(t),
        }
    }

    fn object_field(&self, f: &ast::ObjectTypeField) -> String {
        let readonly = if f.readonly { "readonly " } else { "" };
        match f.optional {
            true => format!("{readonly}{}?: {}", f.name.name, self.ty_optional(&f.ty)),
            false => format!("{readonly}{}: {}", f.name.name, self.ty(&f.ty)),
        }
    }

    /// The type of `name?: T` as written: the parser added a zero-width `| undefined` (or
    /// `| null`) that the `?` stands for.
    fn ty_optional(&self, t: &TypeExpr) -> String {
        let TypeExprKind::Union(members) = &t.kind else {
            return self.ty(t);
        };
        let members: Vec<String> = members
            .iter()
            .filter(|m| m.span.lo != m.span.hi)
            .map(|m| self.operand(m))
            .collect();
        members.join(" | ")
    }

    /// A type as an operand of `|`, `&` or `[]`: parenthesized when it is a function or a union.
    fn operand(&self, t: &TypeExpr) -> String {
        let s = self.ty(t);
        match &t.kind {
            TypeExprKind::Function { .. }
            | TypeExprKind::GenericFunction { .. }
            | TypeExprKind::Union(_)
            | TypeExprKind::Intersection(_)
            | TypeExprKind::Construct { .. }
            | TypeExprKind::Conditional(_) => format!("({s})"),
            TypeExprKind::Named { path, args }
                if path.len() == 1 && args.len() == 1 && is_transparent(&path[0].name) =>
            {
                self.operand(&args[0])
            }
            _ => s,
        }
    }

    /// A function type's parameter: TypeScript needs a name, so one is made up where the
    /// source has none.
    fn fn_type_param(&self, i: usize, t: &TypeExpr) -> String {
        let optional = self.p.src.get(..t.span.lo as usize).is_some_and(|before| {
            let before = before.trim_end();
            before
                .strip_suffix(':')
                .is_some_and(|b| b.trim_end().ends_with('?'))
        });
        let name = self.p.param_name_before(t.span.lo);
        match (name, optional) {
            (Some(name), true) => format!("{name}?: {}", self.ty_optional(t)),
            (Some(name), false) => format!("{name}: {}", self.ty(t)),
            (None, _) => format!("arg{i}: {}", self.ty(t)),
        }
    }

    fn named(&self, path: &[ast::Ident], args: &[TypeExpr]) -> String {
        let names: Vec<&str> = path.iter().map(|s| s.name.as_str()).collect();
        if let (true, Some(to)) = (
            args.is_empty() && names.len() == 1,
            self.subst.borrow().get(names[0]),
        ) {
            return to.clone();
        }
        let name = names.join(".");
        let builtin = names.len() == 1
            && (NUMBERS.contains(&names[0]) || names[0] == "bool" || is_transparent(names[0]));
        if !builtin {
            self.refer(names[0], if names.len() == 1 { args.len() } else { 0 });
        }
        if names.len() == 1 {
            let n = names[0];
            if NUMBERS.contains(&n) {
                return "number".into();
            }
            match (n, args) {
                ("bool", _) => return "boolean".into(),
                // What it shares: TypeScript has one thread.
                (n, [inner]) if is_transparent(n) => return self.ty(inner),
                // TypeScript's promises and iterators carry no error type.
                (n, [value, _, ..]) if ERROR_CARRYING.contains(&n) => {
                    return format!("{n}<{}>", self.ty(value));
                }
                // A proxy is its target in TypeScript (`Proxy` is a value there, not a type).
                ("Proxy", [target]) => return self.ty(target),
                ("Proxy", []) => return "object".into(),
                _ => {}
            }
        }
        match args.is_empty() {
            true => name,
            false => {
                let args: Vec<String> = args.iter().map(|a| self.ty(a)).collect();
                format!("{name}<{}>", args.join(", "))
            }
        }
    }
}

/// `shared<T>`: the same `T` in TypeScript.
fn is_transparent(name: &str) -> bool {
    name == "shared"
}

/// Generic parameters' names.
fn names_of(generics: &[GenericParam]) -> Vec<String> {
    generics.iter().map(|g| g.name.name.clone()).collect()
}

/// A class member (`text`, possibly several lines) indented in its body, its doc comment above.
pub(crate) fn indented(doc: &str, text: &str) -> String {
    let mut out = String::new();
    for line in doc.lines() {
        out.push_str(&format!("  {line}\n"));
    }
    out.push_str(&format!("  {text}\n"));
    out
}

/// A number, string or boolean literal (what an untyped `const` may show in a declaration).
fn is_literal(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    matches!(text, "true" | "false")
        || (!digits.is_empty()
            && digits
                .chars()
                .all(|c| c.is_ascii_hexdigit() || "._xXoObBeE+".contains(c))
            && digits.starts_with(|c: char| c.is_ascii_digit()))
        || ((text.starts_with('"') && text.ends_with('"')
            || text.starts_with('\'') && text.ends_with('\''))
            && text.len() >= 2)
}

/// `type X = X`: the alias of a type the compiler provides (`i8x16`).
fn is_self_alias(ty: &TypeExpr, name: &str) -> bool {
    matches!(&ty.kind, TypeExprKind::Named { path, args }
        if args.is_empty() && path.len() == 1 && path[0].name == name)
}

/// Whether type text `ty` mentions the name `name` (as a whole word).
pub(crate) fn mentions(ty: &str, name: &str) -> bool {
    ty.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .any(|w| w == name)
}
