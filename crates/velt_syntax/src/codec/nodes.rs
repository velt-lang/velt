//! The encoding of every AST type, field by field in declaration order. The impls destructure
//! each struct and match each enum exhaustively, so an AST change that is not reflected here
//! does not compile. Enum tags are the variants' numbers as listed here (a duplicate is an
//! unreachable pattern, which the build denies). Tag 0 of every enum is a variant whose fields
//! decode from zeros without recursing (a damaged value reads on as zeros, `Reader::fail`).

use super::{Codec, Reader, Writer};
use crate::ast::*;

/// `Codec` for structs: `Name { field, … }`.
macro_rules! structs {
    ($($ty:ident { $($f:ident),* $(,)? })*) => {$(
        impl Codec for $ty {
            fn enc(&self, w: &mut Writer) {
                let $ty { $($f),* } = self;
                $($f.enc(w);)*
            }
            fn dec(r: &mut Reader) -> Self {
                // Struct expression fields are evaluated in the order written.
                $ty { $($f: Codec::dec(r)),* }
            }
        }
    )*};
}

/// `Codec` for enums: `Name { tag => Unit, tag => Tuple(a, b), tag => Fields { x, y } }`.
macro_rules! enums {
    ($($ty:ident {
        $($tag:literal => $v:ident $(( $($t:ident),* ))? $({ $($f:ident),* })?),* $(,)?
    })*) => {$(
        impl Codec for $ty {
            fn enc(&self, w: &mut Writer) {
                match self {
                    $($ty::$v $(( $($t),* ))? $({ $($f),* })? => {
                        w.byte($tag);
                        $($($t.enc(w);)*)?
                        $($($f.enc(w);)*)?
                    })*
                }
            }
            fn dec(r: &mut Reader) -> Self {
                match r.byte() {
                    $($tag => $ty::$v
                        $(( $({ let $t = Codec::dec(r); $t }),* ))?
                        $({ $($f: Codec::dec(r)),* })?,)*
                    // Damaged: read on as zeros (tag 0, whose fields end the recursion).
                    _ => {
                        r.fail();
                        Self::dec(r)
                    }
                }
            }
        }
    )*};
}

structs! {
    Ident { name, span }
    Module { items, span, jsx_import_source }
    Item { kind, exported, span }
    Import { names, from, from_span, namespace, all, default_import }
    ImportName { name, alias, type_only }
    GenericParam { name, bounds, default }
    Param { name, pattern, ty, default, optional, rest, span }
    FnSig {
        name, generics, params, ret, throws, is_async, is_unsafe, is_system, is_intrinsic, generator, host, c,
        is_generator, this_param, span,
    }
    GeneratorSpec { spec, span }
    HostModule { module, span }
    CLibrary { library, span }
    FnDecl { sig, body, overloads }
    TypeExpr { kind, span }
    Block { stmts, span }
    VarDecl { kind, pattern, ty, init, span }
    Stmt { kind, span }
    SwitchCase { test, body, span }
    Expr { id, kind, span }
    ArrowParam { name, pattern, ty, default, optional, rest }
    Pattern { id, kind, span }
    SignedLit { lit, negative }
    // blocks
    Namespace { name, items, declare }
    Augment { module, items }
    // decls
    Field { name, ty, default, readonly, optional, is_private, is_static, span }
    Method { decl, is_static, is_private, is_getter, is_setter, is_override, is_abstract }
    TypeDecl {
        name, generics, extends, implements, fields, constructor, ctor_visibility, methods,
        is_abstract, c_type,
    }
    InterfaceDecl { name, generics, extends, fields, methods, call_sigs }
    InterfaceMethod { sig, body, is_getter, is_setter }
    ExtendDecl { generics, target, implements, methods }
    EnumDecl { name, variants }
    Variant { name, discriminant, span }
    TypeAlias { name, generics, ty }
    // jsx
    JsxElement { name, type_args, attrs, children, closing_name, span }
    // type_level
    ConditionalType { check, extends, then, else_ }
    MappedType { param, source, name, readonly, optional, value }
    // type_parts
    CallSig { sig, construct, span }
    IndexKey { name, ty }
    ObjectTypeField { name, ty, optional, readonly, span, accessors }
    TypeFieldAccessor { setter_param, ty, span }
}

enums! {
    ItemKind {
        0 => Import(a),
        1 => Function(a),
        2 => Struct(a),
        3 => Class(a),
        4 => Interface(a),
        5 => Enum(a),
        6 => TypeAlias(a),
        7 => Var(a),
        8 => Extend(a),
        9 => ExternFn(a),
        10 => Augment(a),
        11 => Namespace(a),
    }
    TypeExprKind {
        0 => Named { path, args },
        1 => Array(a),
        2 => Tuple(a),
        3 => TupleRest(a),
        4 => TupleOptional(a),
        5 => Function { params, ret, throws, rest },
        6 => GenericFunction { type_params, ty },
        7 => Construct { ty, is_abstract },
        8 => Union(a),
        9 => Intersection(a),
        10 => Indexed { object, key },
        11 => Literal(a),
        12 => Object(a),
        13 => IndexSig { fields, key, value, readonly },
        14 => Callable { sigs, members },
        15 => Query(a),
        16 => KeyOf(a),
        17 => Conditional(a),
        18 => Infer(a),
        19 => Mapped(a),
        20 => TemplateLiteral { quasis, types },
        21 => Null,
        22 => Undefined,
        23 => Void,
        24 => Predicate { param, ty, asserts },
    }
    VarKind {
        0 => Const,
        1 => Let,
        2 => Using,
        3 => AwaitUsing,
    }
    StmtKind {
        0 => Var(a),
        1 => Expr(a),
        2 => Return(a),
        3 => If { cond, then, els },
        4 => While { cond, body },
        5 => DoWhile { body, cond },
        6 => For { init, cond, update, body },
        7 => ForOf { kind, pattern, iter, body, is_await },
        8 => Break(a),
        9 => Continue(a),
        10 => Block(a),
        11 => Labeled { label, body },
        12 => Throw(a),
        13 => Switch { discriminant, cases },
        14 => Try { body, catch, finally },
        15 => Item(a),
        16 => Empty,
        17 => Unsafe(a),
    }
    Lit {
        0 => Int { value, suffix },
        1 => Float { value, suffix },
        2 => Str(a),
        3 => Bool(a),
        4 => Null,
        5 => Undefined,
    }
    UnaryOp {
        0 => Neg,
        1 => Plus,
        2 => Not,
        3 => BitNot,
        4 => TypeOf,
        5 => Delete,
        6 => Void,
    }
    BinaryOp {
        0 => Add,
        1 => Sub,
        2 => Mul,
        3 => Div,
        4 => Rem,
        5 => Pow,
        6 => Eq,
        7 => NotEq,
        8 => StrictEq,
        9 => StrictNotEq,
        10 => Lt,
        11 => LtEq,
        12 => Gt,
        13 => GtEq,
        14 => And,
        15 => Or,
        16 => Nullish,
        17 => BitAnd,
        18 => BitOr,
        19 => BitXor,
        20 => Shl,
        21 => Shr,
        22 => UShr,
        23 => In,
    }
    UpdateOp {
        0 => Inc,
        1 => Dec,
    }
    ArrowBody {
        0 => Expr(a),
        1 => Block(a),
    }
    ObjectProp {
        0 => KeyValue(a, b),
        1 => Shorthand(a),
        2 => Spread(a),
        3 => Method(a),
    }
    ExprKind {
        0 => Lit(a),
        1 => Template { quasis, exprs },
        2 => Ident(a),
        3 => This,
        4 => Super,
        5 => Unary { op, expr },
        6 => Binary { op, lhs, rhs },
        7 => Assign { op, target, value },
        8 => Update { op, prefix, target },
        9 => Cond { cond, then, els },
        10 => Call { callee, type_args, args, optional },
        11 => New { class, args },
        12 => Member { object, prop, optional },
        13 => Index { object, index, more, optional },
        14 => Arrow { type_params, params, ret, throws, body, is_async },
        15 => Function(a),
        16 => Array(a),
        17 => Object(a),
        18 => StructLit { name, props },
        19 => Spread(a),
        20 => Await(a),
        21 => Yield { arg, delegate },
        22 => Cast { expr, ty },
        23 => InstanceOf { expr, ty },
        24 => Paren(a),
        25 => NonNull(a),
        26 => Jsx(a),
        27 => Unsafe(a),
    }
    PatternKind {
        0 => Ident(a),
        1 => Wildcard,
        2 => Object { fields, rest },
        3 => Array { elems, rest },
        4 => Default { pattern, value },
    }
    CtorVisibility {
        0 => Public,
        1 => Protected,
        2 => Private,
    }
    MappedModifier {
        0 => Keep,
        1 => Add,
        2 => Remove,
    }
    JsxName {
        0 => Ident(a),
        1 => Member(a),
        2 => Namespaced(a, b),
    }
    JsxAttrName {
        0 => Ident(a),
        1 => Namespaced(a, b),
    }
    JsxAttr {
        0 => Named { name, value, span },
        1 => Spread { expr, span },
    }
    JsxAttrValue {
        0 => Str { value, span },
        1 => Expr { expr, span },
        2 => Element(a),
    }
    JsxChild {
        0 => Text { value, span },
        1 => Expr { expr, span },
        2 => Spread { expr, span },
        3 => Element(a),
    }
}
