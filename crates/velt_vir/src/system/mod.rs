//! The table of system functions (docs/reference/system-functions.md): the functions of the
//! platform's C library that `declare system function` may bind, per target, with their C
//! signatures. Velt code reaches the OS through them without a runtime in between, and stays
//! memory-safe because the compiler, not the declaration, decides what crosses:
//!
//! - integers cross as themselves ([`Int`]: the declaration names the same type);
//! - a byte buffer is one Velt parameter, `u8[]` (the C function writes it) or `readonly u8[]`
//!   (it only reads it), and two C arguments, the pointer and the length, both taken from the
//!   array ([`Param::Buf`]), so no call reaches past its end; a declaration may follow it with
//!   `start: usize, end: usize` to pass the bytes `start..end` (checked: [`ranged`]);
//! - a path is a `readonly u8[]` (`u16[]` on Windows) whose last element must be 0
//!   ([`Param::CStr`], [`Param::WStr`]), checked at the call;
//! - a C struct is a Velt `struct` of integers whose fields name the C struct's fields
//!   ([`Struct`]): lowering copies it into a temporary of the C layout the table states for the
//!   target, passes its address and copies the fields back after the call;
//! - results are integers; `lastOsError()` reads `errno` (`GetLastError()`) ([`Ret::Errno`]).
//!
//! No pointers, callbacks or functions that keep a buffer after they return are in the table,
//! and arguments that would make a C function use memory differently are checked at the call
//! ([`Guard`]: `fcntl`'s commands, `CreateFileW`'s flags).
//!
//! A declaration is checked against the table of the target being compiled for (veltc's
//! `native::check_declares`); one whose function another target has but this one lacks is
//! accepted, and a call of it in the program is an error then ([`available`]), so portable
//! code can branch on the platform.

mod check;
mod posix;
mod windows;

pub use check::{expected_decls, match_decl, ranged, Mismatch};

/// An integer type as a declaration names it (`i32`, `isize`, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Int {
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    ISize,
    USize,
}

impl Int {
    /// The Velt type's name.
    pub fn velt(self) -> &'static str {
        match self {
            Int::I16 => "i16",
            Int::U16 => "u16",
            Int::I32 => "i32",
            Int::U32 => "u32",
            Int::I64 => "i64",
            Int::U64 => "u64",
            Int::ISize => "isize",
            Int::USize => "usize",
        }
    }

    /// The VIR type (targets with system functions are 64-bit).
    pub fn vir(self) -> crate::vir::Ty {
        use crate::vir::Ty;
        match self {
            Int::I16 => Ty::I16,
            Int::U16 => Ty::U16,
            Int::I32 => Ty::I32,
            Int::U32 => Ty::U32,
            Int::I64 | Int::ISize => Ty::I64,
            Int::U64 | Int::USize => Ty::U64,
        }
    }
}

/// What a call checks about an integer argument before it calls the C function; a value that
/// fails stops the program (a panic naming the function and the value).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guard {
    None,
    /// The value is one of these (`fcntl`'s commands that take this kind of argument).
    OneOf(&'static [i64]),
    /// None of these bits is set (`FILE_FLAG_OVERLAPPED`: the kernel would write the buffer
    /// after the call returned).
    NoBits(i64),
}

/// One parameter of a C function, as the table states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Param {
    /// An integer: `c` is its C type, for messages.
    Int {
        name: &'static str,
        c: &'static str,
        ty: Int,
        guard: Guard,
    },
    /// A byte buffer: in C a pointer and the length after it (of type `len`, clamped to its
    /// largest value); in Velt one `u8[]` (`written`) or `readonly u8[]`.
    Buf {
        name: &'static str,
        written: bool,
        len: Int,
    },
    /// `const char *`: a `readonly u8[]` whose last byte is 0.
    CStr { name: &'static str },
    /// `LPCWSTR`: a `readonly u16[]` whose last element is 0.
    WStr { name: &'static str },
    /// A pointer to a C struct, from a Velt `struct` (read and written back).
    Struct {
        name: &'static str,
        layout: &'static Struct,
    },
    /// A null pointer: a C parameter the declaration leaves out.
    Null { name: &'static str, c: &'static str },
}

impl Param {
    /// Is it a parameter of the Velt declaration?
    pub fn declared(&self) -> bool {
        !matches!(self, Param::Null { .. })
    }
}

/// A C struct's layout on one target: its fields by the names Velt structs use.
#[derive(Debug, PartialEq, Eq)]
pub struct Struct {
    /// `struct flock`, for messages.
    pub c: &'static str,
    pub size: u32,
    pub align: u32,
    pub fields: &'static [Field],
}

/// A field of a C struct: the Velt type a struct names it with, its C type (which may be
/// narrower: lowering converts) and its offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: &'static str,
    pub ty: Int,
    pub c: Int,
    pub offset: u32,
}

/// A result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ret {
    Int { c: &'static str, ty: Int },
    /// `lastOsError()`: the symbol returns `int *` (`__error`, `__errno_location`), and the
    /// result is the `int` there.
    Errno,
}

/// What a call does after the C function returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    None,
    /// Windows: a result of 0 with `ERROR_IO_PENDING` waits for the operation
    /// (`GetOverlappedResult(handle, overlapped, &n, TRUE)`), so the kernel is done with the
    /// buffer and the `OVERLAPPED` before the call returns. The indices are of `params`.
    WaitOverlapped { handle: usize, overlapped: usize },
}

/// A function of the table.
#[derive(Debug)]
pub struct Func {
    /// The name a declaration uses (`fcntl`; a declaration may add `$suffix`).
    pub name: &'static str,
    /// The symbol it links to on this target (`fstat$INODE64` on x86-64 macOS).
    pub symbol: &'static str,
    pub params: &'static [Param],
    /// Index into `params` of the first variadic one (`params.len()`: none).
    pub variadic: usize,
    pub ret: Ret,
    pub after: After,
}

impl Func {
    /// The C prototype, for messages: `ssize_t pread(int fd, void *buf, size_t nbyte, off_t
    /// offset)`.
    pub fn prototype(&self) -> String {
        let mut ps = vec![];
        for (i, p) in self.params.iter().enumerate() {
            if i == self.variadic {
                ps.push("...".to_string());
            }
            ps.push(match p {
                Param::Int { name, c, .. } => format!("{c} {name}"),
                Param::Buf { name, written, len } => {
                    let cst = if *written { "" } else { "const " };
                    let n = if *len == Int::U32 { "DWORD" } else { "size_t" };
                    format!("{cst}void *{name}, {n} len")
                }
                Param::CStr { name } => format!("const char *{name}"),
                Param::WStr { name } => format!("LPCWSTR {name}"),
                Param::Struct { name, layout } => format!("{} *{name}", layout.c),
                Param::Null { name, c } => format!("{c} {name} = NULL"),
            });
        }
        let ret = match self.ret {
            Ret::Int { c, .. } => c,
            Ret::Errno => "int",
        };
        let name = match self.ret {
            Ret::Errno => "errno",
            _ => self.symbol,
        };
        match self.ret {
            Ret::Errno => format!("{ret} {name}"),
            _ => format!("{ret} {name}({})", ps.join(", ")),
        }
    }
}

/// The operating systems with a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
    Windows,
}

/// The table's view of a target triple: `None` for targets without system functions
/// (WebAssembly, and anything not 64-bit).
pub fn target_os(triple: &str) -> Option<(Os, bool)> {
    let arm = triple.starts_with("aarch64") || triple.starts_with("arm64");
    let x86 = triple.starts_with("x86_64");
    if !(arm || x86) {
        return None;
    }
    let os = if triple.contains("-apple-") {
        Os::MacOs
    } else if triple.contains("-linux") {
        Os::Linux
    } else if triple.contains("-windows") {
        Os::Windows
    } else {
        return None;
    };
    Some((os, arm))
}

/// The triple of the machine the compiler runs on, as the table reads it (lowering without a
/// target: tests and the JIT).
pub fn host_triple() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "windows" => format!("{arch}-pc-windows-msvc"),
        "macos" => format!("{arch}-apple-darwin"),
        "linux" => format!("{arch}-unknown-linux-gnu"),
        os => format!("{arch}-unknown-{os}"),
    }
}

/// The table of a target triple (empty for targets without system functions).
pub fn table(triple: &str) -> &'static [Func] {
    match target_os(triple) {
        Some((Os::MacOs, arm)) => posix::macos(arm),
        Some((Os::Linux, arm)) => posix::linux(arm),
        Some((Os::Windows, _)) => windows::table(),
        None => &[],
    }
}

/// Every target with a table, for "which targets have this function" and checks.
pub const TARGETS: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
];

/// The name a declaration looks up: its own without a `$suffix` (`fcntl$lock` is `fcntl`).
pub fn base_name(declared: &str) -> &str {
    declared.split('$').next().unwrap_or(declared)
}

/// The functions of the table of `triple` that a declaration named `declared` may bind.
pub fn overloads<'a>(triple: &str, declared: &'a str) -> impl Iterator<Item = &'static Func> + 'a {
    let base = base_name(declared);
    table(triple).iter().filter(move |f| f.name == base)
}

/// Is a function named `declared` in the table of `triple`?
pub fn available(triple: &str, declared: &str) -> bool {
    overloads(triple, declared).next().is_some()
}

/// The names of every target's functions (for "did you mean").
pub fn all_names() -> Vec<&'static str> {
    let mut names: Vec<&str> = TARGETS
        .iter()
        .flat_map(|t| table(t).iter().map(|f| f.name))
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

#[cfg(test)]
mod tests;
