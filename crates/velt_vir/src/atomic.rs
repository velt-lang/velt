//! The atomic operations: what each `velt_atomic_*` extern means (rt_abi.md "Atomics",
//! docs/internals/design/atomics.md).
//!
//! Lowering calls one extern per operation, width and ordering, `velt_atomic_<op>_<bits>_<order>`
//! (and `_<failure>` for a compare-and-swap), with the address of the word as the first
//! argument; backends emit each call inline from the [`AtomicCall`] its symbol parses to. No
//! runtime function exists behind these symbols. Values are passed as unsigned integers of the
//! word's width; the operation says whether `max` and `min` compare them signed.

use crate::vir::Ty;

/// The symbol of a static atomic's word (`StaticData::mutable`) in a `velt dev` session, which
/// owns one per constant `id` for all of its versions.
pub fn static_symbol(id: u64) -> String {
    format!("velt_static_atomic_{id}")
}

/// A memory ordering, as C++ and Rust define them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Order {
    Relaxed,
    Acquire,
    Release,
    AcqRel,
    SeqCst,
}

impl Order {
    pub const ALL: [Order; 5] = [
        Order::Relaxed,
        Order::Acquire,
        Order::Release,
        Order::AcqRel,
        Order::SeqCst,
    ];

    /// Its name in symbols: `rlx`, `acq`, `rel`, `acqrel`, `seqcst`.
    pub fn name(self) -> &'static str {
        match self {
            Order::Relaxed => "rlx",
            Order::Acquire => "acq",
            Order::Release => "rel",
            Order::AcqRel => "acqrel",
            Order::SeqCst => "seqcst",
        }
    }

    pub fn named(s: &str) -> Option<Order> {
        Order::ALL.into_iter().find(|o| o.name() == s)
    }

    /// Its spelling in Velt (`Atomic`'s ordering arguments): `"relaxed"`, `"acquire"`,
    /// `"release"`, `"acqrel"`, `"seqcst"`.
    pub fn velt_name(self) -> &'static str {
        match self {
            Order::Relaxed => "relaxed",
            Order::Acquire => "acquire",
            Order::Release => "release",
            Order::AcqRel => "acqrel",
            Order::SeqCst => "seqcst",
        }
    }

    pub fn of_velt_name(s: &str) -> Option<Order> {
        Order::ALL.into_iter().find(|o| o.velt_name() == s)
    }

    /// The failure ordering of a compare-and-swap whose success ordering is `self` and whose
    /// failure ordering is not given: `self` without its release part, as Rust's
    /// `compare_exchange` picks it.
    pub fn default_failure(self) -> Order {
        match self {
            Order::Relaxed | Order::Release => Order::Relaxed,
            Order::Acquire | Order::AcqRel => Order::Acquire,
            Order::SeqCst => Order::SeqCst,
        }
    }

    /// Whether a load may have this ordering (also a compare-and-swap's failure ordering).
    pub fn loads(self) -> bool {
        matches!(self, Order::Relaxed | Order::Acquire | Order::SeqCst)
    }

    /// Whether a store may have this ordering.
    pub fn stores(self) -> bool {
        matches!(self, Order::Relaxed | Order::Release | Order::SeqCst)
    }
}

/// A read-modify-write operation: the word becomes `op(old, v)`, and the call gives `old`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rmw {
    /// `v` (`exchange`).
    Xchg,
    /// `old + v`, wrapping.
    Add,
    /// `old - v`, wrapping.
    Sub,
    And,
    Or,
    Xor,
    /// The larger, compared signed.
    Max,
    /// The smaller, compared signed.
    Min,
    /// The larger, compared unsigned.
    UMax,
    /// The smaller, compared unsigned.
    UMin,
}

impl Rmw {
    pub const ALL: [Rmw; 10] = [
        Rmw::Xchg,
        Rmw::Add,
        Rmw::Sub,
        Rmw::And,
        Rmw::Or,
        Rmw::Xor,
        Rmw::Max,
        Rmw::Min,
        Rmw::UMax,
        Rmw::UMin,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Rmw::Xchg => "xchg",
            Rmw::Add => "add",
            Rmw::Sub => "sub",
            Rmw::And => "and",
            Rmw::Or => "or",
            Rmw::Xor => "xor",
            Rmw::Max => "max",
            Rmw::Min => "min",
            Rmw::UMax => "umax",
            Rmw::UMin => "umin",
        }
    }
}

/// What a `velt_atomic_*` extern does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AtomicCall {
    /// `(p) -> word`: the word at `p`.
    Load { bits: u32, order: Order },
    /// `(p, v)`: `*p = v`.
    Store { bits: u32, order: Order },
    /// `(p, v) -> old`.
    Rmw { op: Rmw, bits: u32, order: Order },
    /// `(p, expected, v) -> old`: `*p = v` if `*p == expected`; `old == expected` tells
    /// whether it was. `weak`: `(p, expected, v) -> Bool`, whether it was, and it may fail
    /// although `*p == expected` (a loop retries it), which is cheaper on LL/SC machines.
    Cas {
        bits: u32,
        order: Order,
        failure: Order,
        weak: bool,
    },
    /// `()`: a fence.
    Fence(Order),
    /// `()`: a hint that the thread spins on a word (`pause` on x86, `isb` on aarch64); no
    /// effect on memory.
    SpinHint,
}

impl AtomicCall {
    /// The extern's symbol.
    pub fn symbol(self) -> String {
        match self {
            AtomicCall::Load { bits, order } => format!("velt_atomic_load_{bits}_{}", order.name()),
            AtomicCall::Store { bits, order } => {
                format!("velt_atomic_store_{bits}_{}", order.name())
            }
            AtomicCall::Rmw { op, bits, order } => {
                format!("velt_atomic_{}_{bits}_{}", op.name(), order.name())
            }
            AtomicCall::Cas {
                bits,
                order,
                failure,
                weak,
            } => format!(
                "velt_atomic_{}_{bits}_{}_{}",
                if weak { "casweak" } else { "cas" },
                order.name(),
                failure.name()
            ),
            AtomicCall::Fence(order) => format!("velt_atomic_fence_{}", order.name()),
            AtomicCall::SpinHint => "velt_atomic_spin_hint".to_string(),
        }
    }

    /// The call a `velt_atomic_*` symbol names, if it names a valid one (a load's ordering is
    /// one a load may have, and so on) in its canonical spelling ([`AtomicCall::symbol`]).
    pub fn of_symbol(symbol: &str) -> Option<AtomicCall> {
        let rest = symbol.strip_prefix("velt_atomic_")?;
        if rest == "spin_hint" {
            return Some(AtomicCall::SpinHint);
        }
        let mut parts = rest.split('_');
        let op = parts.next()?;
        if op == "fence" {
            let order = Order::named(parts.next()?)?;
            return (parts.next().is_none() && order != Order::Relaxed)
                .then_some(AtomicCall::Fence(order));
        }
        let bits: u32 = parts.next()?.parse().ok()?;
        if !matches!(bits, 8 | 16 | 32 | 64) {
            return None;
        }
        let order = Order::named(parts.next()?)?;
        let call = match op {
            "load" => order.loads().then_some(AtomicCall::Load { bits, order })?,
            "store" => order
                .stores()
                .then_some(AtomicCall::Store { bits, order })?,
            "cas" | "casweak" => {
                let failure = Order::named(parts.next()?)?;
                if !failure.loads() {
                    return None;
                }
                AtomicCall::Cas {
                    bits,
                    order,
                    failure,
                    weak: op == "casweak",
                }
            }
            _ => {
                let op = Rmw::ALL.into_iter().find(|r| r.name() == op)?;
                AtomicCall::Rmw { op, bits, order }
            }
        };
        // Only the canonical spelling: `load_08` and `load_+8` would name `load_8` too.
        (parts.next().is_none() && call.symbol() == symbol).then_some(call)
    }

    /// The word's width in bits (0 for a fence or spin hint).
    pub fn bits(self) -> u32 {
        match self {
            AtomicCall::Load { bits, .. }
            | AtomicCall::Store { bits, .. }
            | AtomicCall::Rmw { bits, .. }
            | AtomicCall::Cas { bits, .. } => bits,
            AtomicCall::Fence(_) | AtomicCall::SpinHint => 0,
        }
    }

    /// The VIR signature the extern must have.
    pub fn signature(self) -> (Vec<Ty>, Ty) {
        let w = word(self.bits());
        match self {
            AtomicCall::Load { .. } => (vec![Ty::Ptr], w),
            AtomicCall::Store { .. } => (vec![Ty::Ptr, w], Ty::Unit),
            AtomicCall::Rmw { .. } => (vec![Ty::Ptr, w], w),
            AtomicCall::Cas { weak: false, .. } => (vec![Ty::Ptr, w, w], w),
            AtomicCall::Cas { weak: true, .. } => (vec![Ty::Ptr, w, w], Ty::Bool),
            AtomicCall::Fence(_) | AtomicCall::SpinHint => (vec![], Ty::Unit),
        }
    }
}

/// The unsigned VIR type of a `bits`-wide word (`U8` for 0, which no word has).
pub fn word(bits: u32) -> Ty {
    match bits {
        16 => Ty::U16,
        32 => Ty::U32,
        64 => Ty::U64,
        _ => Ty::U8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every call's symbol parses back to it.
    #[test]
    fn symbols_round_trip() {
        let mut calls = vec![AtomicCall::SpinHint];
        for order in Order::ALL {
            if order != Order::Relaxed {
                calls.push(AtomicCall::Fence(order));
            }
            for bits in [8, 16, 32, 64] {
                if order.loads() {
                    calls.push(AtomicCall::Load { bits, order });
                }
                if order.stores() {
                    calls.push(AtomicCall::Store { bits, order });
                }
                for op in Rmw::ALL {
                    calls.push(AtomicCall::Rmw { op, bits, order });
                }
                for failure in Order::ALL.into_iter().filter(|f| f.loads()) {
                    for weak in [false, true] {
                        calls.push(AtomicCall::Cas {
                            bits,
                            order,
                            failure,
                            weak,
                        });
                    }
                }
            }
        }
        for c in calls {
            assert_eq!(
                AtomicCall::of_symbol(&c.symbol()),
                Some(c),
                "{}",
                c.symbol()
            );
        }
    }

    #[test]
    fn invalid_symbols() {
        for s in [
            "velt_atomic_load_64_rel",
            "velt_atomic_store_64_acq",
            "velt_atomic_load_128_acq",
            "velt_atomic_cas_64_acq_rel",
            "velt_atomic_cas_64_acq",
            "velt_atomic_fence_rlx",
            "velt_atomic_add_64_acq_x",
            "velt_atomic_nand_64_acq",
            "velt_simd_prefetch",
            "velt_atomic_load_08_acq",
            "velt_atomic_load_+8_acq",
        ] {
            assert_eq!(AtomicCall::of_symbol(s), None, "{s}");
        }
    }

    #[test]
    fn default_failure_orders_load() {
        for o in Order::ALL {
            assert!(o.default_failure().loads());
        }
    }
}
