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
    /// parameter and the result. Never with `system` or `is_async`. Additive.
    pub host: Option<HostImport>,
    pub span: Span,
}

/// Where a host function comes from and what crosses (https://veltlang.com/docs/reference/host-functions/).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostImport {
    /// The import module: `"cloudflare"` of `declare host("cloudflare") function`.
    pub module: String,
    /// One per parameter of the declaration, in order.
    pub params: Vec<HostTy>,
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
