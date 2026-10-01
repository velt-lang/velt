//! Per-function slots and trampolines (docs/internals/design/hot-reload.md, phase 3). cranelift-jit has no
//! hot-swap mode, so every swappable function gets
//! - a **slot**: an atomic code pointer in host memory, and
//! - a **trampoline**: a few instructions that jump through the slot. Calls and function
//!   addresses (closures, function values, vtable slots) all use the trampoline, so storing a
//!   new pointer into the slot redirects every later call, from old and new code alike.
//!
//! The trampoline holds the slot's absolute address, so it needs no relocations and can live in
//! any JIT module (each version defines the trampolines of its new keys). It clobbers only the
//! intra-procedure scratch register (x86_64 `r11`, aarch64 `x16`), which no calling convention
//! uses for arguments, and leaves the stack as it found it: to unwinders a trampoline without
//! unwind info looks like a frameless leaf function, which it is.

use std::sync::atomic::{AtomicUsize, Ordering};

/// A function's slot: where its trampoline jumps.
pub(crate) struct Slot {
    cell: &'static AtomicUsize,
}

impl Slot {
    /// A new slot, not yet pointing anywhere (set it before anything can call the trampoline).
    pub(crate) fn new() -> Slot {
        // Leaked on purpose: trampolines refer to it for the rest of the process.
        Slot {
            cell: Box::leak(Box::new(AtomicUsize::new(0))),
        }
    }

    /// The trampoline's machine code for this slot.
    pub(crate) fn trampoline(&self) -> Vec<u8> {
        stub(self.cell as *const AtomicUsize as u64)
    }

    /// Point the slot at `code`: calls that start from now on run it.
    pub(crate) fn set(&self, code: *const u8) {
        self.cell.store(code as usize, Ordering::Release);
    }
}

/// `movabs r11, <slot>; jmp qword ptr [r11]`.
#[cfg(target_arch = "x86_64")]
fn stub(slot: u64) -> Vec<u8> {
    let mut code = vec![0x49, 0xBB];
    code.extend_from_slice(&slot.to_le_bytes());
    code.extend_from_slice(&[0x41, 0xFF, 0x23]);
    code
}

/// `ldr x16, <literal>; ldr x16, [x16]; br x16; nop; <literal: slot>`.
#[cfg(target_arch = "aarch64")]
fn stub(slot: u64) -> Vec<u8> {
    // LDR (literal) x16, pc+16 | LDR x16, [x16] | BR x16 | NOP (8-aligns the literal).
    let words: [u32; 4] = [0x5800_0090, 0xF940_0210, 0xD61F_0200, 0xD503_201F];
    let mut code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    code.extend_from_slice(&slot.to_le_bytes());
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn x86_64_stub_encodes_the_slot_address() {
        let code = stub(0x1122_3344_5566_7788);
        assert_eq!(
            code,
            [0x49, 0xBB, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x41, 0xFF, 0x23]
        );
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn aarch64_stub_encodes_the_slot_address() {
        let code = stub(0x1122_3344_5566_7788);
        assert_eq!(code.len(), 24);
        assert_eq!(&code[16..], &0x1122_3344_5566_7788u64.to_le_bytes());
    }
}
