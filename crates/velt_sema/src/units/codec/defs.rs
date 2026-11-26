//! The encoding of sema's definition records (#2943 M2): every `DefInfo` (`crate::defs`) with
//! everything it holds, the aliases' `AliasInfo` and the `extend` blocks' `Extension`. The AST
//! they borrow is stored as fragments ([`super::ast_refs`]). As in [`super::hir`], each impl
//! destructures its struct or matches its enum exhaustively, so a field added to a record fails
//! to compile until it is encoded too.

use velt_syntax::ast::GeneratorSpec;

use super::{enums, structs};
use crate::body::LocalKind;
use crate::decorators::{Inline, Variant, VariantCond, VariantTest};
use crate::defs::*;
use crate::intrinsic_decls::DeclaredIntrinsic;

structs! {
    Generics { names, bounds, defaults, constraints }
    Bound { iface, args }
    ParamSig { name, span, ty, mode, default }
    ThisSig { ty, mode }
    DeclaredThrows { ty, span, from_body }
    FnInfo<'_> {
        name, name_span, span, module, kind, generics, this, params, ret, ret_span, ret_source,
        fixed_modes, source, state, throw_srcs, declared_throws, throws, local_kinds, owner,
        is_async, host, c, intrinsic, unsafe_intrinsic, rest, sig_unsafe, generator,
        extern_sig, is_generator, is_async_gen, soft_moves, is_private, is_getter, inline_marker, variant,
        inherits_this, escaping, keeps_fn_params, soft_params, modified_params,
        modified_captures, mutated_captures, mutated_through, assigns_capture,
    }
    GeneratorSpec { spec, span }
    MethodRef { def, is_static }
    FieldInfo {
        name, ty, declared, span, readonly, optional, has_default, default, default_throws,
        private_to, weak,
    }
    Redefined { field, at, name, init, throws }
    AdtInfo<'_> {
        name, qual_name, kind, module, span, generics, fields, own_fields_start, redefined, base,
        methods, ctor, own_ctor, vtable, vslots, implements, has_dispose, statics, decl,
    }
    VariantInfo { name, payload, discriminant, str_value }
    EnumInfo<'_> { name, qual_name, span, generics, variants, is_union, decl }
    IfaceMethod { name, span, params, ret, mut_this, default, is_getter, generics, throws, rest }
    IfaceInfo<'_> { name, qual_name, module, span, generics, fields, methods, parents, decl }
    GlobalInfo<'_> { name, qual_name, module, span, ty, init, state, src, mutable, stored, weak, c_const, c_global }
    GlobalSrc<'_> { ann, init, span, owner, is_private, c_const, c_global }
    AliasInfo<'_> { module, decl, expanding, used, pointees_checked, outer }
    Extension { generics, target, methods, implements }
    Variant { plain, when }
    VariantCond { arg, test }
}

enums! {
    @fielded DefInfo<'_> {
        0 => Fn(a),
        1 => Adt(a),
        2 => Enum(a),
        3 => Iface(a),
        4 => Global(a),
    }
    FnKind {
        0 => Free,
        1 => Method,
        2 => Static,
        3 => Ctor,
        4 => IfaceDefault,
        5 => Extern,
        6 => Closure,
    }
    BodyState {
        0 => Unchecked,
        1 => InProgress,
        2 => Done,
    }
    RetSource {
        0 => Known,
        1 => Body,
        2 => Base(a, b),
    }
    @fielded FnSource<'_> {
        0 => Decl(a),
        1 => Default(a, b),
    }
    @fielded ThrowSrc {
        0 => Direct(a, b),
        1 => Call(a, b, c),
        2 => Slot { iface, slot, args, span },
        3 => Defaults(a, b, c),
        4 => Indirect(a, b, c),
    }
    LocalKind {
        0 => Let,
        1 => Const,
        2 => Using,
        3 => Param,
        4 => This,
        5 => Temp,
        6 => Bind,
        7 => Elem,
        8 => Capture,
    }
    DeclaredIntrinsic {
        0 => Invalid,
        1 => Op(a),
        2 => AnyToPrimitive,
        3 => HookCall,
    }
    Inline {
        0 => Always,
        1 => Never,
        2 => Hint,
    }
    @fielded VariantTest {
        0 => StartsWith(a),
        1 => Lacks(a),
        2 => FieldIn(a, b),
    }
}
