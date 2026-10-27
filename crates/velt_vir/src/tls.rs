//! Thread-local values: what each `velt_tls_*` extern means (rt_abi.md "Thread-local values",
//! docs/internals/design/atomics.md).
//!
//! A module constant `new ThreadLocal<T>(init)` is one value per thread, of a number type or
//! `bool`, which starts as `init` on every thread. Lowering calls `velt_tls_get_<id>_<bits>_<init>`
//! to read this thread's value and `velt_tls_set_<id>_<bits>_<init>` to write it: `id` names the
//! constant ([`stable_id`] of its qualified name, so it stays the same across edits of other
//! declarations, which `velt dev` relies on), `bits` is the value's width and `init` its starting
//! word in hex. Values are passed as unsigned integers of that width, as the atomics' are. No runtime
//! function exists behind these symbols: a backend emits each call inline on thread-local storage
//! it defines from the symbol (`velt_tls_data_<id>`, `bits` wide, starting as `init`), or, where it
//! has no thread-local storage, defines the symbols itself as functions on the operating system's
//! thread keys.

use crate::vir::Ty;

/// What a `velt_tls_*` extern does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TlsCall {
    /// `() -> word`: this thread's value.
    Get { id: u64, bits: u32, init: u64 },
    /// `(v)`: this thread's value becomes `v`.
    Set { id: u64, bits: u32, init: u64 },
}

impl TlsCall {
    /// The extern's symbol.
    pub fn symbol(self) -> String {
        let (op, id, bits, init) = self.parts();
        format!("velt_tls_{op}_{id}_{bits}_{init:x}")
    }

    fn parts(self) -> (&'static str, u64, u32, u64) {
        match self {
            TlsCall::Get { id, bits, init } => ("get", id, bits, init),
            TlsCall::Set { id, bits, init } => ("set", id, bits, init),
        }
    }

    /// The call a `velt_tls_*` symbol names, in its canonical spelling ([`TlsCall::symbol`]),
    /// with a starting word that fits its width.
    pub fn of_symbol(symbol: &str) -> Option<TlsCall> {
        let rest = symbol.strip_prefix("velt_tls_")?;
        let mut parts = rest.split('_');
        let op = parts.next()?;
        let id: u64 = parts.next()?.parse().ok()?;
        let bits: u32 = parts.next()?.parse().ok()?;
        let init = u64::from_str_radix(parts.next()?, 16).ok()?;
        if parts.next().is_some() || !matches!(bits, 8 | 16 | 32 | 64) {
            return None;
        }
        if bits < 64 && init >> bits != 0 {
            return None;
        }
        let call = match op {
            "get" => TlsCall::Get { id, bits, init },
            "set" => TlsCall::Set { id, bits, init },
            _ => return None,
        };
        (call.symbol() == symbol).then_some(call)
    }

    /// The constant's number: [`stable_id`] of its qualified name.
    pub fn id(self) -> u64 {
        self.parts().1
    }

    /// The value's width in bits.
    pub fn bits(self) -> u32 {
        self.parts().2
    }

    /// The word every thread's value starts as.
    pub fn init(self) -> u64 {
        self.parts().3
    }

    /// The symbol of the storage a backend defines for the constant.
    pub fn data_symbol(self) -> String {
        format!("velt_tls_data_{}", self.id())
    }

    /// The VIR signature the extern must have.
    pub fn signature(self) -> (Vec<Ty>, Ty) {
        let w = crate::atomic::word(self.bits());
        match self {
            TlsCall::Get { .. } => (vec![], w),
            TlsCall::Set { .. } => (vec![w], Ty::Unit),
        }
    }
}

/// A constant's id: the 64-bit FNV-1a hash of its qualified name (`module::name`), stable across
/// versions of a program, unlike its `DefId`.
pub fn stable_id(qualified_name: &str) -> u64 {
    qualified_name.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The symbol of the slot holding a constant's operating-system thread key (plus one, 0 before
/// it is made), where a backend uses the keys (crate docs).
pub fn key_symbol(id: u64) -> String {
    format!("velt_tls_key_{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_round_trip() {
        for bits in [8, 16, 32, 64] {
            for init in [0, 1, 0x7f] {
                for call in [
                    TlsCall::Get { id: 3, bits, init },
                    TlsCall::Set {
                        id: u64::MAX,
                        bits,
                        init,
                    },
                ] {
                    assert_eq!(TlsCall::of_symbol(&call.symbol()), Some(call));
                }
            }
        }
        let top = TlsCall::Get {
            id: 0,
            bits: 64,
            init: u64::MAX,
        };
        assert_eq!(TlsCall::of_symbol(&top.symbol()), Some(top));
    }

    #[test]
    fn ids_follow_the_name() {
        assert_eq!(stable_id("app::calls"), stable_id("app::calls"));
        assert_ne!(stable_id("app::calls"), stable_id("app::call"));
        assert_ne!(stable_id("app::calls"), stable_id("lib::calls"));
    }

    #[test]
    fn other_spellings_are_not_calls() {
        for s in [
            "velt_tls_get_1_64",
            "velt_tls_get_1_12_0",
            "velt_tls_get_01_64_0",
            "velt_tls_get_1_64_0_0",
            "velt_tls_put_1_64_0",
            "velt_tls_get_1_8_100",
            "velt_tls_get_1_64_0A",
            "velt_tls_data_1",
        ] {
            assert_eq!(TlsCall::of_symbol(s), None, "{s}");
        }
    }
}
