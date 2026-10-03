//! Decoding unions (sema's `union_decode_problem` admits only unions this can tell apart, by
//! the same member classification). The next token picks the members to try:
//!
//! - a string: string literal members and string enum members by value, else the `string`
//!   member; a number: numeric literal / enum members by value (looked ahead with
//!   `reader_mark` and read again for the number member), else the number member; `true` /
//!   `false`: bool literals, else the `bool` member; `[`: the array or tuple member;
//! - `{`: the only object member, or else the member named by the discriminant field (`kind`,
//!   found anywhere in the object) or by the first key that only one member requires
//!   (union_object.rs: a lookahead that stays linear overall).
//!
//! Anything else fails with the members' kinds: `expected one of string, number at $.x`.

use velt_sema::hir::{self, LitValue, TyId, TyKind};

use super::{STR, TOKEN_FALSE, TOKEN_NUMBER, TOKEN_STRING, TOKEN_TRUE};
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, ice, FnLower};
use crate::vir::{BinOp, BlockId, Const, Local, Operand, Place, Proj, Rvalue, Terminator, Ty};

const TOKEN_ARRAY_START: u32 = 6;
const TOKEN_OBJECT_START: u32 = 8;

/// The member classification shared with sema (`velt_sema::json::Shape`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Str,
    StrLits,
    Num,
    NumLits,
    Bool,
    BoolLits,
    Array,
    Object,
}

/// A literal a member is matched by: member `k`, the payload value to store (a string enum's
/// member index or a numeric enum's discriminant; `None` for a zero-sized literal type).
struct Lit {
    k: u32,
    payload: Option<i64>,
    value: LitValue,
}

impl FnLower<'_, '_> {
    fn union_shape(&mut self, m: TyId) -> Shape {
        match self.cx.kind(m) {
            TyKind::Str => Shape::Str,
            TyKind::Int(_) | TyKind::Float(_) => Shape::Num,
            TyKind::Bool => Shape::Bool,
            TyKind::Literal(LitValue::Str(_)) => Shape::StrLits,
            TyKind::Literal(LitValue::Bool(_)) => Shape::BoolLits,
            TyKind::Literal(_) => Shape::NumLits,
            TyKind::Array(_) | TyKind::Tuple(_) => Shape::Array,
            TyKind::Adt(d, _) => match self.cx.hir.def(d) {
                hir::Def::Enum(e) if !e.is_union => {
                    if e.variants.iter().any(|v| v.str_value.is_some()) {
                        Shape::StrLits
                    } else {
                        Shape::NumLits
                    }
                }
                _ => Shape::Object,
            },
            _ => Shape::Object,
        }
    }

    /// The literals member `k` (type `m`) is matched by.
    fn union_lits(&mut self, k: u32, m: TyId) -> Vec<Lit> {
        match self.cx.kind(m) {
            TyKind::Literal(value) => vec![Lit {
                k,
                payload: None,
                value,
            }],
            TyKind::Adt(d, _) => match self.cx.hir.def(d) {
                hir::Def::Enum(e) => e
                    .variants
                    .iter()
                    .enumerate()
                    .map(|(i, v)| match &v.str_value {
                        Some(s) => Lit {
                            k,
                            payload: Some(i as i64),
                            value: LitValue::Str(s.clone()),
                        },
                        None => Lit {
                            k,
                            payload: Some(v.discriminant),
                            value: LitValue::Int(hir::IntTy::I64, v.discriminant as i128),
                        },
                    })
                    .collect(),
                _ => vec![],
            },
            _ => vec![],
        }
    }

    /// Decode a union with members other than literal types into `place`.
    pub(super) fn json_read_union(
        &mut self,
        r: Local,
        place: &Place,
        ctx: Local,
        ty: TyId,
        fail: BlockId,
    ) {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            ice("JSON union of a non-ADT type")
        };
        let n = self.cx.enum_def(d).variants.len() as u32;
        let members: Vec<TyId> = (0..n).map(|k| self.cx.variant_tys(ty, k)[0]).collect();
        let shapes: Vec<Shape> = members.iter().map(|m| self.union_shape(*m)).collect();
        let expected = self.union_expected(&members, &shapes);
        let (done, bad) = (self.new_block(), self.new_block());
        let blocks = self.union_peek(r, bad);
        let cx = UnionCx {
            r,
            place,
            ctx,
            ty,
            members: &members,
            expected: &expected,
            done,
            bad,
            fail,
        };
        self.union_by_kind(&cx, &shapes, &blocks);
        // Another kind of value: skip it so a malformed one reports its syntax error.
        self.switch_to(bad);
        self.rt_u8(Rt::JsonSkipValue, vec![Operand::Copy(Place::local(r))]);
        self.json_fail(ctx, &expected, fail);
        self.switch_to(done);
    }

    /// Peek at the next token and switch on its kind: to the blocks returned for a string, a
    /// number, a bool, an array and an object, or to `bad`.
    fn union_peek(&mut self, r: Local, bad: BlockId) -> Vec<BlockId> {
        let blocks: Vec<BlockId> = (0..5).map(|_| self.new_block()).collect();
        let tok = self.temp(Ty::U32);
        let ro = Operand::Copy(Place::local(r));
        self.call_rt(Rt::JsonPeek, vec![ro], Some(Place::local(tok)));
        self.terminate(Terminator::Switch {
            value: Operand::Copy(Place::local(tok)),
            cases: vec![
                (TOKEN_STRING as i128, blocks[0]),
                (TOKEN_NUMBER as i128, blocks[1]),
                (TOKEN_TRUE as i128, blocks[2]),
                (TOKEN_FALSE as i128, blocks[2]),
                (TOKEN_ARRAY_START as i128, blocks[3]),
                (TOKEN_OBJECT_START as i128, blocks[4]),
            ],
            default: bad,
        });
        blocks
    }

    /// The decoder for each kind of token (`blocks`, from [`Self::union_peek`]): the members
    /// of that kind, by `shapes`.
    fn union_by_kind(&mut self, cx: &UnionCx<'_>, shapes: &[Shape], blocks: &[BlockId]) {
        self.switch_to(blocks[0]);
        let lits = self.shape_lits(cx, shapes, Shape::StrLits);
        self.union_string(cx, &lits, first_of(shapes, Shape::Str));

        self.switch_to(blocks[1]);
        let lits = self.shape_lits(cx, shapes, Shape::NumLits);
        self.union_number(cx, &lits, first_of(shapes, Shape::Num));

        self.switch_to(blocks[2]);
        let lits = self.shape_lits(cx, shapes, Shape::BoolLits);
        self.union_bool(cx, &lits, first_of(shapes, Shape::Bool));

        self.switch_to(blocks[3]);
        match first_of(shapes, Shape::Array) {
            Some(k) => self.union_member(cx, k),
            None => self.goto(cx.bad),
        }

        self.switch_to(blocks[4]);
        let objects = members_of(shapes, Shape::Object);
        match objects.as_slice() {
            [] => self.goto(cx.bad),
            [k] => self.union_member(cx, *k),
            _ => self.union_object(cx, &objects),
        }
    }

    /// The literals of the members of shape `want`.
    fn shape_lits(&mut self, cx: &UnionCx<'_>, shapes: &[Shape], want: Shape) -> Vec<Lit> {
        members_of(shapes, want)
            .into_iter()
            .flat_map(|k| self.union_lits(k, cx.members[k as usize]))
            .collect()
    }

    /// `expected` text: the members' kinds and literal values.
    fn union_expected(&mut self, members: &[TyId], shapes: &[Shape]) -> String {
        let mut parts: Vec<String> = vec![];
        for (k, (m, s)) in members.iter().zip(shapes).enumerate() {
            let texts: Vec<String> = match s {
                Shape::Str => vec!["string".into()],
                Shape::Num => vec!["number".into()],
                Shape::Bool => vec!["boolean".into()],
                Shape::Array => vec!["array".into()],
                Shape::Object => vec!["object".into()],
                _ => self
                    .union_lits(k as u32, *m)
                    .iter()
                    .map(|l| super::literal::literal_text(&l.value))
                    .collect(),
            };
            for t in texts {
                if !parts.contains(&t) {
                    parts.push(t);
                }
            }
        }
        match parts.as_slice() {
            [one] => one.clone(),
            _ => format!("one of {}", parts.join(", ")),
        }
    }

    /// Set the tag of the union at `cx.place` to member `k` (and its payload to `payload`).
    fn union_set(&mut self, cx: &UnionCx<'_>, k: u32, payload: Option<Operand>) {
        let view = self.cx.view(cx.ty, k);
        let vp = proj(cx.place, Proj::Cast(view));
        self.assign(
            proj(&vp, Proj::Field(0)),
            Rvalue::Use(cint(k as i128, Ty::U32)),
        );
        if let Some(v) = payload {
            let m = cx.members[k as usize];
            let vt = self.cx.ty(m);
            let v = self.cast_to(v, Ty::I64, vt);
            self.assign(proj(&vp, Proj::Field(1)), Rvalue::Use(v));
        }
    }

    /// Decode member `k` with its own decoder, then continue at `done`.
    pub(super) fn union_member(&mut self, cx: &UnionCx<'_>, k: u32) {
        let m = cx.members[k as usize];
        let view = self.cx.view(cx.ty, k);
        let vp = proj(cx.place, Proj::Cast(view));
        self.assign(
            proj(&vp, Proj::Field(0)),
            Rvalue::Use(cint(k as i128, Ty::U32)),
        );
        if self.cx.ty(m) == Ty::Unit {
            ice("zero-sized union member decoded as a value");
        }
        self.json_read(cx.r, &proj(&vp, Proj::Field(1)), cx.ctx, m, cx.fail);
        self.goto(cx.done);
    }

    /// Store literal `lit` (it matched) and continue at `done`.
    fn union_lit_hit(&mut self, cx: &UnionCx<'_>, lit: &Lit) {
        let payload = lit.payload.map(|p| cint(p as i128, Ty::I64));
        self.union_set(cx, lit.k, payload);
        self.goto(cx.done);
    }

    fn union_string(&mut self, cx: &UnionCx<'_>, lits: &[Lit], member: Option<u32>) {
        if lits.is_empty() {
            match member {
                Some(k) => self.union_member(cx, k),
                None => self.goto(cx.bad),
            }
            return;
        }
        let s = self.temp(STR);
        let sa = self.addr(Place::local(s));
        let ro = Operand::Copy(Place::local(cx.r));
        self.json_expect(
            Rt::JsonReadString,
            vec![ro, sa.clone()],
            cx.ctx,
            cx.expected,
            cx.fail,
        );
        for lit in lits {
            let LitValue::Str(text) = &lit.value else {
                continue;
            };
            let l = self.str_lit(text);
            let la = self.operand_addr(l, STR);
            let eq = self.rt_u8(Rt::StrEq, vec![sa.clone(), la]);
            let (hit, miss) = (self.new_block(), self.new_block());
            self.branch(eq, hit, miss);
            self.switch_to(hit);
            self.call_rt(Rt::StrDrop, vec![sa.clone()], None);
            self.union_lit_hit(cx, lit);
            self.switch_to(miss);
        }
        match member {
            // Any other string: the `string` member takes it.
            Some(k) => {
                let view = self.cx.view(cx.ty, k);
                let vp = proj(cx.place, Proj::Cast(view));
                self.assign(
                    proj(&vp, Proj::Field(0)),
                    Rvalue::Use(cint(k as i128, Ty::U32)),
                );
                self.assign(
                    proj(&vp, Proj::Field(1)),
                    Rvalue::Use(Operand::Copy(Place::local(s))),
                );
                self.goto(cx.done);
            }
            None => {
                self.call_rt(Rt::StrDrop, vec![sa], None);
                self.json_fail(cx.ctx, cx.expected, cx.fail);
            }
        }
    }

    fn union_number(&mut self, cx: &UnionCx<'_>, lits: &[Lit], member: Option<u32>) {
        if lits.is_empty() {
            match member {
                Some(k) => self.union_member(cx, k),
                None => self.goto(cx.bad),
            }
            return;
        }
        // Read ahead as f64 against the literals; the number member reads it again (an
        // integer member needs the exact digits).
        let ro = Operand::Copy(Place::local(cx.r));
        let mark = self.temp(Ty::U64);
        self.call_rt(Rt::JsonMark, vec![ro.clone()], Some(Place::local(mark)));
        let f = self.temp(Ty::F64);
        let fa = self.addr(Place::local(f));
        self.json_expect(
            Rt::JsonReadF64,
            vec![ro.clone(), fa],
            cx.ctx,
            cx.expected,
            cx.fail,
        );
        for lit in lits {
            let v = match &lit.value {
                LitValue::Int(_, n) => *n as f64,
                LitValue::Float(_, bits) => f64::from_bits(*bits),
                _ => continue,
            };
            let c = Operand::Const(Const::Float(v), Ty::F64);
            let eq = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Eq, Operand::Copy(Place::local(f)), c),
            );
            let (hit, miss) = (self.new_block(), self.new_block());
            self.branch(eq, hit, miss);
            self.switch_to(hit);
            self.union_lit_hit(cx, lit);
            self.switch_to(miss);
        }
        match member {
            Some(k) => {
                self.call_rt(
                    Rt::JsonReset,
                    vec![ro, Operand::Copy(Place::local(mark))],
                    None,
                );
                self.union_member(cx, k);
            }
            None => self.json_fail(cx.ctx, cx.expected, cx.fail),
        }
    }

    fn union_bool(&mut self, cx: &UnionCx<'_>, lits: &[Lit], member: Option<u32>) {
        if lits.is_empty() {
            match member {
                Some(k) => self.union_member(cx, k),
                None => self.goto(cx.bad),
            }
            return;
        }
        let ro = Operand::Copy(Place::local(cx.r));
        let b = self.temp(Ty::U8);
        let ba = self.addr(Place::local(b));
        self.json_expect(Rt::JsonReadBool, vec![ro, ba], cx.ctx, cx.expected, cx.fail);
        for lit in lits {
            let LitValue::Bool(want) = lit.value else {
                continue;
            };
            let eq = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(
                    BinOp::Eq,
                    Operand::Copy(Place::local(b)),
                    cint(want as i128, Ty::U8),
                ),
            );
            let (hit, miss) = (self.new_block(), self.new_block());
            self.branch(eq, hit, miss);
            self.switch_to(hit);
            self.union_lit_hit(cx, lit);
            self.switch_to(miss);
        }
        match member {
            Some(k) => {
                let view = self.cx.view(cx.ty, k);
                let vp = proj(cx.place, Proj::Cast(view));
                self.assign(
                    proj(&vp, Proj::Field(0)),
                    Rvalue::Use(cint(k as i128, Ty::U32)),
                );
                let v = Rvalue::Binary(BinOp::Ne, Operand::Copy(Place::local(b)), cint(0, Ty::U8));
                self.assign(proj(&vp, Proj::Field(1)), v);
                self.goto(cx.done);
            }
            None => self.json_fail(cx.ctx, cx.expected, cx.fail),
        }
    }
}

/// The members of shape `want`.
fn members_of(shapes: &[Shape], want: Shape) -> Vec<u32> {
    (0..shapes.len() as u32)
        .filter(|&k| shapes[k as usize] == want)
        .collect()
}

/// The first member of shape `want`.
fn first_of(shapes: &[Shape], want: Shape) -> Option<u32> {
    members_of(shapes, want).first().copied()
}

/// What every part of a union decoder needs.
pub(super) struct UnionCx<'a> {
    pub(super) r: Local,
    pub(super) place: &'a Place,
    pub(super) ctx: Local,
    pub(super) ty: TyId,
    pub(super) members: &'a [TyId],
    pub(super) expected: &'a str,
    /// Decoded: continue here.
    pub(super) done: BlockId,
    /// A kind of value no member takes.
    pub(super) bad: BlockId,
    pub(super) fail: BlockId,
}
