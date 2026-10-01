//! Byte-addressed memory for the interpreter: three regions at fixed, far-apart base
//! addresses (so null and wild pointers trap instead of aliasing something):
//! read-only statics, a stack of frames (every VIR local lives here), and a bump-allocated heap
//! for hosts that implement allocation externs. Scalars are little endian, as on all targets.

use velt_vir::vir::Ty;

use super::Trap;

const STATIC_BASE: u64 = 0x0001_0000;
const STACK_BASE: u64 = 0x1_0000_0000;
const HEAP_BASE: u64 = 0x2_0000_0000;
/// Stack size limit; deep recursion traps with `StackOverflow` instead of exhausting RAM.
const STACK_LIMIT: usize = 64 << 20;
/// Fill for fresh stack memory: reading it before writing gives recognizable garbage.
const UNINIT: u8 = 0xAA;

/// Interpreter memory.
#[derive(Debug, Default)]
pub struct Memory {
    statics: Vec<u8>,
    stack: Vec<u8>,
    heap: Vec<u8>,
}

/// A position in the stack to release back to (see `Memory::stack_mark`).
#[derive(Clone, Copy, Debug)]
pub struct StackMark(usize);

fn align_up(n: usize, align: u32) -> usize {
    let a = align.max(1) as usize;
    n.div_ceil(a) * a
}

impl Memory {
    /// Append a read-only static; returns its address.
    pub fn add_static(&mut self, bytes: &[u8], align: u32) -> u64 {
        let start = align_up(self.statics.len(), align);
        self.statics.resize(start, 0);
        self.statics.extend_from_slice(bytes);
        STATIC_BASE + start as u64
    }

    /// Overwrite bytes inside the static region (relocations are patched in at load time,
    /// before execution makes statics read-only).
    pub fn patch_static(&mut self, addr: u64, data: &[u8]) -> Result<(), Trap> {
        let off = addr
            .checked_sub(STATIC_BASE)
            .filter(|off| off + data.len() as u64 <= self.statics.len() as u64)
            .ok_or(Trap::BadAddress(addr))? as usize;
        self.statics[off..off + data.len()].copy_from_slice(data);
        Ok(())
    }

    /// Allocate `size` bytes of stack.
    pub fn alloc_stack(&mut self, size: u32, align: u32) -> Result<u64, Trap> {
        let start = align_up(self.stack.len(), align);
        let end = start + size as usize;
        if end > STACK_LIMIT {
            return Err(Trap::StackOverflow);
        }
        self.stack.resize(end, UNINIT);
        Ok(STACK_BASE + start as u64)
    }

    /// Current stack position.
    pub fn stack_mark(&self) -> StackMark {
        StackMark(self.stack.len())
    }

    /// Free everything allocated on the stack since `mark`.
    pub fn stack_release(&mut self, mark: StackMark) {
        self.stack.truncate(mark.0);
    }

    /// Allocate zeroed heap memory (never freed; programs under test are small).
    pub fn alloc_heap(&mut self, size: u64, align: u64) -> Result<u64, Trap> {
        let start = align_up(self.heap.len(), align.clamp(1, 4096) as u32);
        let end = start
            .checked_add(usize::try_from(size).map_err(|_| Trap::BadAddress(size))?)
            .filter(|&e| e <= 1 << 30)
            .ok_or(Trap::BadAddress(size))?;
        self.heap.resize(end, 0);
        Ok(HEAP_BASE + start as u64)
    }

    /// Locate `len` bytes at `addr`: (region, offset). Statics are only readable.
    fn locate(&self, addr: u64, len: u64, write: bool) -> Result<(u8, usize), Trap> {
        let regions: [(u64, usize, bool); 3] = [
            (STATIC_BASE, self.statics.len(), false),
            (STACK_BASE, self.stack.len(), true),
            (HEAP_BASE, self.heap.len(), true),
        ];
        for (i, (base, size, writable)) in regions.into_iter().enumerate() {
            let Some(offset) = addr.checked_sub(base) else {
                continue;
            };
            let in_bounds = offset
                .checked_add(len)
                .is_some_and(|end| end <= size as u64);
            if in_bounds && (writable || !write) {
                return Ok((i as u8, offset as usize));
            }
        }
        Err(Trap::BadAddress(addr))
    }

    /// Read `len` bytes.
    pub fn read(&self, addr: u64, len: u64) -> Result<&[u8], Trap> {
        if len == 0 {
            return Ok(&[]);
        }
        let (region, off) = self.locate(addr, len, false)?;
        let bytes = match region {
            0 => &self.statics,
            1 => &self.stack,
            _ => &self.heap,
        };
        Ok(&bytes[off..off + len as usize])
    }

    /// Write bytes.
    pub fn write(&mut self, addr: u64, data: &[u8]) -> Result<(), Trap> {
        if data.is_empty() {
            return Ok(());
        }
        let (region, off) = self.locate(addr, data.len() as u64, true)?;
        let bytes = match region {
            1 => &mut self.stack,
            _ => &mut self.heap,
        };
        bytes[off..off + data.len()].copy_from_slice(data);
        Ok(())
    }

    /// Read a scalar of type `ty` as raw bits (zero-extended to 64 bits).
    pub fn read_scalar(&self, addr: u64, ty: Ty) -> Result<u64, Trap> {
        let size = ty.scalar_size().unwrap_or(0);
        let mut buf = [0u8; 8];
        buf[..size as usize].copy_from_slice(self.read(addr, u64::from(size))?);
        Ok(u64::from_le_bytes(buf))
    }

    /// Write the low bytes of `bits` as a scalar of type `ty`.
    pub fn write_scalar(&mut self, addr: u64, ty: Ty, bits: u64) -> Result<(), Trap> {
        let size = ty.scalar_size().unwrap_or(0) as usize;
        self.write(addr, &bits.to_le_bytes()[..size])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_and_bounds() {
        let mut m = Memory::default();
        let s = m.add_static(b"hi", 1);
        assert_eq!(m.read(s, 2).unwrap(), b"hi");
        assert!(m.write(s, b"x").is_err(), "statics are read-only");
        m.patch_static(s + 1, b"o").unwrap();
        assert_eq!(m.read(s, 2).unwrap(), b"ho");
        assert!(m.patch_static(s + 2, b"!").is_err());
        let mark = m.stack_mark();
        let a = m.alloc_stack(8, 8).unwrap();
        m.write_scalar(a, Ty::U32, 0xDEAD_BEEF).unwrap();
        assert_eq!(m.read_scalar(a, Ty::U16).unwrap(), 0xBEEF);
        m.stack_release(mark);
        assert_eq!(m.read_scalar(a, Ty::U8), Err(Trap::BadAddress(a)));
        assert_eq!(m.read_scalar(0, Ty::U8), Err(Trap::BadAddress(0)));
        let h = m.alloc_heap(16, 8).unwrap();
        assert_eq!(m.read_scalar(h + 8, Ty::I64).unwrap(), 0);
    }
}
