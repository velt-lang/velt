//! A std function's key: everything its compiled object depends on.
//!
//! - Who compiles it for what ([`Inputs`]): the compiler (`build_cache::compiler_identity`),
//!   the LLVM backend's clang (its path, size and time, as the unit cache keys it), the target,
//!   the CPU, and whether a release or a debug build links it.
//! - What it is ([`Code`]): its canonical code (`velt_vir::fingerprint`, `Mode::Link`: callees,
//!   statics and aggregates by content and symbol, externs with their declarations, the target
//!   feature set, poll state) and its parameter attributes; then the same for every small
//!   function its code may inline (`select::inlinable`), since those are compiled into it as
//!   they are in this program.
//!
//! Whole-program facts that change a function's code (counted types, atomic counts in a program
//! with threads, frozen and caller-tracking instances, error sets) show in its lowered code, so
//! the canonical code covers them.
//!
//! The parameter attributes `noalias`, `readonly` and `frozen` are whole-program facts too
//! (velt_vir `lower/param_attrs.rs`: whether a type is counted or borrowed inside counted objects
//! anywhere in the program, and what module state reaches), and they don't show in the code: a
//! program that sorts an array of a class holding a string with a comparator has `string` params
//! without `readonly frozen`. So a release build keys an object two ways ([`Assumed`]): with the
//! program's attributes, which its object assumes, and with none of the three, under which an
//! object compiled with none of them is right in every program that has the same code.
//!
//! A debug build's lookups ([`Inputs::debug`]) name no clang and always the target's default CPU:
//! a reader has no use for either, and may have no clang at all.

use velt_build_cache::{Key, KeyHasher};
use velt_vir::fingerprint::Canon;
use velt_vir::vir;

use crate::driver::BuildOptions;

/// Who compiles a std function for what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Inputs {
    pub compiler: Key,
    pub backend: String,
    /// clang's optimization flag (`VELT_LLVM_OPT`, `-O3` by default).
    pub opt: String,
    pub target: String,
    pub cpu: Option<String>,
    /// For a release build. Both are compiled at `Speed`, but each build lowers its code its own
    /// way, and a debug build's object never serves a release build, nor the other way round.
    pub release: bool,
}

/// The parameter attributes an object is compiled with, and keyed by: those of every function
/// of its program of its own (the served function, what it is compiled with, what it declares).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Assumed {
    /// The program's, as lowered. They hold in this program, and in every program that makes the
    /// same key, as the key holds them all.
    Program,
    /// The program's without `noalias`, `readonly` and `frozen`, the attributes that depend on
    /// the whole program. Leaving one out only takes a fact away from the optimizers, so an
    /// object compiled so is right in every program with the same code and the same other
    /// attributes (`nonnull`, `dereferenceable`: the types' layouts), and they all make this key.
    Conservative,
}

impl Assumed {
    /// `attrs` as an object compiled under `self` assumes them.
    pub fn attrs(self, attrs: &[vir::ParamAttrs]) -> Vec<vir::ParamAttrs> {
        match self {
            Assumed::Program => attrs.to_vec(),
            Assumed::Conservative => (attrs.iter())
                .map(|a| vir::ParamAttrs {
                    noalias: false,
                    readonly: false,
                    frozen: false,
                    ..*a
                })
                .collect(),
        }
    }
}

/// What a function is, as its key sees it.
#[derive(Clone)]
pub(super) struct Code {
    pub symbol: String,
    pub text: String,
    pub attrs: String,
}

impl Code {
    pub fn of(f: &vir::Function, canon: &Canon, assumed: Assumed) -> Code {
        Code::with_text(f, canon.text(f), assumed)
    }

    /// [`Code::of`] with `f`'s canonical text already worked out.
    pub fn with_text(f: &vir::Function, text: String, assumed: Assumed) -> Code {
        Code {
            symbol: f.symbol.clone(),
            text,
            attrs: format!("{:?}", assumed.attrs(&f.param_attrs)),
        }
    }

    /// A function the object declares (calls without compiling it): its signature and the
    /// parameter attributes LLVM declares it with, which the caller is optimized against. They
    /// are whole-program facts (`readonly` depends on the counted types, `noalias` on module
    /// state), so two programs can declare one callee differently.
    pub fn declared(f: &vir::Function, assumed: Assumed) -> Code {
        Code {
            symbol: f.symbol.clone(),
            text: format!("declare {:?} -> {:?}", f.params, f.ret),
            attrs: format!("{:?}", assumed.attrs(&f.param_attrs)),
        }
    }
}

impl Inputs {
    /// This velt building with `opts` for `cg`.
    pub fn of(opts: &BuildOptions, cg: &velt_codegen_cl::CodegenOptions) -> Inputs {
        // clang's path, size and modification time (nanoseconds), as explicit fields, and the version
        // it reports: a new clang can sit behind the same three.
        let clang = velt_codegen_llvm::find_clang();
        let meta = clang.as_ref().and_then(|c| std::fs::metadata(c).ok());
        let size = meta.as_ref().map_or(0, |m| m.len());
        let time = (meta.and_then(|m| m.modified().ok()))
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        let path = clang
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_default();
        Inputs {
            compiler: crate::build_cache::compiler_identity(),
            backend: format!(
                "clang\0{path}\0{size}\0{time}\0{}",
                velt_codegen_llvm::clang_version().unwrap_or_default()
            ),
            opt: velt_codegen_llvm::opt_flag().to_string(),
            target: cg.target.clone(),
            cpu: cg.cpu.clone(),
            release: opts.release,
        }
    }

    /// Who compiled what a debug build links: objects that `velt cache warm` or a prebuilt
    /// toolchain stored, whichever clang did it, for the target's default CPU.
    pub fn debug(target: &str) -> Inputs {
        Inputs {
            compiler: crate::build_cache::compiler_identity(),
            backend: "llvm".into(),
            opt: velt_codegen_llvm::opt_flag().to_string(),
            target: target.to_string(),
            cpu: None,
            release: false,
        }
    }

    /// The key of a function whose code is `code`: itself, then what it may inline.
    pub fn key(&self, code: impl IntoIterator<Item = Code>) -> Key {
        let mut h = self.hasher(b"velt std function 1");
        for c in code {
            h.part(c.symbol.as_bytes());
            h.part(c.text.as_bytes());
            h.part(c.attrs.as_bytes());
        }
        h.finish()
    }

    /// The key of a function from what each function of its code adds (`serve`'s digests).
    pub fn key_of_digests(&self, digests: &[Key]) -> Key {
        let mut h = self.hasher(b"velt std function digests 1");
        for d in digests {
            h.part(&d.0);
        }
        h.finish()
    }

    fn hasher(&self, what: &[u8]) -> KeyHasher {
        let mut h = KeyHasher::new();
        h.part(what);
        h.part(&self.compiler.0);
        h.part(self.backend.as_bytes());
        h.part(self.opt.as_bytes());
        h.part(self.target.as_bytes());
        h.part(self.cpu.as_deref().unwrap_or("").as_bytes());
        h.part(&[u8::from(self.cpu.is_some()), u8::from(self.release)]);
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Inputs {
        Inputs {
            compiler: Key([1; 32]),
            backend: "clang a".into(),
            opt: "-O3".into(),
            target: "aarch64-apple-darwin".into(),
            cpu: None,
            release: true,
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

    /// A debug build's key: the inputs, and each function's digest in order.
    #[test]
    fn digests_and_inputs_are_in_a_debug_key() {
        let (a, b) = (Key([1; 32]), Key([2; 32]));
        let key = base().key_of_digests(&[a, b]);
        assert_eq!(key, base().key_of_digests(&[a, b]));
        assert_ne!(key, base().key_of_digests(&[b, a]));
        assert_ne!(key, base().key_of_digests(&[a]));
        for other in [
            Inputs {
                target: "x86_64-unknown-linux-gnu".into(),
                ..base()
            },
            Inputs {
                compiler: Key([2; 32]),
                ..base()
            },
            Inputs {
                opt: "-O1".into(),
                ..base()
            },
            Inputs {
                backend: "other".into(),
                ..base()
            },
            Inputs {
                cpu: Some("other".into()),
                ..base()
            },
            Inputs {
                release: !base().release,
                ..base()
            },
        ] {
            assert_ne!(key, other.key_of_digests(&[a, b]));
        }
        assert_eq!(
            Inputs::debug("t").cpu,
            None,
            "always the default CPU's objects"
        );
        assert!(
            !Inputs::debug("t").release,
            "never a release build's objects"
        );
    }

    /// Each input, changed alone, gives another key.
    #[test]
    fn every_input_is_in_the_key() {
        let key = of(&base(), &[F, G]);
        assert_eq!(key, of(&base(), &[F, G]), "deterministic");
        let inputs: [Inputs; 7] = [
            Inputs {
                opt: "-O1".into(),
                ..base()
            },
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
                release: false,
                ..base()
            },
        ];
        for (n, other) in inputs.iter().enumerate() {
            assert_ne!(key, of(other, &[F, G]), "input {n}");
        }
        let changed_code = [
            [("_V3stdN1h", F.1, F.2), G],
            [(F.0, "fn() other body", F.2), G],
            [(F.0, F.1, "[noalias]"), G],
            [F, (G.0, "fn() other small", G.2)],
            [F, (G.0, G.1, "[frozen]")],
        ];
        for (n, parts) in changed_code.iter().enumerate() {
            assert_ne!(key, of(&base(), parts), "code {n}");
        }
        assert_ne!(key, of(&base(), &[F]), "an inlinable function less");
        // A declared callee whose attributes differ (`readonly` here, not there).
        let mut g = vir::Function::new(
            "_V3stdN1g".into(),
            vec![vir::Ty::Ptr],
            vir::Ty::Unit,
            vir::Linkage::Import,
        );
        g.param_attrs = vec![vir::ParamAttrs::default()];
        let plain = base().key([code(F.0, F.1, F.2), Code::declared(&g, Assumed::Program)]);
        g.param_attrs = vec![vir::ParamAttrs {
            readonly: true,
            ..vir::ParamAttrs::default()
        }];
        assert_ne!(
            plain,
            base().key([code(F.0, F.1, F.2), Code::declared(&g, Assumed::Program)]),
            "declared attributes"
        );
        assert_eq!(
            base().key([
                code(F.0, F.1, F.2),
                Code::declared(&g, Assumed::Conservative)
            ]),
            plain,
            "but not those that depend on the whole program, where none are assumed"
        );
        assert_ne!(key, of(&base(), &[G, F]), "another function first");
    }
}
