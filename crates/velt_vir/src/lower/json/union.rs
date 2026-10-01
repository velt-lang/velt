//! Decoding unions (sema's `union_decode_problem` admits only unions this can tell apart, by
//! the same member classification). The next token picks the members to try:
//!
//! - a string: string literal members and string enum members by value, else the `string`
//!   member; a number: numeric literal / enum members by value (looked ahead with
//!   `reader_mark` and read again for the number member), else the number member; `true` /
//!   `false`: bool literals, else the `bool` member; `[`: the array or tuple member;
//! - `{`: the only object member, or else the member named by the discriminant field (`kind`,
//!   found anywhere in the object) or by the first key that only one member requires. The
//!   decoder looks ahead for that key, goes back to the `{` and decodes the chosen member.
//!
//! Anything else fails with the members' kinds: `expected one of string, number at $.x`.

use velt_sema::hir::{self, LitValue, TyId, TyKind};

use super::{json_quote, Seg, STR, TOKEN_FALSE, TOKEN_NUMBER, TOKEN_STRING, TOKEN_TRUE};
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
        let pick =
            |want: Shape| -> Vec<u32> { (0..n).filter(|&k| shapes[k as usize] == want).collect() };
        let expected = self.union_expected(&members, &shapes);
        let ro = Operand::Copy(Place::local(r));
        let (done, bad) = (self.new_block(), self.new_block());
        let blocks: Vec<BlockId> = (0..5).map(|_| self.new_block()).collect();
        let tok = self.temp(Ty::U32);
        self.call_rt(Rt::JsonPeek, vec![ro.clone()], Some(Place::local(tok)));
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

        self.switch_to(blocks[0]);
        let lits: Vec<Lit> = pick(Shape::StrLits)
            .into_iter()
            .flat_map(|k| self.union_lits(k, members[k as usize]))
            .collect();
        self.union_string(&cx, &lits, pick(Shape::Str).first().copied());

        self.switch_to(blocks[1]);
        let lits: Vec<Lit> = pick(Shape::NumLits)
            .into_iter()
            .flat_map(|k| self.union_lits(k, members[k as usize]))
            .collect();
        self.union_number(&cx, &lits, pick(Shape::Num).first().copied());

        self.switch_to(blocks[2]);
        let lits: Vec<Lit> = pick(Shape::BoolLits)
            .into_iter()
            .flat_map(|k| self.union_lits(k, members[k as usize]))
            .collect();
        self.union_bool(&cx, &lits, pick(Shape::Bool).first().copied());

        self.switch_to(blocks[3]);
        match pick(Shape::Array).first() {
            Some(&k) => self.union_member(&cx, k),
            None => self.goto(bad),
        }

        self.switch_to(blocks[4]);
        let objects = pick(Shape::Object);
        match objects.as_slice() {
            [] => self.goto(bad),
            [k] => self.union_member(&cx, *k),
            _ => self.union_object(&cx, &objects),
        }

        // Another kind of value: skip it so a malformed one reports its syntax error.
        self.switch_to(bad);
        self.rt_u8(Rt::JsonSkipValue, vec![ro]);
        self.json_fail(ctx, &expected, fail);
        self.switch_to(done);
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
    fn union_member(&mut self, cx: &UnionCx<'_>, k: u32) {
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

    /// The literal value of field `name` in object type `t`, if it has one.
    fn field_literal(&mut self, t: TyId, name: &str) -> Option<LitValue> {
        let TyKind::Adt(d, _) = self.cx.kind(t) else {
            return None;
        };
        let i = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .position(|f| f.name == name)?;
        let ft = self.cx.adt_field_tys(t)[i];
        match self.cx.kind(ft) {
            TyKind::Literal(l) => Some(l),
            _ => None,
        }
    }

    /// `(name, required)` of each field of object type `t`.
    fn object_field_names(&mut self, t: TyId) -> Vec<(String, bool)> {
        let TyKind::Adt(d, _) = self.cx.kind(t) else {
            return vec![];
        };
        let names: Vec<String> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .map(|f| f.name.clone())
            .collect();
        let tys = self.cx.adt_field_tys(t);
        names
            .into_iter()
            .zip(tys)
            .map(|(n, ft)| (n, !matches!(self.cx.kind(ft), TyKind::Option(_))))
            .collect()
    }

    /// Several object members: find the one named by the discriminant field, or else by a key
    /// only it requires (sema's `object_discriminant` / `required_keys`, in the same order).
    fn union_object(&mut self, cx: &UnionCx<'_>, objects: &[u32]) {
        let tys: Vec<TyId> = objects.iter().map(|&k| cx.members[k as usize]).collect();
        let fields: Vec<Vec<(String, bool)>> =
            tys.iter().map(|t| self.object_field_names(*t)).collect();
        // The discriminant: a field with a literal type in every member, values distinct.
        let disc = fields[0].iter().map(|(n, _)| n.clone()).find(|name| {
            let mut seen = vec![];
            for &t in &tys {
                match self.field_literal(t, name) {
                    Some(v) if !seen.contains(&v) => seen.push(v),
                    _ => return false,
                }
            }
            true
        });
        // Otherwise one key per member that only it has (required there).
        let keys: Vec<String> = match &disc {
            Some(_) => vec![],
            None => fields
                .iter()
                .enumerate()
                .map(|(i, fs)| {
                    fs.iter()
                        .find(|(n, req)| {
                            *req && fields
                                .iter()
                                .enumerate()
                                .all(|(j, o)| j == i || o.iter().all(|(on, _)| on != n))
                        })
                        .map(|(n, _)| n.clone())
                        .unwrap_or_else(|| ice("JSON union members without distinguishing keys"))
                })
                .collect(),
        };
        let ro = Operand::Copy(Place::local(cx.r));
        let mark = self.temp(Ty::U64);
        self.call_rt(Rt::JsonMark, vec![ro.clone()], Some(Place::local(mark)));
        self.json_expect(
            Rt::JsonObjectStart,
            vec![ro.clone()],
            cx.ctx,
            "object",
            cx.fail,
        );
        // `which` is the position in `objects` of the chosen member.
        let which = self.temp(Ty::U32);
        let key = self.temp(STR);
        let ka = self.addr(Place::local(key));
        let (head, body, end, broken, found) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        self.goto(head);
        self.switch_to(head);
        let step = self.temp(Ty::U8);
        self.call_rt(
            Rt::JsonNextKey,
            vec![ro.clone(), ka.clone()],
            Some(Place::local(step)),
        );
        self.terminate(Terminator::Switch {
            value: Operand::Copy(Place::local(step)),
            cases: vec![(1, body), (0, end)],
            default: broken,
        });
        self.switch_to(broken);
        self.json_fail(cx.ctx, "object", cx.fail);

        self.switch_to(body);
        match &disc {
            Some(name) => {
                let l = self.str_lit(name);
                let la = self.operand_addr(l, STR);
                let eq = self.rt_u8(Rt::StrEq, vec![ka.clone(), la]);
                let (hit, other) = (self.new_block(), self.new_block());
                self.branch(eq, hit, other);
                self.switch_to(hit);
                self.call_rt(Rt::StrDrop, vec![ka.clone()], None);
                let alts: Vec<LitValue> = tys
                    .iter()
                    .map(|t| {
                        self.field_literal(*t, name)
                            .unwrap_or_else(|| ice("discriminant"))
                    })
                    .collect();
                let tag_fail = self.new_block();
                let idx = self.json_match_choice(cx.r, cx.ctx, &alts, tag_fail);
                self.assign(
                    Place::local(which),
                    Rvalue::Use(Operand::Copy(Place::local(idx))),
                );
                self.goto(found);
                self.switch_to(tag_fail);
                self.json_prepend(cx.ctx, Seg::Field(name));
                self.goto(cx.fail);
                self.switch_to(other);
            }
            None => {
                for (i, k) in keys.iter().enumerate() {
                    let l = self.str_lit(k);
                    let la = self.operand_addr(l, STR);
                    let eq = self.rt_u8(Rt::StrEq, vec![ka.clone(), la]);
                    let (hit, miss) = (self.new_block(), self.new_block());
                    self.branch(eq, hit, miss);
                    self.switch_to(hit);
                    self.call_rt(Rt::StrDrop, vec![ka.clone()], None);
                    self.assign(Place::local(which), Rvalue::Use(cint(i as i128, Ty::U32)));
                    self.goto(found);
                    self.switch_to(miss);
                }
            }
        }
        // Not the key we look for: skip its value.
        let ok = self.rt_u8(Rt::JsonSkipValue, vec![ro.clone()]);
        let (next, skip_bad) = (self.new_block(), self.new_block());
        self.branch(ok, next, skip_bad);
        self.switch_to(skip_bad);
        self.json_prepend(cx.ctx, Seg::Key(ka.clone()));
        self.call_rt(Rt::StrDrop, vec![ka.clone()], None);
        self.json_fail(cx.ctx, "value", cx.fail);
        self.switch_to(next);
        self.call_rt(Rt::StrDrop, vec![ka], None);
        self.goto(head);

        // The object ended without the key.
        self.switch_to(end);
        let missing = match &disc {
            Some(name) => format!("field {}", json_quote(name)),
            None => {
                let list: Vec<String> = keys.iter().map(|k| json_quote(k)).collect();
                format!("object with one of the fields {}", list.join(", "))
            }
        };
        self.json_fail(cx.ctx, &missing, cx.fail);

        // Back to the `{`, then decode the chosen member.
        self.switch_to(found);
        self.call_rt(
            Rt::JsonReset,
            vec![ro, Operand::Copy(Place::local(mark))],
            None,
        );
        let blocks: Vec<BlockId> = objects.iter().map(|_| self.new_block()).collect();
        self.terminate(Terminator::Switch {
            value: Operand::Copy(Place::local(which)),
            cases: blocks
                .iter()
                .enumerate()
                .map(|(i, b)| (i as i128, *b))
                .collect(),
            default: cx.bad,
        });
        for (&k, b) in objects.iter().zip(blocks) {
            self.switch_to(b);
            self.union_member(cx, k);
        }
    }
}

/// What every part of a union decoder needs.
struct UnionCx<'a> {
    r: Local,
    place: &'a Place,
    ctx: Local,
    ty: TyId,
    members: &'a [TyId],
    expected: &'a str,
    /// Decoded: continue here.
    done: BlockId,
    /// A kind of value no member takes.
    bad: BlockId,
    fail: BlockId,
}
