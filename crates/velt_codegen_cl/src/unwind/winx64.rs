//! Windows x64 `UNWIND_INFO` records, written from Cranelift's description of the prologue
//! (`UnwindInst`s) instead of by Cranelift's own Windows x64 writer.
//!
//! That writer (cranelift-codegen 0.132) describes the register saves of a frame-pointer frame
//! wrongly: it leaves the frame offset at 0 and scales every save offset by 16, so the unwinder
//! reads the saved `rbx`, `rsi`, `rdi`, `r12`-`r15` and `xmm6`-`xmm15` from the saved `rbp`,
//! the return address and the caller's frame. A stack walk only needs `rip`, `rsp` and `rbp`,
//! which it gets right, so backtraces and debuggers worked; a structured exception (a Velt panic
//! unwinding to `catchPanic`, #2008) restores every callee-saved register, and the frame that
//! catches it then runs on garbage.
//!
//! Cranelift's x64 prologue is
//!
//! ```text
//! push rbp                 ; PushFrameRegs
//! mov  rbp, rsp            ; DefineNewFrame { offset_downward_to_clobbers: C }
//! sub  rsp, N              ; (no unwind op: rbp gives rsp back)
//! mov  [rsp + k + o], reg  ; SaveReg { clobber_offset: o }: reg is at rbp - C + o
//! ```
//!
//! Windows reads saved registers at unsigned offsets from the frame base, `rbp - 16 × frame
//! offset`, so the record describes the equivalent prologue `push rbp; sub rsp, C;
//! lea rbp, [rsp + C]`: frame offset C / 16, so the base is the clobber area, and each save at
//! `o / 8` (integer, scaled by 8) or `o / 16` (XMM, scaled by 16) from it. Unwinding from the body
//! then sets `rsp = rbp - C`, adds C back, and pops `rbp`, as before.

use cranelift_codegen::isa::unwind::UnwindInst;
use cranelift_codegen::RealReg;

use crate::CodegenResult;

const UWOP_PUSH_NONVOL: u8 = 0;
const UWOP_ALLOC_LARGE: u8 = 1;
const UWOP_ALLOC_SMALL: u8 = 2;
const UWOP_SET_FPREG: u8 = 3;
const UWOP_SAVE_NONVOL: u8 = 4;
const UWOP_SAVE_NONVOL_FAR: u8 = 5;
const UWOP_SAVE_XMM128: u8 = 8;
const UWOP_SAVE_XMM128_FAR: u8 = 9;
/// `rbp` in the unwind codes' register numbering (the same as the x64 encoding).
const RBP: u8 = 5;

/// One unwind code: its prologue offset and its 16-bit nodes (the first holds the offset).
type Code = Vec<u16>;

fn code(at: u8, op: u8, info: u8, rest: &[u16]) -> Code {
    let mut nodes = vec![u16::from_le_bytes([at, op | (info << 4)])];
    nodes.extend_from_slice(rest);
    nodes
}

fn alloc(at: u8, size: u32) -> CodegenResult<Code> {
    if size == 0 || !size.is_multiple_of(8) {
        return Err(format!("ICE: Windows x64 stack allocation of {size} bytes"));
    }
    Ok(if size <= 128 {
        code(at, UWOP_ALLOC_SMALL, (size / 8 - 1) as u8, &[])
    } else if size / 8 <= u32::from(u16::MAX) {
        code(at, UWOP_ALLOC_LARGE, 0, &[(size / 8) as u16])
    } else {
        code(at, UWOP_ALLOC_LARGE, 1, &[size as u16, (size >> 16) as u16])
    })
}

fn save(at: u8, offset: u32, reg: RealReg) -> CodegenResult<Code> {
    // `RegClass` is not exported; its discriminants are Int = 0, Float = 1, Vector = 2.
    let (near, far, scale) = match reg.class() as u8 {
        0 => (UWOP_SAVE_NONVOL, UWOP_SAVE_NONVOL_FAR, 8),
        1 => (UWOP_SAVE_XMM128, UWOP_SAVE_XMM128_FAR, 16),
        _ => return Err("ICE: Windows x64 unwind info for a vector register save".into()),
    };
    if !offset.is_multiple_of(scale) {
        return Err(format!(
            "ICE: Windows x64 register save at unaligned offset {offset}"
        ));
    }
    let enc = reg.hw_enc();
    Ok(match u16::try_from(offset / scale) {
        Ok(scaled) => code(at, near, enc, &[scaled]),
        Err(_) => code(at, far, enc, &[offset as u16, (offset >> 16) as u16]),
    })
}

/// The `UNWIND_INFO` record (version 1, no handler) for a function whose prologue Cranelift
/// described with `insts` (`MachBufferFinalized::unwind_info`).
pub(crate) fn record(insts: &[(u32, UnwindInst)]) -> CodegenResult<Vec<u8>> {
    // In prologue order; written in reverse.
    let mut codes: Vec<Code> = Vec::new();
    let mut clobbers: Option<u32> = None;
    let mut prologue = 0u8;
    for (at, inst) in insts {
        let at = u8::try_from(*at).map_err(|_| {
            "codegen: a function prologue over 255 bytes is too long for Windows x64 unwind info"
                .to_string()
        })?;
        match inst {
            UnwindInst::PushFrameRegs { .. } => codes.push(code(at, UWOP_PUSH_NONVOL, RBP, &[])),
            UnwindInst::DefineNewFrame {
                offset_downward_to_clobbers: c,
                ..
            } => {
                // The frame offset is 4 bits of 16 bytes.
                if !c.is_multiple_of(16) || *c > 15 * 16 {
                    return Err(format!("ICE: Windows x64 clobber area of {c} bytes"));
                }
                if *c > 0 {
                    codes.push(alloc(at, *c)?);
                }
                codes.push(code(at, UWOP_SET_FPREG, 0, &[]));
                clobbers = Some(*c);
            }
            UnwindInst::StackAlloc { size } => codes.push(alloc(at, *size)?),
            UnwindInst::SaveReg {
                clobber_offset,
                reg,
            } => {
                if clobbers.is_none() {
                    return Err("ICE: Windows x64 register save before the frame is set".into());
                }
                codes.push(save(at, *clobber_offset, *reg)?);
            }
            other => return Err(format!("ICE: Windows x64 unwind info for {other:?}")),
        }
        prologue = prologue.max(at);
    }
    let nodes: usize = codes.iter().map(Vec::len).sum();
    let count = u8::try_from(nodes)
        .map_err(|_| "codegen: prologue too complex for Windows x64 unwind info".to_string())?;
    let frame = clobbers.map_or(0, |c| ((c / 16) as u8) << 4 | RBP);
    let mut out = vec![1, prologue, count, frame];
    for node in codes.iter().rev().flatten() {
        out.extend(node.to_le_bytes());
    }
    // The code array has an even number of slots.
    if nodes % 2 == 1 {
        out.extend([0, 0]);
    }
    Ok(out)
}
