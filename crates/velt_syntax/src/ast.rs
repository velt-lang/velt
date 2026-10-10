//! Surface AST produced by the parser.
//! CONTRACT FILE — maintainer-owned (see docs/internals/contracts/README.md).
//! Agents may not change existing items; request additions in an issue (see CONTRIBUTING.md).
//!
//! Every node carries a [`Span`]. Expressions and patterns also carry a [`NodeId`]
//! (unique per module) so later stages can keep side tables keyed by node.

use velt_common::Span;

mod jsx;

pub use jsx::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

/// Member name of the computed key `[Symbol.dispose]` (a method declaration
/// `[Symbol.dispose]() {}` or an access `x[Symbol.dispose]`): an [`Ident`] with this exact
/// name. No plain identifier can collide with it.
pub const SYMBOL_DISPOSE: &str = "[Symbol.dispose]";

/// Member name of the computed key `[Symbol.asyncDispose]` (see [`SYMBOL_DISPOSE`]).
pub const SYMBOL_ASYNC_DISPOSE: &str = "[Symbol.asyncDispose]";

/// Member name of the computed key `[Symbol.iterator]` (see [`SYMBOL_DISPOSE`]).
pub const SYMBOL_ITERATOR: &str = "[Symbol.iterator]";

/// Member name of the computed key `[Symbol.asyncIterator]` (see [`SYMBOL_DISPOSE`]).
pub const SYMBOL_ASYNC_ITERATOR: &str = "[Symbol.asyncIterator]";

#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

/// The first character of an ES private name (`#x`), kept in [`Ident::name`]: no identifier
/// can start with it, so `#x` and `x` are different members.
pub const PRIVATE_NAME_PREFIX: char = '#';

impl Ident {
    /// Is this an ES private name (`#x`)?
    pub fn is_private_name(&self) -> bool {
        self.name.starts_with(PRIVATE_NAME_PREFIX)
    }
}

#[derive(Clone, Debug)]
pub struct Module {
    pub items: Vec<Item>,
    pub span: Span,
    /// `// @jsxImportSource pkg` in the comments before the first token: the module whose
    /// `jsx-runtime` provides the JSX factories for this file.
    pub jsx_import_source: Option<String>,
}

// ───────────────────────────── Items ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Item {
    pub kind: ItemKind,
    /// `export` keyword present.
    pub exported: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ItemKind {
    /// `import { a, b as c } from "velt:fs";`, `import * as ns from "./m";`. With `exported`
    /// set it is a re-export: `export { a, b as c } from "./m";`, `export * from "./m";`, or a
    /// local export list `export { a, b as c };` (empty `from`).
    Import(Import),
    Function(FnDecl),
    /// `struct Point { x: f64, y: f64; len(): f64 { ... } }` — value type.
    Struct(TypeDecl),
    /// `class User { name: string; constructor(...) {...} greet(): string {...} }` — heap owned.
    Class(TypeDecl),
    Interface(InterfaceDecl),
    Enum(EnumDecl),
    /// `type Id = u64;`
    TypeAlias(TypeAlias),
    /// Top-level `const X: T = expr;` / `let`.
    Var(VarDecl),
    /// `extend<T> Array<T> { methods }` — add methods to an existing type (used by std/prelude).
    Extend(ExtendDecl),
    /// `declare function name(a: i64): void;` — external C-ABI symbol (used by std for rt FFI).
    ExternFn(FnSig),
}

#[derive(Clone, Debug)]
pub struct Import {
    pub names: Vec<ImportName>,
    /// Module specifier string, e.g. `"std/fs"`, `"./util"`, `"some-pkg"`; empty for a local
    /// export list `export { a, b };`.
    pub from: String,
    pub from_span: Span,
    /// `import * as ns from "…"`: the namespace name (`names` is then empty).
    pub namespace: Option<Ident>,
    /// `export * from "…"`: re-export every export of the module (`names` is then empty).
    pub all: bool,
}

#[derive(Clone, Debug)]
pub struct ImportName {
    pub name: Ident,
    pub alias: Option<Ident>,
    /// `import type { T }` / `import { type T }`: usable in type positions only (erased).
    pub type_only: bool,
}

#[derive(Clone, Debug)]
pub struct GenericParam {
    pub name: Ident,
    /// `T extends A & B`
    pub bounds: Vec<TypeExpr>,
    /// `T = Default` (used where a call, `new` or type annotation leaves `T` out).
    pub default: Option<TypeExpr>,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: Ident,
    /// For an optional parameter, the written type plus `| null`.
    pub ty: TypeExpr,
    /// For an optional parameter, `null`.
    pub default: Option<Expr>,
    /// `name?: T` — parsed as `name: T | null = null` (the flag only keeps the spelling).
    pub optional: bool,
    /// `...name: T[]` (the last parameter): a call's remaining arguments, spreads included,
    /// as one array. Parsed with the default `[]`.
    pub rest: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FnSig {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    /// `None` means `void`.
    pub ret: Option<TypeExpr>,
    /// `throws A | B` after the return type; `None`: the thrown types are inferred.
    pub throws: Option<TypeExpr>,
    pub is_async: bool,
    /// `function*` / `*name()`: a generator (`yield` is allowed in its body).
    pub is_generator: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FnDecl {
    pub sig: FnSig,
    pub body: Block,
    /// TypeScript-form overloads: the bodiless signatures written right before this
    /// implementation (`function f(a: string): A;` `function f(a: number): B;` then
    /// `function f(a: string | number): A | B { ... }`), in order. Calls choose among them;
    /// the implementation's own signature is not callable from outside. Empty for a function
    /// without overloads, and always for function expressions, object-literal methods and
    /// constructors.
    pub overloads: Vec<FnSig>,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: Ident,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
    pub readonly: bool,
    /// `name?: T` — the field has type `T | null` and defaults to `null`.
    pub optional: bool,
    /// `private name: T` — only accessible inside the declaring type's body.
    pub is_private: bool,
    /// `static readonly name: T = e;` — a per-type constant, not an instance field.
    pub is_static: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Method {
    pub decl: FnDecl,
    pub is_static: bool,
    /// `private` keyword: only callable inside the declaring type's body.
    pub is_private: bool,
    /// `get name(): T { ... }` — read as a property (`x.name`), no parameters.
    pub is_getter: bool,
    /// `set name(v: T) { ... }` — runs on assignment `x.name = v`; one parameter, no return
    /// type; it always mutates `this`. May share its name with a getter.
    pub is_setter: bool,
    /// `override` keyword: redefines a base-class method.
    pub is_override: bool,
}

/// Shared by `struct` and `class`.
#[derive(Clone, Debug)]
pub struct TypeDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    /// `extends Base<T>` — classes only, at most one.
    pub extends: Option<TypeExpr>,
    /// `implements A, B`
    pub implements: Vec<TypeExpr>,
    pub fields: Vec<Field>,
    pub constructor: Option<FnDecl>,
    /// `private constructor` / `protected constructor` (`Public` without a constructor).
    pub ctor_visibility: CtorVisibility,
    pub methods: Vec<Method>,
}

/// Who may call a class's constructor (TypeScript's rules, docs/reference/classes.md): anyone;
/// the class body and its subclasses (`protected`); only the class body, and the class cannot
/// be extended (`private`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CtorVisibility {
    #[default]
    Public,
    Protected,
    Private,
}

#[derive(Clone, Debug)]
pub struct InterfaceDecl {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub extends: Vec<TypeExpr>,
    /// Fields every implementor must have (same name and type).
    pub fields: Vec<Field>,
    pub methods: Vec<InterfaceMethod>,
}

/// Interface method: required (`body: None`) or with a default implementation.
#[derive(Clone, Debug)]
pub struct InterfaceMethod {
    pub sig: FnSig,
    pub body: Option<Block>,
    /// `get name(): T` — read as a property, no parameters.
    pub is_getter: bool,
    /// `set name(v: T)` — called by `x.name = v`; one parameter; it always mutates `this`.
    pub is_setter: bool,
}

/// `extend<T> Target<T> { methods }`
#[derive(Clone, Debug)]
pub struct ExtendDecl {
    pub generics: Vec<GenericParam>,
    pub target: TypeExpr,
    pub methods: Vec<Method>,
}

/// `enum Color { Red, Green = 5 }` (numeric, TS-style) or `enum Dir { Up = "UP" }` (string enum).
/// Tagged unions are written as discriminated unions of object types instead.
#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub name: Ident,
    pub variants: Vec<Variant>,
}

#[derive(Clone, Debug)]
pub struct Variant {
    pub name: Ident,
    /// `= 5` / `= -1` / `= "UP"`.
    pub discriminant: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TypeAlias {
    pub name: Ident,
    pub generics: Vec<GenericParam>,
    pub ty: TypeExpr,
}

// ───────────────────────────── Types ─────────────────────────────

#[derive(Clone, Debug)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum TypeExprKind {
    /// `i64`, `string`, `Map<string, i64>`, `fs.File` (path segments joined by '.').
    Named {
        path: Vec<Ident>,
        args: Vec<TypeExpr>,
    },
    /// `T[]`
    Array(Box<TypeExpr>),
    /// `[A, B]`
    Tuple(Vec<TypeExpr>),
    /// `(a: A, b: B) => R`, optionally `=> R throws E` (the function may throw `E`).
    Function {
        params: Vec<TypeExpr>,
        ret: Box<TypeExpr>,
        throws: Option<Box<TypeExpr>>,
    },
    /// `A | B` (a union type; `T | null` is an optional `T`).
    Union(Vec<TypeExpr>),
    /// `A & B` (an intersection; binds tighter than `|`). Generic bounds (`T extends A & B`)
    /// are [`GenericParam::bounds`] instead.
    Intersection(Vec<TypeExpr>),
    /// `T["k"]` (an indexed access type; the key is a string literal type or a union of them).
    Indexed {
        object: Box<TypeExpr>,
        key: Box<TypeExpr>,
    },
    /// Literal type: `"circle"`, `42`, `-1`, `1.5`, `true`.
    Literal(SignedLit),
    /// Object type `{ kind: "circle"; r: f64 }` (an anonymous object type).
    Object(Vec<ObjectTypeField>),
    /// `null` in type position.
    Null,
    /// `void`
    Void,
    /// A type predicate as a return type (TypeScript's user-defined type guards):
    /// `x is T` (the function returns a `boolean` that is `true` when parameter `x` is a `T`),
    /// `this is T`, `asserts x is T` (the function returns only when `x` is a `T`) and
    /// `asserts x` (only when `x` is truthy; `ty: None`).
    Predicate {
        param: Box<Ident>,
        ty: Option<Box<TypeExpr>>,
        asserts: bool,
    },
}

/// `name: T` / `name?: T` / `readonly name: T` in an object type.
#[derive(Clone, Debug)]
pub struct ObjectTypeField {
    pub name: Ident,
    /// For an optional field, the written type plus `| null`.
    pub ty: TypeExpr,
    /// `name?: T` — parsed as `name: T | null`; the flag makes it an optional field of the object
    /// type (left out by `JSON.stringify` while `null`). An object literal may leave out any
    /// `T | null` field of an object type.
    pub optional: bool,
    /// `readonly name: T` — the field can't be assigned (docs/internals/design/shared-models.md).
    pub readonly: bool,
    pub span: Span,
}

// ─────────────────────────── Statements ───────────────────────────

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarKind {
    Const,
    Let,
    /// `using x = e;`: a `const` whose value is disposed at the end of the enclosing block.
    Using,
    /// `await using x = e;`: `[Symbol.asyncDispose]()` is awaited at the end of the block.
    AwaitUsing,
}

impl VarKind {
    /// The declaration's keyword(s) as written: `const`, `let`, `using`, `await using`.
    pub fn keyword(self) -> &'static str {
        match self {
            VarKind::Const => "const",
            VarKind::Let => "let",
            VarKind::Using => "using",
            VarKind::AwaitUsing => "await using",
        }
    }
}

#[derive(Clone, Debug)]
pub struct VarDecl {
    pub kind: VarKind,
    pub pattern: Pattern,
    pub ty: Option<TypeExpr>,
    pub init: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Var(VarDecl),
    Expr(Expr),
    Return(Option<Expr>),
    If {
        cond: Expr,
        then: Block,
        els: Option<Box<Stmt>>,
    },
    While {
        cond: Expr,
        body: Block,
    },
    DoWhile {
        body: Block,
        cond: Expr,
    },
    /// C-style `for (init; cond; update) body`.
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        update: Option<Expr>,
        body: Block,
    },
    /// `for (const x of xs) body`, or `for await (const x of xs) body` (`is_await`).
    ForOf {
        kind: VarKind,
        pattern: Pattern,
        iter: Expr,
        body: Block,
        is_await: bool,
    },
    Break(Option<Ident>),
    Continue(Option<Ident>),
    Block(Block),
    /// `label: stmt`
    Labeled {
        label: Ident,
        body: Box<Stmt>,
    },
    Throw(Expr),
    /// `switch (discriminant) { case a: ...; default: ... }` (JS semantics: fallthrough, `break`).
    Switch {
        discriminant: Expr,
        cases: Vec<SwitchCase>,
    },
    Try {
        body: Block,
        catch: Option<(Option<Pattern>, Block)>,
        finally: Option<Block>,
    },
    /// Nested function/struct/etc. declarations inside a block.
    Item(Box<Item>),
    Empty,
}

/// `case test: body` (`test: None` for `default:`).
#[derive(Clone, Debug)]
pub struct SwitchCase {
    pub test: Option<Expr>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

// ─────────────────────────── Expressions ──────────────────────────

#[derive(Clone, Debug)]
pub struct Expr {
    pub id: NodeId,
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    /// Integer literal with optional type suffix (`10u8`, `5i32`). Value fits u128.
    Int {
        value: u128,
        suffix: Option<String>,
    },
    Float {
        value: f64,
        suffix: Option<String>,
    },
    Str(String),
    Bool(bool),
    Null,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,    // -x
    Plus,   // +x
    Not,    // !x
    BitNot, // ~x
    TypeOf, // typeof x
    Delete, // delete r[k] (only on `Record`s; sema checks)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Eq,    // == (same as === in Velt; no coercion)
    NotEq, // != / !==
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
    Nullish, // && || ??
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    /// `#x in o`: the left operand is an `Ident` named `#x` (a private name; only the parser
    /// builds one, only here). `KEY in o` / `Symbol.iterator in o`: the left operand is an
    /// `Ident` or a `Member` of `Symbol` (a symbol key).
    In,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateOp {
    Inc,
    Dec,
}

#[derive(Clone, Debug)]
pub enum ArrowBody {
    Expr(Box<Expr>),
    Block(Block),
}

#[derive(Clone, Debug)]
pub struct ArrowParam {
    pub name: Ident,
    /// Optional in arrows when inferable from context. For an optional parameter with a
    /// written type, that type plus `| null`.
    pub ty: Option<TypeExpr>,
    /// `(x: T = e) => …`: the value when a call leaves the argument out; `null` for an
    /// optional parameter.
    pub default: Option<Expr>,
    /// `(x?: T) => …`, parsed as `(x: T | null = null) => …` (the flag keeps the spelling).
    pub optional: bool,
}

#[derive(Clone, Debug)]
pub enum ObjectProp {
    /// `key: value`
    KeyValue(Ident, Expr),
    /// `{ key }`
    Shorthand(Ident),
    /// `{ ...other }`
    Spread(Expr),
    /// `{ name(params) { body } }`, `*[Symbol.iterator]() { ... }`: a method (its name in
    /// `sig.name`; generators and `async` as on class methods). Sema allows only
    /// `[Symbol.iterator]` / `[Symbol.asyncIterator]`, as the only member of the literal.
    Method(Box<FnDecl>),
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Lit(Lit),
    /// Template literal: `quasis.len() == exprs.len() + 1`. Quasis are already unescaped.
    Template {
        quasis: Vec<String>,
        exprs: Vec<Expr>,
    },
    Ident(Ident),
    This,
    /// `super` — only as `super(args)` in constructors or `super.method(args)`.
    Super,
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `a = b` (op None) or compound `a += b` (op Some(Add)).
    Assign {
        op: Option<BinaryOp>,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    /// `++x` / `x--`
    Update {
        op: UpdateOp,
        prefix: bool,
        target: Box<Expr>,
    },
    /// `cond ? a : b`
    Cond {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    /// `f(args)`; `optional` for `f?.(args)`.
    Call {
        callee: Box<Expr>,
        type_args: Vec<TypeExpr>,
        args: Vec<Expr>,
        optional: bool,
    },
    /// `new Foo<T>(args)`
    New {
        class: TypeExpr,
        args: Vec<Expr>,
    },
    /// `obj.prop` / `obj?.prop`; also `obj[Symbol.dispose]` (`prop` named [`SYMBOL_DISPOSE`]).
    Member {
        object: Box<Expr>,
        prop: Ident,
        optional: bool,
    },
    /// `obj[index]`
    /// `optional` for `obj?.[index]`.
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
        optional: bool,
    },
    Arrow {
        /// `<T>(x: T) => x`: type parameters (empty for an ordinary arrow).
        type_params: Vec<GenericParam>,
        params: Vec<ArrowParam>,
        ret: Option<TypeExpr>,
        /// `(x: T): R throws E => ...` (only after a return type).
        throws: Option<TypeExpr>,
        body: ArrowBody,
        is_async: bool,
    },
    /// `function* name(params): R { body }` (`async function*` too): a function expression.
    /// The name is optional (empty when left out). Only generators are allowed (sema); other
    /// functions are written as arrows.
    Function(Box<FnDecl>),
    /// `[a, b, ...c]` (spread elements are `Spread`).
    Array(Vec<Expr>),
    /// `{ a: 1, b }` — anonymous struct literal, or struct literal when typed by context.
    Object(Vec<ObjectProp>),
    /// `Point { x: 1, y: 2 }` — explicitly named struct literal.
    StructLit {
        name: TypeExpr,
        props: Vec<ObjectProp>,
    },
    /// `...expr` inside array literals / call args.
    Spread(Box<Expr>),
    Await(Box<Expr>),
    /// `yield expr`, bare `yield` (`arg: None`) or `yield* iterable` (`delegate`).
    Yield {
        arg: Option<Box<Expr>>,
        delegate: bool,
    },
    /// `expr as T`
    Cast {
        expr: Box<Expr>,
        ty: TypeExpr,
    },
    /// `expr instanceof C` (`C` a class type).
    InstanceOf {
        expr: Box<Expr>,
        ty: TypeExpr,
    },
    /// `(a, b)` is not a tuple; tuples use `[a, b]` with a tuple type context.
    Paren(Box<Expr>),
    /// `x!`: the value of `x`, which must not be `null` (checked: a `null` panics).
    NonNull(Box<Expr>),
    /// A JSX element or fragment: `<div class="a">{x}</div>`, `<></>`. Parentheses around an
    /// element are not kept (`(<a />)` is just the element).
    Jsx(Box<JsxElement>),
}

// ──────────────────────────── Patterns ────────────────────────────

#[derive(Clone, Debug)]
pub struct Pattern {
    pub id: NodeId,
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum PatternKind {
    /// Binding `x` (in `let`; mutability follows `let` vs `const`).
    Ident(Ident),
    /// `_`
    Wildcard,
    /// `{ a, b: renamed, ...rest }`
    Object {
        fields: Vec<(Ident, Pattern)>,
        rest: Option<Ident>,
    },
    /// `[a, b, ...rest]`
    Array {
        elems: Vec<Pattern>,
        rest: Option<Ident>,
    },
    /// `p = value` inside an object or array pattern (`{ a = 1 }`, `[x = 0]`): `value` when the
    /// field is `null` or the element is past the end (where JS reads `undefined`).
    Default {
        pattern: Box<Pattern>,
        value: Box<Expr>,
    },
}

/// A literal with an optional sign (`-1`, `-2.5`), as in literal types.
#[derive(Clone, Debug, PartialEq)]
pub struct SignedLit {
    pub lit: Lit,
    pub negative: bool,
}
