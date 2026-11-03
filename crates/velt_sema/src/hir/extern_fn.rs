//! `declare function` definitions: runtime and native-library functions, system functions and
//! host functions (part of the HIR contract).

use velt_common::Span;

use super::{IntTy, TyId};

#[derive(Clone, Debug)]
pub struct ExternFnDef {
    pub name: String,
    /// Link symbol name (same as `name` without module path). For a host function, its name in
    /// the host's import module.
    pub symbol: String,
    pub params: Vec<TyId>,
    pub ret: TyId,
    /// M3: `declare async function f(...): Promise<T>` — the symbol returns an rt leaf future
    /// (`VeltFut*`, result slot at offset 16; see rt_abi_async.md); `ret` is `Promise<T>`.
    pub is_async: bool,
    /// `declare system function` (https://veltlang.com/docs/reference/system-functions/): per parameter, whether
    /// its type was written `readonly` (`readonly u8[]`, a buffer the C function only reads;
    /// `params` has `u8[]` either way). `None` for runtime and native-package functions.
    pub system: Option<Vec<bool>>,
    /// `declare host("<module>") function` (https://veltlang.com/docs/reference/host-functions/): a function the
    /// JS host provides, the WebAssembly import `module`.`symbol`, and what crosses for each
    /// parameter and the result. Never with `system`. With `is_async`, `ret` is
    /// `Promise<T, IoError>` and `HostImport::ret` what `T` crosses as. Additive.
    pub host: Option<HostImport>,
    /// `declare c("<library>") function` (docs/internals/design/c-ffi.md): a function of a native
    /// library, called through the C ABI, and what crosses. Never with `system` or `host`.
    /// Additive.
    pub c: Option<CImport>,
    pub span: Span,
}

/// A C function's library and what crosses (docs/internals/design/c-ffi.md "What crosses").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CImport {
    /// The logical library name of `declare c("<library>")`; `"c"` is the platform's C library.
    pub library: String,
    /// One per parameter of the declaration, in order.
    pub params: Vec<CTy>,
    pub ret: CTy,
}

/// A type that crosses to C.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CTy {
    Int(IntTy),
    F32,
    F64,
    /// C's `_Bool` (one byte, 0 or 1).
    Bool,
    /// std's `Ptr<T>` (velt/ffi), or `Ptr<T> | null` (`nullable`): an address; `NULL` is `null`.
    Ptr {
        nullable: bool,
    },
    /// The type `null` (parameters only): always `NULL`.
    Null,
    /// `StrBytes` (velt/ffi; parameters only): a `string`'s UTF-8 bytes, borrowed for the call,
    /// without a NUL; one that may hold a lone surrogate is passed as a copy with U+FFFD.
    StrBytes,
    /// An array of numbers (`u8[]`, `readonly f64[]`; parameters only): the address of its first
    /// element, borrowed for the call. `readonly`: C only reads it. A `u8[]` parameter also
    /// takes a view of bytes (a typed array, a `DataView`, a `subarray`).
    Buf {
        readonly: bool,
    },
    /// `void` (results only).
    Void,
}

/// Where a host function comes from and what crosses (https://veltlang.com/docs/reference/host-functions/).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostImport {
    /// The import module: `"cloudflare"` of `declare host("cloudflare") function`.
    pub module: String,
    /// One per parameter of the declaration, in order.
    pub params: Vec<HostTy>,
    /// The result; of an async host function, what its promise fulfills with.
    pub ret: HostTy,
}

/// A type that crosses between Velt and the JS host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostTy {
    Int(IntTy),
    F64,
    Bool,
    /// `string`: UTF-8 bytes the glue decodes (a parameter) or encodes (the result).
    Str,
    /// `u8[]` (`readonly: false`, bytes the host may write) or `readonly u8[]`; a parameter
    /// also takes a view of bytes at the call (https://veltlang.com/docs/reference/system-functions/ "Views").
    Bytes {
        readonly: bool,
    },
    /// std's `HostRef` (velt/host), or `HostRef | null` (`nullable`): a handle into the glue's
    /// table of JS values.
    Ref {
        nullable: bool,
    },
    /// `void` (results only).
    Void,
}
