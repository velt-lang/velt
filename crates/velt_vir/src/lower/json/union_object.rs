//! Decoding a union with several object members (union.rs handles the others): the member is
//! named by the discriminant field (`kind`, a literal-typed field with distinct values in every
//! member) or else by the first key that only one member requires, in the order of sema's
//! `object_discriminant` / `required_keys`. The decoder marks the `{`, scans the keys for that
//! one (skipping other values with `skip_lookahead`, which remembers where they end, so a union
//! nested in them finds its own key without scanning again), goes back and decodes the member.

use velt_sema::hir::{LitValue, TyId, TyKind};

use super::union::UnionCx;
use super::{json_quote, Seg, STR};
use crate::lower::rt::Rt;
use crate::lower::{cint, ice, FnLower};
use crate::vir::{BlockId, Local, Operand, Place, Rvalue, Terminator, Ty};

/// How the member of a union of objects is chosen.
enum ObjectChoice {
    /// By the value of this field, which every member has with a distinct literal type.
    Discriminant(String),
    /// By the first of these keys found: key `i` only member `i` has (and requires).
    Keys(Vec<String>),
}

impl ObjectChoice {
    /// What an object with none of the keys was expected to have.
    fn missing(&self) -> String {
        match self {
            ObjectChoice::Discriminant(name) => format!("field {}", json_quote(name)),
            ObjectChoice::Keys(keys) => {
                let list: Vec<String> = keys.iter().map(|k| json_quote(k)).collect();
                format!("object with one of the fields {}", list.join(", "))
            }
        }
    }
}

impl FnLower<'_, '_> {
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

    /// How to choose among the object members `tys`: the discriminant, a field with a literal
    /// type in every member and distinct values, or else one key per member that only it has
    /// (required there).
    fn object_choice(&mut self, tys: &[TyId]) -> ObjectChoice {
        let fields: Vec<Vec<(String, bool)>> =
            tys.iter().map(|t| self.object_field_names(*t)).collect();
        let disc = fields[0].iter().map(|(n, _)| n.clone()).find(|name| {
            let mut seen = vec![];
            for &t in tys {
                match self.field_literal(t, name) {
                    Some(v) if !seen.contains(&v) => seen.push(v),
                    _ => return false,
                }
            }
            true
        });
        if let Some(name) = disc {
            return ObjectChoice::Discriminant(name);
        }
        let keys = fields
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
            .collect();
        ObjectChoice::Keys(keys)
    }

    /// Several object members: find the one the [`ObjectChoice`] names, then decode it.
    pub(super) fn union_object(&mut self, cx: &UnionCx<'_>, objects: &[u32]) {
        let tys: Vec<TyId> = objects.iter().map(|&k| cx.members[k as usize]).collect();
        let choice = self.object_choice(&tys);
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
        self.object_next_key(cx, &ka, head, body, end, broken);
        self.switch_to(body);
        match &choice {
            ObjectChoice::Discriminant(name) => {
                self.object_match_discriminant(cx, &ka, name, &tys, which, found)
            }
            ObjectChoice::Keys(keys) => self.object_match_keys(&ka, keys, which, found),
        }
        self.object_skip_value(cx, ka, head);
        // The object ended without the key.
        self.switch_to(end);
        self.json_fail(cx.ctx, &choice.missing(), cx.fail);
        self.switch_to(found);
        self.object_decode_chosen(cx, objects, mark, which);
    }

    /// The loop head `head`: read the next key into `ka`, on to `body` with a key, `end` at
    /// the object's end, `broken` (which fails) on malformed input.
    fn object_next_key(
        &mut self,
        cx: &UnionCx<'_>,
        ka: &Operand,
        head: BlockId,
        body: BlockId,
        end: BlockId,
        broken: BlockId,
    ) {
        let ro = Operand::Copy(Place::local(cx.r));
        self.goto(head);
        self.switch_to(head);
        let step = self.temp(Ty::U8);
        self.call_rt(
            Rt::JsonNextKey,
            vec![ro, ka.clone()],
            Some(Place::local(step)),
        );
        self.terminate(Terminator::Switch {
            value: Operand::Copy(Place::local(step)),
            cases: vec![(1, body), (0, end)],
            default: broken,
        });
        self.switch_to(broken);
        self.json_fail(cx.ctx, "object", cx.fail);
    }

    /// The key `ka` is the discriminant `name`: its value picks the member (into `which`), and
    /// on to `found`. Otherwise falls through.
    fn object_match_discriminant(
        &mut self,
        cx: &UnionCx<'_>,
        ka: &Operand,
        name: &str,
        tys: &[TyId],
        which: Local,
        found: BlockId,
    ) {
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

    /// The key `ka` is `keys[i]`: member `i` (into `which`), and on to `found`. Otherwise
    /// falls through.
    fn object_match_keys(&mut self, ka: &Operand, keys: &[String], which: Local, found: BlockId) {
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

    /// Not the key we look for: skip its value (remembering where its objects end, so the
    /// lookahead of a union nested in it does not scan it again) and read the next key.
    fn object_skip_value(&mut self, cx: &UnionCx<'_>, ka: Operand, head: BlockId) {
        let ro = Operand::Copy(Place::local(cx.r));
        let ok = self.rt_u8(Rt::JsonSkipLookahead, vec![ro]);
        let (next, skip_bad) = (self.new_block(), self.new_block());
        self.branch(ok, next, skip_bad);
        self.switch_to(skip_bad);
        self.json_prepend(cx.ctx, Seg::Key(ka.clone()));
        self.call_rt(Rt::StrDrop, vec![ka.clone()], None);
        self.json_fail(cx.ctx, "value", cx.fail);
        self.switch_to(next);
        self.call_rt(Rt::StrDrop, vec![ka], None);
        self.goto(head);
    }

    /// Back to the `{` (at `mark`), then decode member `objects[which]`.
    fn object_decode_chosen(
        &mut self,
        cx: &UnionCx<'_>,
        objects: &[u32],
        mark: Local,
        which: Local,
    ) {
        let ro = Operand::Copy(Place::local(cx.r));
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
