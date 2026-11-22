//! The encoding of every HIR type a unit saves (#2943 M2): the definitions (`hir::Def`), the
//! interface implementations (`hir::ImplDef`) and the type table's kinds (`hir::TyKind`), with
//! everything they hold, and the element types of the program's tables. Each impl destructures
//! its struct or matches its enum exhaustively (the `structs!` / `enums!` macros), so a HIR change
//! that is not reflected here does not compile. Tags are the variants' numbers as listed (a
//! duplicate is an unreachable pattern, which the build denies); tag 0 of every recursive enum is
//! a variant whose fields decode from zeros without recursing.

use super::{enums, structs};
use crate::hir::*;

structs! {
    // Definitions.
    FnDef {
        name, generics, params, ret, is_async, is_generator, self_ty, captures,
        shares_captures, body, throws, span,
    }
    Param { local, ty, mode }
    Capture { outer, inner, mode, share }
    Body { locals, block }
    LocalDef { name, ty, mutable, boxed, span }
    ExternFnDef { name, symbol, params, ret, is_async, host, c, is_decorator, span }
    CImport { library, errno, params, ret, variadic }
    HostImport { module, params, ret }
    AdtDef {
        name, kind, generics, fields, is_copy, declared_struct, c_struct, private_fields, opaque,
        base, inherited_inits, ctor, dispose, to_string, to_json, inspect, vtable, span,
    }
    FieldDef { name, ty, default, optional, private, presence, hidden, cell, weak }
    FieldInit { field, depth, at, init }
    EnumDef { name, generics, variants, is_copy, is_union, span }
    VariantDef { name, payload, discriminant, str_value }
    GlobalDef { name, ty, init, span, is_slot, c_const, c_global }
    CConst { ty, value }
    CGlobal { library, symbol, ty, writable }
    InterfaceDef { name, generics, fields, methods, span }
    InterfaceMethodDef { name, default, promise, throws }
    ImplDef { ty, generics, iface, iface_args, methods }
    // Bodies.
    Block { stmts, value, span }
    Stmt { kind, span }
    Expr { kind, ty, span }
    Arm { pat, guard, body }
    Pat { kind, ty, span }
    ConstArray { elem, values }
    ConstElem { bits, float, lo, hi }
    // The program's tables.
    FnShape { name, length, wraps }
    DynMember { name, kind, def, owner }
    ImplBound { param, iface, args }
    ModuleState { ty, slots, deferred }
    Decorator { def, args, span }
    CExport { def, name, params, ret, span }
}

enums! {
    @fielded Def {
        0 => Fn(a),
        1 => ExternFn(a),
        2 => Adt(a),
        3 => Enum(a),
        4 => Global(a),
        5 => Interface(a),
    }
    AdtKind {
        0 => Struct,
        1 => Class,
        2 => Anon,
    }
    PassMode {
        0 => Copy,
        1 => Borrow,
        2 => BorrowMut,
        3 => Owned,
    }
    UseMode {
        0 => Copy,
        1 => Move,
        2 => Borrow,
        3 => BorrowMut,
    }
    Conversion {
        0 => No,
        1 => Copy,
        2 => ProxyOnly,
    }
    @fielded CTy {
        0 => Int(a),
        1 => F32,
        2 => F64,
        3 => Bool,
        4 => Ptr { nullable },
        5 => Null,
        6 => StrBytes,
        7 => Buf { readonly },
        8 => CString,
        9 => Out,
        // 10 was `OutPtr` (#3094): not reused.
        11 => Void,
    }
    @fielded HostTy {
        0 => Int(a),
        1 => F64,
        2 => Bool,
        3 => Str,
        4 => Bytes { readonly },
        5 => Ref { nullable },
        6 => Void,
    }
    @fielded CValue {
        0 => Int(a),
        1 => Float(a),
        2 => Bool(a),
        3 => Address(a),
    }
    @fielded StmtKind {
        0 => Let { local, init },
        1 => LetPat { pat, init },
        2 => Expr(a),
        3 => Return(a),
        4 => If { cond, then, els },
        5 => While { label, cond, body, step },
        6 => ForOf { label, binding, iter, body, consume, share_elems },
        7 => Try { body, catch, finally },
        8 => Break(a),
        9 => Continue(a),
        10 => Block(a),
    }
    @fielded ExprKind {
        0 => Lit(a),
        1 => Local(a, b),
        2 => Unary { op, expr },
        3 => Binary { op, lhs, rhs },
        4 => Logical { op, lhs, rhs },
        5 => Assign { place, value },
        6 => CompoundAssign { op, place, value },
        7 => Call { callee, args },
        8 => Cast(a),
        9 => If { cond, then, els },
        10 => Block(a),
        11 => Global(a),
        12 => FnRef(a, b),
        13 => Field { base, index, mode },
        14 => Index { base, index, mode },
        15 => AdtLit { def, type_args, fields, converted },
        16 => Variant { def, type_args, variant, args },
        17 => ArrayLit(a),
        18 => ConstArray(a),
        19 => Tuple(a),
        20 => Closure(a),
        21 => Match { scrutinee, arms },
        22 => Await(a),
        23 => WrapSome(a),
        24 => UnwrapSome(a, b),
        25 => UnwrapVariant { expr, variant, mode },
        26 => New { def, type_args, args },
        27 => Upcast(a),
        28 => Downcast(a),
        29 => ToDyn { expr, impl_index },
        30 => Throw(a),
    }
    @fielded Callee {
        0 => Def(a, b),
        1 => Indirect(a),
        2 => Virtual { slot },
        3 => Dyn { slot, method_type_args },
        4 => ParamMethod { iface, iface_args, slot, method_type_args },
        5 => Intrinsic(a),
    }
    PatKind {
        0 => Wildcard,
        1 => Binding(a, b),
        2 => Lit(a),
        3 => Variant { def, variant, args },
        4 => Adt { fields },
        5 => Tuple(a),
        6 => Array { elems, rest },
        7 => Or(a),
        8 => None,
        9 => Some(a),
        10 => InstanceOf(a),
    }
    @fielded Lit {
        0 => Int(a),
        1 => Float(a),
        2 => Str(a),
        3 => Bool(a),
        4 => Unit,
        5 => Null,
    }
    UnOp {
        0 => Neg,
        1 => Not,
        2 => BitNot,
    }
    BinOp {
        0 => Add,
        1 => Sub,
        2 => Mul,
        3 => Div,
        4 => Rem,
        5 => Pow,
        6 => Eq,
        7 => NotEq,
        8 => Lt,
        9 => LtEq,
        10 => Gt,
        11 => GtEq,
        12 => BitAnd,
        13 => BitOr,
        14 => BitXor,
        15 => Shl,
        16 => Shr,
        17 => UShr,
    }
    LogicOp {
        0 => And,
        1 => Or,
    }
    // Types.
    @fielded TyKind {
        0 => Int(a),
        1 => Float(a),
        2 => Bool,
        3 => Str,
        4 => Unit,
        5 => Never,
        6 => Error,
        7 => Adt(a, b),
        8 => Array(a),
        9 => Map(a, b),
        10 => Tuple(a),
        11 => Option(a, b),
        12 => Result(a, b),
        13 => Promise(a, b),
        14 => Shared(a),
        15 => FnPtr { params, ret, throws, rest },
        16 => Closure(a),
        17 => Param(a),
        18 => Dyn(a, b),
        19 => Literal(a),
        20 => Symbol,
        21 => Simd(a),
        22 => Op(a, b),
        23 => Ptr(a),
    }
    @fielded LitValue {
        0 => Str(a),
        1 => Int(a, b),
        2 => Float(a, b),
        3 => Bool(a),
    }
    Nothing {
        0 => Null,
        1 => Undefined,
        2 => Both,
    }
    IntTy {
        0 => I8,
        1 => I16,
        2 => I32,
        3 => I64,
        4 => ISize,
        5 => U8,
        6 => U16,
        7 => U32,
        8 => U64,
        9 => USize,
    }
    FloatTy {
        0 => F32,
        1 => F64,
        2 => F16,
        3 => BF16,
    }
    SimdTy {
        0 => I8x16,
        1 => U8x16,
        2 => I16x8,
        3 => U16x8,
        4 => I32x4,
        5 => U32x4,
        6 => I64x2,
        7 => U64x2,
        8 => F32x4,
        9 => F64x2,
        10 => F16x8,
        11 => BF16x8,
        12 => Mask8x16,
        13 => Mask16x8,
        14 => Mask32x4,
        15 => Mask64x2,
    }
    @fielded TyOp {
        0 => Alias(a),
        1 => Intersect,
        2 => Spread,
        3 => Convert(a),
        4 => Inline(a),
        5 => ErrSite(a),
    }
    // The program's tables.
    CpuTarget {
        0 => ShaArm,
        1 => ShaX86,
        2 => ClmulX86,
        3 => ClmulArm,
        4 => AesX86,
        5 => AesArm,
        6 => FmaX86,
        7 => Bf16Arm,
        8 => Sse3X86,
        9 => Ssse3X86,
        10 => Sse41X86,
        11 => Sse42X86,
        12 => PopcntX86,
        13 => Cmpxchg16bX86,
        14 => AvxX86,
        15 => Avx2X86,
        16 => Bmi1X86,
        17 => Bmi2X86,
        18 => LzcntX86,
        19 => MovbeX86,
        20 => F16cX86,
        21 => Avx512fX86,
        22 => Avx512bwX86,
        23 => Avx512cdX86,
        24 => Avx512dqX86,
        25 => Avx512vlX86,
        26 => Avx512vbmiX86,
        27 => Avx512bitalgX86,
        28 => DotprodArm,
        29 => CrcArm,
        30 => Sha3Arm,
        31 => Fp16Arm,
        32 => I8mmArm,
    }
    DynMemberKind {
        0 => Method,
        1 => Getter,
        2 => Setter,
        3 => ReadonlyField,
        4 => Unsafe,
        5 => UnsafeSetter,
    }
    @fielded FnWrapped {
        0 => Local(a),
        1 => Fn(a, b),
    }
    @fielded FreshClosure {
        0 => New(a),
        1 => Several(a),
        2 => Converts { from, to },
    }
    @fielded DecoratorTarget {
        0 => Def(a),
        1 => Field(a, b),
        2 => Param(a, b),
    }
    ConstTree {
        0 => Null,
        1 => Undefined,
        2 => Bool(a),
        3 => Int(a),
        4 => Float(a),
        5 => Str(a),
        6 => Array(a),
        7 => Object(a),
        8 => EnumMember(a, b),
    }
    // Calls the compiler implements. A tag stays with its intrinsic for good: a new one takes
    // the next free number, wherever it is declared.
    Intrinsic {
        0 => Print,
        1 => PrintErr,
        2 => ToString,
        3 => ErrorText,
        4 => ThrowLoc,
        5 => Inspect,
        6 => StrConcat,
        7 => StrLen,
        8 => StrByteLen,
        9 => Exit,
        10 => Panic,
        11 => ArrayWithCapacity,
        12 => ArrayLen,
        13 => ArrayPush,
        14 => ArrayPop,
        15 => ArraySwap,
        16 => ArrayRemove,
        17 => ArrayTruncate,
        18 => ArrayMove,
        19 => ArrayCopyBytes,
        20 => ArraySetLen,
        21 => ArrayReserve,
        22 => ArrayPrefetch,
        23 => ArrayRotateRight,
        24 => ArrayRotateLeft,
        25 => ArraySwapNested,
        26 => Subarray,
        27 => ParamOrdering,
        28 => Hash,
        29 => KeyEq,
        30 => IncludesEq,
        31 => Same,
        32 => SameValue,
        33 => Clone,
        34 => Share,
        35 => SpreadObject,
        36 => KeyRead,
        37 => KeyWrite,
        38 => FieldAbsent,
        39 => FieldPresent,
        40 => Transfer,
        41 => NeedsTransfer,
        42 => NeedsDrop,
        43 => ModuleStateUsed,
        44 => MayAlias,
        45 => HoldsNull,
        46 => NullAs,
        47 => Truthy,
        48 => IsNull,
        49 => IsNullish,
        50 => LooseEq,
        51 => MayHoldPrimitive,
        52 => HoldsDynamic,
        53 => HoldsDynamicOrClass,
        54 => FnCapturesNothing,
        55 => FnLength,
        56 => FnName,
        57 => ClosureSibling(a),
        58 => InitSlots(a, b),
        59 => Sqrt,
        60 => Floor,
        61 => Ceil,
        62 => Round,
        63 => Trunc,
        64 => FAbs,
        65 => SharedNew,
        66 => Spawn,
        67 => SpawnHandled,
        68 => Sleep,
        69 => YieldNow,
        70 => PromiseAll,
        71 => PromiseRace,
        72 => PromiseAny,
        73 => SourceLocation,
        74 => SymbolStatic,
        75 => SymbolRegistry,
        76 => TemplateToString,
        77 => PromiseWiden,
        78 => PromiseSame,
        79 => PerfNow,
        80 => DateNow,
        81 => SharedAdd,
        82 => SharedGet,
        83 => SharedSet,
        84 => SharedRead,
        85 => MutexNew,
        86 => MutexWith,
        87 => RwLockNew,
        88 => LockRead,
        89 => AtomicLoad,
        90 => AtomicStore,
        91 => AtomicRmw,
        92 => AtomicCas,
        93 => AtomicCasWeak,
        94 => AtomicFence,
        95 => AtomicSpinHint,
        96 => ThreadLocalGet,
        97 => ThreadLocalSet,
        98 => StrCharCodeAt,
        99 => AddressLoad,
        100 => AddressStore,
        106 => AtomicAddress,
        107 => FutPoll,
        108 => FutDrop,
        109 => ExportedAddress,
        110 => CatchUnwind,
        111 => CatchUnwindAt,
        112 => StatePoll,
        113 => StateDrop,
        114 => AddressCall,
        115 => FnAddress,
        116 => SlotSize,
        117 => SlotPut,
        118 => SlotTransfer,
        119 => SlotStore,
        120 => SlotTake,
        121 => PromiseAt,
        122 => RawAlloc,
        123 => RawFree,
        124 => StrByteAt,
        125 => StrLoadU8x16,
        126 => StrLoadPartialU8x16,
        127 => StrAsciiWords,
        128 => StrbufPushAsciiWords,
        129 => StrbufPushAsciiWord,
        130 => ArrayDataPtr,
        131 => StrWord0,
        132 => StrWord1,
        133 => StrWord2,
        134 => StrDataAddress,
        135 => Attempt,
        136 => Yield,
        137 => GeneratorResume,
        138 => GeneratorValue,
        139 => GeneratorReturn,
        140 => GeneratorResumeWith,
        141 => GeneratorThrow,
        142 => YieldDelegate,
        143 => GeneratorEmbed,
        144 => AsyncGeneratorResume,
        145 => AsyncGeneratorValue,
        146 => AsyncGeneratorReturn,
        147 => AsyncGeneratorDispose,
        148 => WeakMapSet,
        149 => WeakMapGet,
        150 => WeakMapHas,
        151 => WeakMapDelete,
        152 => WeakMapValue,
        153 => WeakRefNew,
        154 => WeakRefDeref,
        155 => AnyTo,
        156 => AnyToExact,
        157 => AnyTypeof,
        158 => AnyTruthy,
        159 => AnyUnboundMethod,
        160 => AnyStrictEq,
        161 => AnyLooseEq,
        162 => AnyGet,
        163 => AnyIndex,
        164 => AnyCall,
        165 => AnyApply,
        166 => AnySet,
        167 => AnySetIndex,
        168 => AnyHas,
        169 => AnyDelete,
        170 => AnyMethodCalled,
        171 => AnyIsList,
        172 => NoteStdUse,
        173 => StdUsed,
        174 => ProxyArrayCall,
        175 => ProxyArrayHole,
        176 => IsTypedProxy,
        177 => Identity,
        178 => ToAny,
        179 => AnyReflectInfo,
        180 => AnyReflectCount,
        181 => AnyReflectName,
        182 => AnyReflectValue,
        183 => AnyReflectHook,
        184 => AnyIsArray,
        185 => AnyKeys,
        186 => AnyTag,
        187 => AnyToNumber,
        188 => SimdShuffle(a, b),
        189 => InspectNested,
        190 => AddressAtomicLoad,
        191 => AddressAtomicStore,
        192 => AddressAtomicRmw,
        193 => AddressAtomicCas,
        194 => AddressAtomicCasWeak,
        195 => RecordViewSync,
        196 => RecordViewIs,
        197 => RecordViewGet,
        198 => RecordViewHas,
        199 => RecordViewSet,
        200 => RecordViewDelete,
        201 => RecordViewFlush,
        202 => AddressArray,
        203 => AddressCallWord,
        204 => AddressCall3,
        205 => WeakMapDrop,
        206 => WeakRefDrop,
        207 => ArrayDataAddress,
        208 => ArrayReadAddress,
        209 => ArrayBytesWritten,
        210 => ArrayByteLength,
        211 => SymbolAddress,
        212 => SymbolAt,
        213 => AddressArrayOwned,
        214 => StrAt,
        215 => SpreadParam,
        218 => PtrFromAddress,
        219 => PtrToAddress,
        220 => IsUnique,
        221 => PtrRead,
        222 => PtrWrite,
        225 => Freeze,
        226 => IsFrozen,
    }
}

/// A set of CPU features (velt_common's, whose bits are private) as its features, each by its
/// `CpuTarget` tag: the encoding does not depend on the order of the bits.
impl super::Codec for CpuTargets {
    fn enc(&self, w: &mut super::Writer) {
        super::Codec::enc(&self.iter().collect::<Vec<CpuTarget>>(), w);
    }
    fn dec(r: &mut super::Reader) -> Self {
        let features: Vec<CpuTarget> = super::Codec::dec(r);
        (features.into_iter()).fold(CpuTargets::NONE, CpuTargets::with)
    }
}
