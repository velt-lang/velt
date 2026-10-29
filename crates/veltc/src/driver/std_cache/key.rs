//! A std function's key: everything its compiled object depends on.
//!
//! - Who compiles it for what ([`Inputs`]): the compiler (`build_cache::compiler_identity`),
//!   the LLVM backend's clang (its path, size and time, as the unit cache keys it), the target,
//!   the CPU and the optimization level.
//! - What it is ([`Code`]): its canonical code (`velt_vir::fingerprint`, `Mode::Link`: callees,
//!   statics and aggregates by content and symbol, externs with their declarations, the target
//!   feature set, poll state) and its parameter attributes; then the same for every small
//!   function its code may inline (`select::inlinable`), since those are compiled into it as
//!   they are in this program.
//!
//! Whole-program facts that change a function's code (counted types, atomic counts in a program
//! with threads, frozen and caller-tracking instances, error sets) show in its lowered code, so
//! the canonical code covers them.

use velt_build_cache::{Key, KeyHasher};
use velt_vir::fingerprint::Canon;
use velt_vir::vir;

use crate::driver::BuildOptions;

/// Who compiles a std function for what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Inputs {
    pub compiler: Key,
    pub backend: String,
    pub target: String,
    pub cpu: Option<String>,
    pub optimize: bool,
}

/// What a function is, as its key sees it.
pub(super) struct Code {
    pub symbol: String,
    pub text: String,
    pub attrs: String,
}

impl Code {
    pub fn of(f: &vir::Function, canon: &Canon) -> Code {
        Code {
            symbol: f.symbol.clone(),
            text: canon.text(f),
            attrs: format!("{:?}", f.param_attrs),
        }
    }
}

impl Inputs {
    /// This velt building with `opts` for `cg`.
    pub fn of(opts: &BuildOptions, cg: &velt_codegen_cl::CodegenOptions) -> Inputs {
        let clang = velt_codegen_llvm::find_clang();
        let meta = clang.as_ref().and_then(|c| std::fs::metadata(c).ok());
        let stamp = meta.map(|m| (m.len(), m.modified().ok()));
        Inputs {
            compiler: crate::build_cache::compiler_identity(),
            backend: format!("clang {clang:?} {stamp:?}"),
            target: cg.target.clone(),
            cpu: cg.cpu.clone(),
            optimize: opts.release,
        }
    }

    /// The key of a function whose code is `code`: itself, then what it may inline.
    pub fn key(&self, code: impl IntoIterator<Item = Code>) -> Key {
        let mut h = KeyHasher::new();
        h.part(b"velt std function 1");
        h.part(&self.compiler.0);
        h.part(self.backend.as_bytes());
        h.part(self.target.as_bytes());
        h.part(self.cpu.as_deref().unwrap_or("").as_bytes());
        h.part(&[u8::from(self.cpu.is_some()), u8::from(self.optimize)]);
        for c in code {
            h.part(c.symbol.as_bytes());
            h.part(c.text.as_bytes());
            h.part(c.attrs.as_bytes());
        }
        h.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Inputs {
        Inputs {
            compiler: Key([1; 32]),
            backend: "clang a".into(),
            target: "aarch64-apple-darwin".into(),
            cpu: None,
            optimize: true,
        }
    }

    fn code(symbol: &str, text: &str, attrs: &str) -> Code {
        Code {
            symbol: symbol.into(),
            text: text.into(),
            attrs: attrs.into(),
        }
    }

    fn of(inputs: &Inputs, parts: &[(&str, &str, &str)]) -> Key {
        inputs.key(parts.iter().map(|&(s, t, a)| code(s, t, a)))
    }

    const F: (&str, &str, &str) = ("_V3stdN1f", "fn() body", "[]");
    const G: (&str, &str, &str) = ("_V3stdN1g", "fn() small", "[]");

    /// Each input, changed alone, gives another key.
    #[test]
    fn every_input_is_in_the_key() {
        let key = of(&base(), &[F, G]);
        assert_eq!(key, of(&base(), &[F, G]), "deterministic");
        let inputs: [Inputs; 6] = [
            Inputs {
                compiler: Key([2; 32]),
                ..base()
            },
            Inputs {
                backend: "clang b".into(),
                ..base()
            },
            Inputs {
                target: "x86_64-unknown-linux-gnu".into(),
                ..base()
            },
            Inputs {
                cpu: Some("native".into()),
                ..base()
            },
            Inputs {
                cpu: Some(String::new()),
                ..base()
            },
            Inputs {
                optimize: false,
                ..base()
            },
        ];
        for (n, other) in inputs.iter().enumerate() {
            assert_ne!(key, of(other, &[F, G]), "input {n}");
        }
        let code = [
            [("_V3stdN1h", F.1, F.2), G],
            [(F.0, "fn() other body", F.2), G],
            [(F.0, F.1, "[noalias]"), G],
            [F, (G.0, "fn() other small", G.2)],
            [F, (G.0, G.1, "[frozen]")],
        ];
        for (n, parts) in code.iter().enumerate() {
            assert_ne!(key, of(&base(), parts), "code {n}");
        }
        assert_ne!(key, of(&base(), &[F]), "an inlinable function less");
        assert_ne!(key, of(&base(), &[G, F]), "another function first");
    }
}
