//! Whether what a `declare c` parameter or result passes matches the C type the header gives it
//! (docs/internals/design/c-ffi.md "Checking declarations against C headers").

use super::ctype::CType;

/// A number as it crosses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scalar {
    Int {
        bits: u32,
        signed: bool,
        /// `isize` / `usize`: only C's pointer-sized typedefs match.
        pointer_sized: bool,
    },
    F32,
    F64,
    Bool,
}

/// What a `Ptr<T>` points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pointee {
    Scalar(Scalar),
    Void,
    /// A `declare c type`, by name.
    CType(String),
    Ptr(Box<Pointee>),
}

/// What a parameter or result of a `declare c` function passes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shape {
    Scalar(Scalar),
    Ptr(Pointee),
    /// The type `null`: any pointer.
    Null,
    /// `StrBytes` or `CString`: a `const char *`.
    Text,
    /// An array of numbers: a pointer to them; `readonly` when C only reads it.
    Buf {
        elem: Scalar,
        readonly: bool,
    },
    /// `Out<T>`: a `T *` C writes.
    Out(Scalar),
    /// `OutPtr<T>`: a `T **` C writes.
    OutPtr(Pointee),
    Void,
}

/// The Velt type of `shape`, for messages.
pub fn describe(shape: &Shape) -> String {
    fn scalar(s: &Scalar) -> String {
        match s {
            Scalar::Int {
                signed,
                pointer_sized: true,
                ..
            } => if *signed { "isize" } else { "usize" }.into(),
            Scalar::Int { bits, signed, .. } => {
                format!("{}{bits}", if *signed { "i" } else { "u" })
            }
            Scalar::F32 => "f32".into(),
            Scalar::F64 => "f64".into(),
            Scalar::Bool => "bool".into(),
        }
    }
    fn pointee(p: &Pointee) -> String {
        match p {
            Pointee::Scalar(s) => scalar(s),
            Pointee::Void => "void".into(),
            Pointee::CType(n) => n.clone(),
            Pointee::Ptr(inner) => format!("Ptr<{}>", pointee(inner)),
        }
    }
    match shape {
        Shape::Scalar(s) => scalar(s),
        Shape::Ptr(p) => format!("Ptr<{}>", pointee(p)),
        Shape::Null => "null".into(),
        Shape::Text => "StrBytes` or `CString".into(),
        Shape::Buf { elem, readonly } => {
            format!(
                "{}{}[]",
                if *readonly { "readonly " } else { "" },
                scalar(elem)
            )
        }
        Shape::Out(s) => format!("Out<{}>", scalar(s)),
        Shape::OutPtr(p) => format!("OutPtr<{}>", pointee(p)),
        Shape::Void => "void".into(),
    }
}

/// C's typedefs of pointer-sized integers, which `isize` and `usize` require.
fn is_pointer_sized(names: &[String]) -> bool {
    names.iter().any(|n| {
        matches!(
            n.trim_start_matches('_'),
            "size_t" | "ssize_t" | "ptrdiff_t" | "intptr_t" | "uintptr_t" | "SIZE_TYPE__"
        )
    })
}

/// `Err(note)` when `shape` doesn't match `c`; the note says what to declare.
pub fn compare(shape: &Shape, c: &CType) -> Result<(), String> {
    match shape {
        Shape::Void => match c {
            CType::Void => Ok(()),
            _ => Err(format!("declare the result as {}", velt_for(c))),
        },
        Shape::Scalar(s) => scalar(s, c),
        Shape::Null => match c {
            CType::Pointer { .. } | CType::FunctionPointer => Ok(()),
            _ => Err(format!(
                "`null` passes a NULL pointer; declare {}",
                velt_for(c)
            )),
        },
        Shape::Ptr(p) => match c {
            CType::Pointer {
                pointee,
                pointee_names,
                ..
            } => self::pointee(p, pointee, pointee_names),
            _ => Err(format!("declare {}", velt_for(c))),
        },
        Shape::Text => match c {
            CType::Pointer { pointee, .. } if text_bytes(pointee) => Ok(()),
            _ => Err(format!(
                "`StrBytes` and `CString` pass a `const char *`; declare {}",
                velt_for(c)
            )),
        },
        Shape::Buf { elem, readonly } => match c {
            CType::Pointer {
                pointee, is_const, ..
            } => {
                if !*readonly && *is_const {
                    return Err("C only reads the array: declare it `readonly`".into());
                }
                if matches!(**pointee, CType::Void) || scalar(elem, pointee).is_ok() {
                    Ok(())
                } else {
                    Err(format!("the array's elements are {}", velt_for(pointee)))
                }
            }
            _ => Err(format!("declare {}", velt_for(c))),
        },
        Shape::Out(s) => match c {
            CType::Pointer {
                pointee, is_const, ..
            } if !*is_const => scalar(s, pointee).map_err(|e| format!("its `Out<T>`: {e}")),
            CType::Pointer { .. } => Err("C only reads it: it is no out-parameter".into()),
            _ => Err(format!("declare {}", velt_for(c))),
        },
        Shape::OutPtr(p) => match c {
            CType::Pointer {
                pointee, is_const, ..
            } if !*is_const => match &**pointee {
                CType::Pointer {
                    pointee,
                    pointee_names,
                    ..
                } => self::pointee(p, pointee, pointee_names),
                other => Err(format!("C writes {} there, not a pointer", velt_for(other))),
            },
            _ => Err(format!("declare {}", velt_for(c))),
        },
    }
}

fn scalar(s: &Scalar, c: &CType) -> Result<(), String> {
    let ok = match (s, c) {
        (Scalar::F32, CType::Float)
        | (Scalar::F64, CType::Double)
        | (Scalar::Bool, CType::Bool) => true,
        (
            Scalar::Int {
                bits,
                signed,
                pointer_sized: false,
            },
            CType::Enum {
                bits: cbits,
                signed: csigned,
            },
        ) => bits == cbits && csigned.is_none_or(|c| c == *signed),
        (
            Scalar::Int {
                bits,
                signed,
                pointer_sized,
            },
            CType::Int {
                bits: cbits,
                signed: csigned,
                names,
                plain_char,
            },
        ) => {
            bits == cbits
                && (signed == csigned || *plain_char)
                && *pointer_sized == is_pointer_sized(names)
        }
        _ => false,
    };
    match ok {
        true => Ok(()),
        false => Err(format!("declare {}", velt_for(c))),
    }
}

fn pointee(p: &Pointee, c: &CType, names: &[String]) -> Result<(), String> {
    // `void *` takes and gives any pointer, as in C.
    if matches!(p, Pointee::Void) || matches!(c, CType::Void) {
        return Ok(());
    }
    let ok = match (p, c) {
        (Pointee::Scalar(s), _) => scalar(s, c).is_ok(),
        (Pointee::CType(name), CType::Named(tag)) => name == tag || names.iter().any(|n| n == name),
        (Pointee::CType(name), _) => names.iter().any(|n| n == name),
        (
            Pointee::Ptr(inner),
            CType::Pointer {
                pointee,
                pointee_names,
                ..
            },
        ) => self::pointee(inner, pointee, pointee_names).is_ok(),
        _ => false,
    };
    match (ok, c) {
        (true, _) => Ok(()),
        (false, CType::Named(n)) => Err(format!(
            "it points at C type `{n}`: declare `Ptr<{n}>`, with `declare c type {n};`"
        )),
        (false, _) => Err(format!("it points at {}", velt_for(c))),
    }
}

/// Bytes a `const char *` can be: `char`, `signed char`, `unsigned char` or `void`.
fn text_bytes(c: &CType) -> bool {
    matches!(c, CType::Int { bits: 8, .. } | CType::Void)
}

/// What to declare for C type `c`, for messages.
fn velt_for(c: &CType) -> String {
    match c {
        CType::Void => "`void`".into(),
        CType::Bool => "`bool`".into(),
        CType::Float => "`f32`".into(),
        CType::Double => "`f64`".into(),
        CType::Enum { bits, signed } => {
            let sign = if *signed == Some(false) { "u" } else { "i" };
            format!("`{sign}{bits}` (a C enum)")
        }
        CType::Int {
            bits,
            signed,
            names,
            ..
        } => {
            if is_pointer_sized(names) {
                format!("`{}`", if *signed { "isize" } else { "usize" })
            } else {
                format!("`{}{bits}`", if *signed { "i" } else { "u" })
            }
        }
        CType::Pointer { pointee, .. } => match &**pointee {
            CType::Named(n) => format!("a `Ptr<{n}>` (with `declare c type {n}`)"),
            CType::Void => "a `Ptr<void>`".into(),
            _ => "a pointer (`Ptr<T>`, an array, `Out<T>`)".into(),
        },
        CType::Named(n) => format!("struct `{n}` by value, which can't cross yet"),
        CType::FunctionPointer => "a function pointer, which can't cross yet".into(),
        CType::Unknown(n) => format!("`{n}`, which can't cross"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(bits: u32, signed: bool) -> CType {
        CType::Int {
            bits,
            signed,
            names: vec![],
            plain_char: false,
        }
    }

    fn velt_int(bits: u32, signed: bool) -> Shape {
        Shape::Scalar(Scalar::Int {
            bits,
            signed,
            pointer_sized: false,
        })
    }

    fn ptr(pointee: CType, is_const: bool, names: &[&str]) -> CType {
        CType::Pointer {
            pointee: Box::new(pointee),
            is_const,
            pointee_names: names.iter().map(|n| n.to_string()).collect(),
        }
    }

    #[test]
    fn numbers_match_by_width_and_sign() {
        assert!(compare(&velt_int(32, true), &int(32, true)).is_ok());
        assert_eq!(
            compare(&velt_int(64, true), &int(32, true)).unwrap_err(),
            "declare `i32`"
        );
        assert!(compare(&velt_int(32, false), &int(32, true)).is_err());
        assert!(compare(&Shape::Scalar(Scalar::F64), &CType::Double).is_ok());
        assert!(compare(&Shape::Scalar(Scalar::Bool), &int(32, true)).is_err());
        let int_enum = CType::Enum {
            bits: 32,
            signed: None,
        };
        assert!(compare(&velt_int(32, true), &int_enum).is_ok());
        assert!(compare(&velt_int(32, false), &int_enum).is_ok());
        assert_eq!(
            compare(&velt_int(64, true), &int_enum).unwrap_err(),
            "declare `i32` (a C enum)"
        );
        let wide = CType::Enum {
            bits: 64,
            signed: Some(false),
        };
        assert!(compare(&velt_int(64, false), &wide).is_ok());
        assert!(compare(&velt_int(64, true), &wide).is_err());
        assert_eq!(
            compare(&velt_int(32, true), &wide).unwrap_err(),
            "declare `u64` (a C enum)"
        );
        // `char`'s sign is the target's: both match.
        let plain = CType::Int {
            bits: 8,
            signed: true,
            names: vec![],
            plain_char: true,
        };
        assert!(compare(&velt_int(8, false), &plain).is_ok());
    }

    #[test]
    fn size_t_needs_usize_and_usize_needs_size_t() {
        let size_t = CType::Int {
            bits: 64,
            signed: false,
            names: vec!["size_t".into(), "__size_t".into()],
            plain_char: false,
        };
        let usize = Shape::Scalar(Scalar::Int {
            bits: 64,
            signed: false,
            pointer_sized: true,
        });
        assert!(compare(&usize, &size_t).is_ok());
        assert_eq!(
            compare(&velt_int(64, false), &size_t).unwrap_err(),
            "declare `usize`"
        );
        assert!(compare(&usize, &int(64, false)).is_err());
    }

    #[test]
    fn pointers_match_their_pointee() {
        let sqlite = ptr(CType::Named("sqlite3".into()), false, &["sqlite3"]);
        let decl = Shape::Ptr(Pointee::CType("sqlite3".into()));
        assert!(compare(&decl, &sqlite).is_ok());
        let other = Shape::Ptr(Pointee::CType("sqlite3_stmt".into()));
        assert!(compare(&other, &sqlite).is_err());
        // `void *` either way.
        assert!(compare(&decl, &ptr(CType::Void, false, &[])).is_ok());
        assert!(compare(&Shape::Ptr(Pointee::Void), &sqlite).is_ok());
        assert!(compare(&decl, &int(64, false)).is_err());
        assert!(compare(&Shape::Null, &sqlite).is_ok());
        let char_ptr = ptr(int(8, true), true, &["char"]);
        assert!(compare(&Shape::Text, &char_ptr).is_ok());
        assert!(compare(&Shape::Text, &sqlite).is_err());
    }

    #[test]
    fn arrays_and_out_parameters_follow_const() {
        let elem = Scalar::Int {
            bits: 8,
            signed: false,
            pointer_sized: false,
        };
        let const_bytes = ptr(int(8, false), true, &[]);
        let bytes = ptr(int(8, false), false, &[]);
        let read = Shape::Buf {
            elem: elem.clone(),
            readonly: true,
        };
        let written = Shape::Buf {
            elem: elem.clone(),
            readonly: false,
        };
        assert!(compare(&read, &const_bytes).is_ok());
        // Many APIs leave `const` out: a read-only array still matches.
        assert!(compare(&read, &bytes).is_ok());
        assert_eq!(
            compare(&written, &const_bytes).unwrap_err(),
            "C only reads the array: declare it `readonly`"
        );
        assert!(compare(&written, &ptr(CType::Void, false, &[])).is_ok());
        assert!(compare(&written, &ptr(CType::Double, false, &[])).is_err());
        assert!(compare(&Shape::Out(elem.clone()), &bytes).is_ok());
        assert!(compare(&Shape::Out(elem), &const_bytes).is_err());
        let end = ptr(ptr(int(8, true), false, &["char"]), false, &[]);
        let out_ptr = Shape::OutPtr(Pointee::Scalar(Scalar::Int {
            bits: 8,
            signed: false,
            pointer_sized: false,
        }));
        // `char **`: `char`'s sign is unknown here, as `plain_char` is false: a mismatch.
        assert!(compare(&out_ptr, &end).is_err());
        let end = ptr(
            ptr(
                CType::Int {
                    bits: 8,
                    signed: true,
                    names: vec![],
                    plain_char: true,
                },
                false,
                &["char"],
            ),
            false,
            &[],
        );
        assert!(compare(&out_ptr, &end).is_ok());
        assert!(compare(&Shape::Void, &CType::Void).is_ok());
        assert!(compare(&Shape::Void, &int(32, true)).is_err());
    }
}
