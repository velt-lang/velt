//! velt/arch's instructions (#3222 step 4, docs/internals/design/package-machine-code.md §3): one
//! table ([`table::ROWS`]), a row per instruction, from which everything else follows.
//!
//! - std's declarations: each row is a static method of its feature's class in velt/arch/x86 or
//!   velt/arch/arm (`Sse42.crc32U64`), whose body calls the row's extern (`velt_arch_<class>_
//!   <method>`, [`ArchOp::symbol`]). The methods and the externs are generated from the table
//!   ([`render`]); `cargo xtask gen-arch` rewrites them, and a test fails when they are stale.
//! - sema: a velt/arch method other than `isSupported` is gated on its class's feature, so a
//!   call needs the check or a gated caller (velt_sema `body/gated.rs`).
//! - VIR: the extern is [`crate::simd::SimdCall::Arch`]; the verifier rejects one in a function
//!   whose target does not cover the row's feature.
//! - LLVM lowers each row as its [`Llvm`] says. Cranelift never runs gated code (it answers every
//!   check false), so it only gives a zero.

mod render;
mod table;

pub use render::{c_index, class_tail, vector_file, CLASS_MARKER};
pub use table::ROWS;

use crate::simd::Ext;
use crate::vir::Ty;

/// A value of an instruction's signature: a scalar, one of velt/simd's 128-bit lane types, or
/// velt/arch/x86's `Vector256<T>` / `Vector512<T>` (#3222 step 5) of lanes `T`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Val {
    U8,
    U16,
    U32,
    U64,
    U8x16,
    I8x16,
    U32x4,
    I32x4,
    U64x2,
    /// `Vector256<T>`: two 128-bit halves, taken by address.
    V256(Lane),
    /// `Vector512<T>`: four 128-bit quarters, taken by address.
    V512(Lane),
}

/// The lane type of a `Vector256` or `Vector512`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
}

impl Lane {
    /// Every lane type, in the order of the generated API.
    pub const ALL: [Lane; 10] = [
        Lane::U8,
        Lane::I8,
        Lane::U16,
        Lane::I16,
        Lane::U32,
        Lane::I32,
        Lane::U64,
        Lane::I64,
        Lane::F32,
        Lane::F64,
    ];

    /// The Velt scalar type (`u32`).
    pub fn scalar(self) -> &'static str {
        match self {
            Lane::U8 => "u8",
            Lane::I8 => "i8",
            Lane::U16 => "u16",
            Lane::I16 => "i16",
            Lane::U32 => "u32",
            Lane::I32 => "i32",
            Lane::U64 => "u64",
            Lane::I64 => "i64",
            Lane::F32 => "f32",
            Lane::F64 => "f64",
        }
    }

    /// Its size in bytes.
    pub fn bytes(self) -> u32 {
        match self {
            Lane::U8 | Lane::I8 => 1,
            Lane::U16 | Lane::I16 => 2,
            Lane::U32 | Lane::I32 | Lane::F32 => 4,
            Lane::U64 | Lane::I64 | Lane::F64 => 8,
        }
    }

    /// The velt/simd type of 128 bits of these lanes (`u32x4`).
    pub fn narrow(self) -> String {
        format!("{}x{}", self.scalar(), 16 / self.bytes())
    }

    /// The LLVM type of one lane.
    pub fn llvm(self) -> &'static str {
        match self {
            Lane::U8 | Lane::I8 => "i8",
            Lane::U16 | Lane::I16 => "i16",
            Lane::U32 | Lane::I32 => "i32",
            Lane::U64 | Lane::I64 => "i64",
            Lane::F32 => "float",
            Lane::F64 => "double",
        }
    }

    /// The suffix of a name of the generated API (`U32`).
    pub fn suffix(self) -> String {
        self.scalar().to_uppercase()
    }
}

impl Val {
    /// The Velt type.
    pub fn velt(self) -> String {
        match self {
            Val::U8 => "u8".into(),
            Val::U16 => "u16".into(),
            Val::U32 => "u32".into(),
            Val::U64 => "u64".into(),
            Val::U8x16 => "u8x16".into(),
            Val::I8x16 => "i8x16".into(),
            Val::U32x4 => "u32x4".into(),
            Val::I32x4 => "i32x4".into(),
            Val::U64x2 => "u64x2".into(),
            Val::V256(l) => format!("Vector256<{}>", l.scalar()),
            Val::V512(l) => format!("Vector512<{}>", l.scalar()),
        }
    }

    /// The VIR type it is passed as: a wide vector by address (a result through a last `Ptr`
    /// parameter, [`ArchOp::signature`]).
    pub fn ty(self) -> Ty {
        match self {
            Val::U8 => Ty::U8,
            Val::U16 => Ty::U16,
            Val::U32 => Ty::U32,
            Val::U64 => Ty::U64,
            Val::V256(_) | Val::V512(_) => Ty::Ptr,
            _ => Ty::V128,
        }
    }

    /// Its LLVM type: a vector's lanes as the row's operation sees them.
    pub fn llvm(self) -> String {
        match self {
            Val::U8 => "i8".into(),
            Val::U16 => "i16".into(),
            Val::U32 => "i32".into(),
            Val::U64 => "i64".into(),
            Val::U8x16 | Val::I8x16 => "<16 x i8>".into(),
            Val::U32x4 | Val::I32x4 => "<4 x i32>".into(),
            Val::U64x2 => "<2 x i64>".into(),
            Val::V256(l) => format!("<{} x {}>", 32 / l.bytes(), l.llvm()),
            Val::V512(l) => format!("<{} x {}>", 64 / l.bytes(), l.llvm()),
        }
    }

    /// Whether it is a 128-bit vector (a `V128`).
    pub fn is_vector(self) -> bool {
        self.ty() == Ty::V128
    }

    /// Its width in bits where it is a 256- or 512-bit vector.
    pub fn wide(self) -> Option<u32> {
        match self {
            Val::V256(_) => Some(256),
            Val::V512(_) => Some(512),
            _ => None,
        }
    }
}

/// How LLVM computes a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Llvm {
    /// A call of this LLVM intrinsic with the arguments as they are (in their [`Val::llvm`]
    /// types), returning the result's type.
    Call(&'static str),
    /// Instructions on `%x0`, `%x1`, … (the arguments in their [`Val::llvm`] types) whose last
    /// value is `%r` (of the result's type), with the declarations they need.
    Ir {
        decls: &'static [&'static str],
        body: &'static str,
    },
}

/// One instruction: a static method of its feature's class.
#[derive(Clone, Copy, Debug)]
pub struct Row {
    /// The feature whose class has it, which a call needs.
    pub ext: Ext,
    /// The method's name (`crc32U64`).
    pub method: &'static str,
    /// The C intrinsic (Intel's guide, Arm's ACLE): `_mm_crc32_u64`.
    pub c: &'static str,
    /// The instruction's mnemonic as the assembler prints it for this row (`crc32q`), which the
    /// LLVM tests look for: `*` stands for any text (`vfmadd*ps`: LLVM picks the operand order),
    /// and `|` separates the equivalent forms LLVM may choose (`vpermd|vpermps`).
    pub instr: &'static str,
    /// The parameters' names and types.
    pub params: &'static [(&'static str, Val)],
    pub ret: Val,
    pub llvm: Llvm,
    /// The method's doc comment: one sentence, what it computes.
    pub doc: &'static str,
}

/// An instruction of [`ROWS`], by its index there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ArchOp(pub u16);

impl ArchOp {
    /// The instruction `symbol` (`velt_arch_sse42_crc32_u64`) names, if it is one.
    pub fn of_symbol(symbol: &str) -> Option<ArchOp> {
        symbol.strip_prefix("velt_arch_")?;
        // Every row's symbol, made once: the verifier and both backends ask for each extern.
        static BY_SYMBOL: std::sync::LazyLock<std::collections::HashMap<String, ArchOp>> =
            std::sync::LazyLock::new(|| {
                (ROWS.iter().enumerate())
                    .map(|(i, r)| (symbol_of(r), ArchOp(i as u16)))
                    .collect()
            });
        BY_SYMBOL.get(symbol).copied()
    }

    /// Its row.
    pub fn row(self) -> &'static Row {
        &ROWS[self.0 as usize]
    }

    /// The feature a call needs.
    pub fn ext(self) -> Ext {
        self.row().ext
    }

    /// Its extern's name.
    pub fn symbol(self) -> String {
        symbol_of(self.row())
    }

    /// Its extern's VIR signature: a wide result is written through one more `Ptr`, the last
    /// parameter, as a std function's struct result is.
    pub fn signature(self) -> (Vec<Ty>, Ty) {
        let r = self.row();
        let mut params: Vec<Ty> = r.params.iter().map(|(_, v)| v.ty()).collect();
        if r.ret.wide().is_some() {
            params.push(Ty::Ptr);
            return (params, Ty::Unit);
        }
        (params, r.ret.ty())
    }

    /// The widest vector of its signature, in bits (128 for one without a wide vector).
    pub fn width(self) -> u32 {
        let r = self.row();
        (r.params.iter().map(|(_, v)| *v).chain([r.ret]))
            .filter_map(Val::wide)
            .max()
            .unwrap_or(128)
    }
}

/// `velt_arch_<class>_<method in snake case>`: `velt_arch_sse42_crc32_u64`.
fn symbol_of(r: &Row) -> String {
    let mut s = format!("velt_arch_{}_", r.ext.arch_class().to_lowercase());
    for c in r.method.chars() {
        if c.is_ascii_uppercase() {
            s.push('_');
            s.push(c.to_ascii_lowercase());
        } else {
            s.push(c);
        }
    }
    s
}

#[cfg(test)]
mod tests;
