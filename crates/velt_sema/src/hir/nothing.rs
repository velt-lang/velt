//! `Nothing`: which of `null` / `undefined` an option type allows (issue #930).

/// Which nothings a `TyKind::Option` allows (issue #930, hir_encodings.md "Undefined").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Nothing {
    /// `T | null`
    Null,
    /// `T | undefined`
    Undefined,
    /// `T | null | undefined`
    Both,
}

impl Nothing {
    /// The nothings of `(T | self) | other`.
    pub fn join(self, other: Nothing) -> Nothing {
        if self == other {
            self
        } else {
            Nothing::Both
        }
    }

    /// Does `other` allow every nothing `self` does?
    pub fn within(self, other: Nothing) -> bool {
        self == other || other == Nothing::Both
    }

    pub fn has_null(self) -> bool {
        self != Nothing::Undefined
    }

    pub fn has_undefined(self) -> bool {
        self != Nothing::Null
    }

    /// How a value of a two-state option prints its nothing: `null` or `undefined` (a
    /// three-state option never reaches lowering in this phase).
    pub fn word(self) -> &'static str {
        match self {
            Nothing::Undefined => "undefined",
            _ => "null",
        }
    }

    /// How a type shows it: `" | null"`, `" | undefined"`, `" | null | undefined"`.
    pub fn suffix(self) -> &'static str {
        match self {
            Nothing::Null => " | null",
            Nothing::Undefined => " | undefined",
            Nothing::Both => " | null | undefined",
        }
    }
}
