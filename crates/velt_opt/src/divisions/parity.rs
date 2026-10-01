//! Known low zero bits of values within one block, by local value numbering: `x * (x + 1)` is
//! even whatever `x` is, and stays even when the multiplication wraps (wrapping keeps the low
//! bits). Spectral-norm's `((i + j) * (i + j + 1)) / 2` computes `i + j` twice, so equal
//! expressions over unchanged locals get the same value number.

use std::collections::HashMap;

use velt_vir::vir::{BinOp, Const, Local, Operand, Rvalue, Stmt, Ty};

use crate::locals::Usage;

/// A value number.
type Vn = u32;

/// How a value was computed (the key that makes equal expressions share a number).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Expr {
    /// Value and bit width.
    Const(i128, u32),
    /// Operator (`BinOp as u8`: `BinOp` is not `Hash`) and operands.
    Binary(u8, Vn, Vn),
    /// Anything else: its own number.
    Opaque(u32),
}

/// Value numbers of one block, filled statement by statement.
pub(super) struct Parity<'a> {
    usage: &'a Usage,
    local: HashMap<Local, Vn>,
    known: HashMap<Expr, Vn>,
    exprs: Vec<Expr>,
    /// Known trailing zero bits per value number.
    zeros: Vec<u32>,
}

impl<'a> Parity<'a> {
    /// Empty numbering at the start of a block.
    pub fn new(usage: &'a Usage) -> Self {
        Parity {
            usage,
            local: HashMap::new(),
            known: HashMap::new(),
            exprs: vec![],
            zeros: vec![],
        }
    }

    /// Known trailing zero bits of `op` (of type `ty`) at the current point.
    pub fn zeros(&mut self, op: &Operand, ty: Ty) -> u32 {
        let vn = self.operand(op, ty);
        self.zeros[vn as usize]
    }

    /// Record statement `s`.
    pub fn step(&mut self, s: &Stmt, ty_of: impl Fn(Local) -> Ty) {
        let Stmt::Assign(dst, rv) = s else { return };
        if !dst.proj.is_empty() || !self.usage.is_register(dst.local) {
            return;
        }
        let ty = ty_of(dst.local);
        let vn = match rv {
            Rvalue::Use(op) => self.operand(op, ty),
            Rvalue::Binary(op, a, b) if ty.is_int() => {
                let (x, y) = (self.operand(a, ty), self.operand(b, ty));
                let (x, y) = match op {
                    BinOp::Add | BinOp::Mul | BinOp::BitAnd if y < x => (y, x),
                    _ => (x, y),
                };
                let z = self.binary_zeros(*op, x, y, ty);
                self.number(Expr::Binary(*op as u8, x, y), z)
            }
            _ => self.opaque(),
        };
        self.local.insert(dst.local, vn);
    }

    fn operand(&mut self, op: &Operand, ty: Ty) -> Vn {
        match op {
            Operand::Const(Const::Int(v), _) => {
                let bits = ty.scalar_size().unwrap_or(8) * 8;
                let z = if *v == 0 {
                    bits
                } else {
                    v.trailing_zeros().min(bits)
                };
                self.number(Expr::Const(*v, bits), z)
            }
            Operand::Copy(p) if p.proj.is_empty() && self.usage.is_register(p.local) => {
                match self.local.get(&p.local) {
                    Some(&vn) => vn,
                    None => {
                        let vn = self.opaque();
                        self.local.insert(p.local, vn);
                        vn
                    }
                }
            }
            _ => self.opaque(),
        }
    }

    fn number(&mut self, e: Expr, zeros: u32) -> Vn {
        if let Some(&vn) = self.known.get(&e) {
            return vn;
        }
        let vn = self.exprs.len() as Vn;
        self.exprs.push(e.clone());
        self.zeros.push(zeros);
        self.known.insert(e, vn);
        vn
    }

    fn opaque(&mut self) -> Vn {
        let e = Expr::Opaque(self.exprs.len() as u32);
        self.number(e, 0)
    }

    fn binary_zeros(&self, op: BinOp, x: Vn, y: Vn, ty: Ty) -> u32 {
        let bits = ty.scalar_size().unwrap_or(8) * 8;
        let (zx, zy) = (self.zeros[x as usize], self.zeros[y as usize]);
        match op {
            BinOp::Add | BinOp::Sub => zx.min(zy),
            BinOp::Mul => {
                let consecutive = self.differ_by_odd(x, y) || self.differ_by_odd(y, x);
                (zx + zy).max(u32::from(consecutive)).min(bits)
            }
            BinOp::BitAnd => zx.max(zy),
            BinOp::Shl => match self.exprs[y as usize] {
                Expr::Const(k, _) if (0..i128::from(bits)).contains(&k) => {
                    (zx + k as u32).min(bits)
                }
                _ => 0,
            },
            _ => 0,
        }
    }

    /// Whether `b` is `a ± odd constant` (so one of `a`, `b` is even).
    fn differ_by_odd(&self, a: Vn, b: Vn) -> bool {
        let Expr::Binary(op, p, q) = self.exprs[b as usize] else {
            return false;
        };
        let (add, sub) = (op == BinOp::Add as u8, op == BinOp::Sub as u8);
        let odd = |vn: Vn| matches!(self.exprs[vn as usize], Expr::Const(k, _) if k % 2 != 0);
        ((add || sub) && p == a && odd(q)) || (add && q == a && odd(p))
    }
}
