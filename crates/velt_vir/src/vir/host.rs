//! Host functions in VIR (https://veltlang.com/docs/reference/host-functions/): an `ExternFn` with `host` set is
//! the WebAssembly import `module`.`name`, which the JS glue (crates/velt_rt_wasm/js/
//! velt_web.mjs) provides by wrapping the embedder's JS function. The glue converts each value by
//! what the program's manifest says crosses ([`HostImport::manifest_line`]; the LLVM backend
//! writes the lines into the custom section [`MANIFEST_SECTION`]).
//!
//! The extern's VIR signature, per value (lowering builds it):
//! - an integer, `F64`, `Bool`: the scalar (`I64`/`U64` reach JS as a `BigInt`);
//! - [`HostValue::Str`]: a `Ptr` to the `VeltStr`, which the glue reads through the runtime's
//!   exports `velt_host_str_ptr` / `velt_host_str_len`;
//! - [`HostValue::Bytes`] / [`HostValue::ReadonlyBytes`]: the `Ptr` to the bytes and their length
//!   as a `U32`;
//! - [`HostValue::Ref`] / [`HostValue::NullableRef`]: the `U32` slot of the glue's table (0 for
//!   `null`);
//! - a `Str` or `Bytes` result: a trailing `Ptr` out-parameter and a `Unit` return; the glue
//!   fills it through `velt_host_alloc` and `velt_host_str_new` / `velt_host_bytes_new`.
//!
//! The glue never wraps a result: one that does not fit the declared type (an integer out of
//! range or not an integer, a value of another type) is a `TypeError`, which stops the program.

/// The name of the custom section holding the program's host imports, one
/// [`HostImport::manifest_line`] each (in any order, possibly repeated).
pub const MANIFEST_SECTION: &str = "velt.host";

/// What a host function is: the import, and what crosses for each parameter and the result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostImport {
    pub module: String,
    pub name: String,
    pub params: Vec<HostValue>,
    pub ret: HostValue,
}

/// A value crossing to or from the host, as the glue converts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostValue {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F64,
    Bool,
    Str,
    /// `u8[]`: a view of the bytes, which the host may write during the call.
    Bytes,
    /// `readonly u8[]` (or a view of one): the host gets a copy.
    ReadonlyBytes,
    Ref,
    NullableRef,
    Void,
}

impl HostValue {
    /// The name the glue knows the value by.
    pub fn manifest_name(self) -> &'static str {
        match self {
            HostValue::I8 => "i8",
            HostValue::I16 => "i16",
            HostValue::I32 => "i32",
            HostValue::I64 => "i64",
            HostValue::U8 => "u8",
            HostValue::U16 => "u16",
            HostValue::U32 => "u32",
            HostValue::U64 => "u64",
            HostValue::F64 => "f64",
            HostValue::Bool => "bool",
            HostValue::Str => "str",
            HostValue::Bytes => "bytes",
            HostValue::ReadonlyBytes => "rbytes",
            HostValue::Ref => "ref",
            HostValue::NullableRef => "ref?",
            HostValue::Void => "void",
        }
    }
}

impl HostImport {
    /// The import's line of the manifest: module, name, the parameters (comma-separated) and the
    /// result, separated by tabs, ending in a line feed. Neither a module nor a name holds a tab
    /// or a line feed (sema rejects such a module).
    pub fn manifest_line(&self) -> String {
        let params: Vec<&str> = self.params.iter().map(|p| p.manifest_name()).collect();
        format!(
            "{}\t{}\t{}\t{}\n",
            self.module,
            self.name,
            params.join(","),
            self.ret.manifest_name()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_lines() {
        let h = HostImport {
            module: "env".into(),
            name: "greet".into(),
            params: vec![
                HostValue::Str,
                HostValue::ReadonlyBytes,
                HostValue::NullableRef,
            ],
            ret: HostValue::Str,
        };
        assert_eq!(h.manifest_line(), "env\tgreet\tstr,rbytes,ref?\tstr\n");
        let none = HostImport {
            params: vec![],
            ret: HostValue::Void,
            ..h
        };
        assert_eq!(none.manifest_line(), "env\tgreet\t\tvoid\n");
    }
}
