//! Intersection types of object types, `A & B` (design on #384): resolved here to one ordinary
//! anonymous object type, the type the user could write out, so nothing after sema sees them.
//!
//! - Fields are those of every operand, in TypeScript's order: `A`'s, then `B`'s new ones.
//! - A field in both gets the intersection of its types: identical types stay, object types
//!   merge, `T | null` & `U | null` is `(T & U) | null` (a field is required when any operand
//!   requires it), a literal & its base type is the literal; it is `readonly` only when every
//!   operand that has it says so.
//! - Unions distribute: `(A | B) & C` is `(A & C) | (B & C)`, without the members that have no
//!   value (`{ kind: "a" } & { kind: "b" }`), so `& { kind: "a" }` picks a member.
//! - A result with no value at all is an error, not a silent `never`.
//!
//! Operands are object types: anonymous ones, field-only interfaces, utility type results,
//! other intersections, and unions of these (with `null`). Classes, structs, interfaces with
//! methods, arrays and function types are errors; a primitive with object types is a branded
//! type (`crate::brands`). `any`, `unknown` and `object` parts intersect as in TypeScript.
//! A part that is a type parameter makes the deferred `TyOp::Intersect` (#350), reduced here
//! once the type arguments are known (`Ctx::reduce_op`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::anon::ShapeField;
use crate::ctx::Ctx;
use crate::hir::{AdtKind, TyId, TyKind, TyOp};
use crate::resolve::TyEnv;

/// How deep field types may nest before an intersection gives up (recursive object types).
const MAX_DEPTH: u32 = 32;

impl Ctx<'_> {
    /// `A & B & …` as written in `t`.
    pub(crate) fn resolve_intersection(
        &mut self,
        t: &ast::TypeExpr,
        parts: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> TyId {
        // An interface made from the parts names its type parameters as written here.
        let names = match self.display_params.len() < env.params.len() {
            true => Some(std::mem::replace(
                &mut self.display_params,
                env.params.clone(),
            )),
            false => None,
        };
        let r = self.resolve_intersection_in(t, parts, env);
        if let Some(names) = names {
            self.display_params = names;
        }
        r
    }

    fn resolve_intersection_in(
        &mut self,
        t: &ast::TypeExpr,
        parts: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> TyId {
        // In an alias expanded at known arguments, the parts are those arguments' types. At
        // arguments that are still type parameters (`Signal<{ value: V }>` in a generic
        // function) the parts stay in the alias's parameters, which the alias substitutes once:
        // substituting here too would replace `V` (the function's first parameter) by the
        // alias's first argument again.
        let known = !env.args.iter().any(|a| self.has_type_params(*a));
        let mut operands: Vec<(TyId, Span)> = vec![];
        for p in parts {
            let ty = self.resolve_type(p, env);
            let ty = match known {
                true => self.subst(ty, &env.args),
                false => ty,
            };
            if ty == self.ty.error {
                return ty;
            }
            // A part that is the alias's own parameter (`T` of `type Tagged<T> = T & { … }`)
            // is the argument written where the alias is used: its errors are reported there.
            let own_param = written_name(p).is_some_and(|n| env.params.iter().any(|q| q == n));
            let at = match (own_param, self.type_level.alias_site) {
                (true, Some(site)) => site,
                _ => p.span,
            };
            operands.push((ty, at));
        }
        // A part written as an alias's type parameter (`T` in `type Tagged<T> = T & { … }`)
        // is shown as its argument.
        let names: Vec<Option<String>> = parts
            .iter()
            .map(|p| written_name(p).filter(|n| !env.params.iter().any(|q| q == n)))
            .map(|n| n.map(str::to_string))
            .collect();
        if operands.iter().any(|&(o, _)| self.deferred_part(o)) {
            return self.deferred_intersection(operands, &names, env.module, t.span);
        }
        if let Some(view) = self.record_intersection(&operands, env.module, t.span) {
            return view;
        }
        self.intersect(operands, names, env.module, t.span)
    }

    /// The function type operands of an intersection with object types made callable object
    /// types (`crate::callables`); false (reported at `span`) when there is more than one
    /// function type (overloads, not supported).
    fn callable_operands(
        &mut self,
        operands: &mut [(TyId, Span)],
        module: usize,
        span: Span,
    ) -> bool {
        let is_fn = |cx: &Self, t: TyId| matches!(cx.ty.kind(t), TyKind::FnPtr { .. });
        let fns: Vec<TyId> = operands
            .iter()
            .map(|&(o, _)| o)
            .filter(|&o| is_fn(self, o))
            .collect();
        if fns.is_empty() || operands.len() < 2 {
            return true;
        }
        let mut calls: Vec<TyId> = fns.clone();
        for &(o, _) in operands.iter() {
            if let Some(c) = self.call_type(o) {
                calls.push(c);
            }
        }
        calls.dedup();
        if fns.len() == operands.len() || calls.iter().any(|&c| c != calls[0]) {
            self.error(
                Diagnostic::error(
                    "intersections of function types (overloads) are not supported",
                    span,
                )
                .with_note("declare one function type with the parameter types every call needs"),
            );
            return false;
        }
        for op in operands.iter_mut() {
            if is_fn(self, op.0) {
                op.0 = self.callable_part(op.0, module);
            }
        }
        true
    }

    /// Is `t` an intersection's part that defers it (#350): a type parameter or a deferred type,
    /// also with `null` (`T | null`).
    pub(crate) fn deferred_part(&self, t: TyId) -> bool {
        let t = self.ty.opt_payload(t).unwrap_or(t);
        matches!(self.ty.kind(t), TyKind::Param(_) | TyKind::Op(..))
    }

    /// `t`, a generic alias's body at its arguments, with an intersection the arguments made
    /// known reduced: `Signal<{ value: V }>` (`T & { $set(v: T): void }`) in a generic function
    /// is the object type `{ value: V; $set(v: { value: V }): void }`, as if written out.
    pub(crate) fn reduce_known_intersection(&mut self, t: TyId) -> TyId {
        match self.reducible_now(t) {
            true => self.reduce_op(t),
            false => t,
        }
    }

    /// Is `t` a deferred intersection none of whose parts is a type parameter or deferred (its
    /// fields may mention type parameters): it can be reduced to an object type now.
    pub(crate) fn reducible_now(&self, t: TyId) -> bool {
        match self.ty.kind(t) {
            TyKind::Op(TyOp::Intersect | TyOp::Spread, parts) => {
                !parts.iter().any(|&p| self.deferred_part(p))
            }
            _ => false,
        }
    }

    /// `T & { … }` with a part that is a type parameter: the deferred `TyOp::Intersect`, reduced
    /// by [`intersect`](Self::intersect) once its parts are known (`Ctx::reduce_op`). The parts
    /// known now are checked here, so `T & C` with a class `C` is reported where it is written.
    fn deferred_intersection(
        &mut self,
        operands: Vec<(TyId, Span)>,
        names: &[Option<String>],
        module: usize,
        span: Span,
    ) -> TyId {
        for (&(ty, at), name) in operands.iter().zip(names) {
            // A function type makes a function with properties once the parts are known.
            if self.deferred_part(ty)
                || matches!(self.ty.kind(ty), TyKind::FnPtr { .. })
                || self.brand_operand(ty)
                || self.dynamic(ty).is_some()
                || self.has_type_params_at_head(ty)
            {
                continue;
            }
            if !self.intersectable(ty, at, name.as_deref()) {
                return self.ty.error;
            }
        }
        let args: Vec<TyId> = operands.iter().map(|&(o, _)| o).collect();
        let op = self.ty.intern(TyKind::Op(TyOp::Intersect, args));
        self.type_level.op_sites.entry(op).or_insert(span);
        self.type_level.op_modules.entry(op).or_insert(module);
        op
    }

    /// `t` with each deferred intersection in the form it has under the bounds of the function
    /// being checked: its parts flattened and deduplicated, and its `object` parts dropped when
    /// a part is a type parameter whose values are objects (`object_params`, `T extends
    /// object`), so `T & object` is `T` there, as in TypeScript.
    pub(crate) fn bounded_form(&mut self, t: TyId, object_params: &[u32], depth: u32) -> TyId {
        if depth > MAX_DEPTH {
            return t;
        }
        let m = |cx: &mut Self, x: TyId| cx.bounded_form(x, object_params, depth + 1);
        let k = match self.ty.kind(t).clone() {
            TyKind::Op(TyOp::Intersect, parts) => {
                let mut flat: Vec<TyId> = vec![];
                for p in parts {
                    let p = m(self, p);
                    let ps = match self.ty.kind(p) {
                        TyKind::Op(TyOp::Intersect, ps) => ps.clone(),
                        _ => vec![p],
                    };
                    for q in ps {
                        if !flat.contains(&q) {
                            flat.push(q);
                        }
                    }
                }
                let objects = |cx: &Self, p: TyId| matches!(cx.ty.kind(p), TyKind::Param(n) if object_params.contains(n));
                if flat.iter().any(|&p| objects(self, p)) {
                    use crate::any::Dynamic;
                    flat.retain(|&p| self.dynamic(p) != Some(Dynamic::Object));
                }
                if let [one] = flat[..] {
                    return one;
                }
                TyKind::Op(TyOp::Intersect, flat)
            }
            TyKind::Op(o, args) => TyKind::Op(o, args.iter().map(|a| m(self, *a)).collect()),
            TyKind::Adt(d, args) => {
                let anon = self.adt(d).filter(|a| a.kind == AdtKind::Anon);
                if let Some(module) = anon.map(|a| a.module) {
                    if args.is_empty() || self.reaches_itself(d) {
                        return t;
                    }
                    let mut fields = self.public_fields(t);
                    for f in &mut fields {
                        f.ty = m(self, f.ty);
                    }
                    return self.anon_type_with(&fields, module);
                }
                TyKind::Adt(d, args.iter().map(|a| m(self, *a)).collect())
            }
            TyKind::Array(e) => TyKind::Array(m(self, e)),
            TyKind::Map(a, b) => TyKind::Map(m(self, a), m(self, b)),
            TyKind::Tuple(ts) => TyKind::Tuple(ts.iter().map(|a| m(self, *a)).collect()),
            TyKind::Option(e, n) => TyKind::Option(m(self, e), n),
            TyKind::Result(a, b) => TyKind::Result(m(self, a), m(self, b)),
            TyKind::Promise(v, e) => TyKind::Promise(m(self, v), m(self, e)),
            TyKind::Shared(e) => TyKind::Shared(m(self, e)),
            TyKind::FnPtr {
                params,
                ret,
                throws,
                rest,
            } => TyKind::FnPtr {
                params: params.iter().map(|a| m(self, *a)).collect(),
                ret: m(self, ret),
                throws: m(self, throws),
                rest,
            },
            _ => return t,
        };
        self.ty.intern(k)
    }

    /// Is every value of `t` an object? An intersection is when a part is `object` or a type
    /// parameter in `object_params` (`T extends object`), or when a part is an object type and
    /// no part may be a primitive: `{ value: T } & { $set(v: T): void }` is, but
    /// `T & { __brand: "Id" }` with an unbounded `T` is not (at `T = string` it is a branded
    /// `string`).
    pub(crate) fn holds_objects(&self, t: TyId, object_params: &[u32]) -> bool {
        use crate::any::Dynamic;
        match self.ty.kind(t) {
            TyKind::Op(TyOp::Intersect, parts) => {
                let closed = |cx: &Self, p: TyId| match cx.ty.kind(p) {
                    TyKind::Param(n) => object_params.contains(n),
                    TyKind::Op(TyOp::Intersect, _) => cx.holds_objects(p, object_params),
                    _ => cx.dynamic(p) == Some(Dynamic::Object),
                };
                if parts.iter().any(|&p| closed(self, p)) {
                    return true;
                }
                let open = |cx: &Self, p: TyId| {
                    let p = cx.ty.opt_payload(p).unwrap_or(p);
                    matches!(cx.ty.kind(p), TyKind::Param(_) | TyKind::Op(..))
                };
                !parts.iter().any(|&p| open(self, p)) && parts.iter().any(|&p| self.is_object_ty(p))
            }
            TyKind::Param(n) => object_params.contains(n),
            _ => self.is_object_ty(t),
        }
    }

    /// Is `t` a union with a member that is a type parameter (`T | string`)?
    fn has_type_params_at_head(&mut self, t: TyId) -> bool {
        self.union_members(t)
            .is_some_and(|ms| ms.iter().any(|&m| self.deferred_part(m)))
    }

    /// The deferred intersection `op` (`TyOp::Intersect`) with known parts `args`, reduced.
    pub(crate) fn reduce_intersection(&mut self, op: TyId, args: Vec<TyId>, span: Span) -> TyId {
        // A part with an error inside (a closure whose parameter type is unknown) was reported.
        if args.iter().any(|&a| self.error_inside(a, 0)) {
            return self.ty.error;
        }
        let module = self.type_level.op_modules.get(&op).copied().unwrap_or(0);
        let operands: Vec<(TyId, Span)> = args.into_iter().map(|a| (a, span)).collect();
        let names = vec![None; operands.len()];
        self.intersect(operands, names, module, span)
    }

    /// Does `t` hold an error type, also in the fields of the object types it holds?
    fn error_inside(&mut self, t: TyId, depth: u32) -> bool {
        if self.ty.has_error(t) {
            return true;
        }
        if depth > 4 || !self.is_object_type(t) {
            return false;
        }
        let fields = self.public_fields(t);
        fields.iter().any(|f| self.error_inside(f.ty, depth + 1))
    }

    /// The intersection of `operands` (written as `names`, where they are names), at `span`.
    fn intersect(
        &mut self,
        mut operands: Vec<(TyId, Span)>,
        mut names: Vec<Option<String>>,
        module: usize,
        span: Span,
    ) -> TyId {
        if let Some(t) = self.literal_meet(&operands, span) {
            return t;
        }
        if let Some(t) = self.dynamic_meet(&mut operands, &mut names) {
            return t;
        }
        // A primitive or a class instance with object types is a branded type.
        let bases: Vec<usize> = (0..operands.len())
            .filter(|&i| self.brand_operand(operands[i].0))
            .collect();
        let brand = match bases.as_slice() {
            [i] if operands.len() > 1 => {
                names.remove(*i);
                Some(operands.remove(*i).0)
            }
            _ => None,
        };
        // A function with properties (#921): an interface or object type with a call signature
        // and members (`{ (): T; stop(): void }`, which the parser writes as this
        // intersection). The function type is the callable object type `{ #call: F }`
        // (`crate::callables`), which merges with the members as any object type does.
        if !self.callable_operands(&mut operands, module, span) {
            return self.ty.error;
        }
        for (&(ty, at), name) in operands.iter().zip(&names) {
            if !self.intersectable(ty, at, name.as_deref()) {
                return self.ty.error;
            }
        }
        self.synth_ifaces.span = span;
        let mut acc = operands[0].0;
        for &(b, _) in &operands[1..] {
            match self.meet(acc, b, module, 0) {
                Ok(m) => acc = m,
                Err(why) => {
                    let shown: Vec<String> =
                        operands.iter().map(|(o, _)| self.display(*o)).collect();
                    let what = match operands.iter().any(|(o, _)| self.is_method_iface(*o)) {
                        true => "members",
                        false => "fields",
                    };
                    self.error(
                        Diagnostic::error(
                            format!("no value has type `{}`: {why}", shown.join(" & ")),
                            span,
                        )
                        .with_note(format!(
                            "a value of an intersection has every part's {what} at once"
                        )),
                    );
                    return self.ty.error;
                }
            }
        }
        if self.is_object_type(acc) {
            if let Some(fields) = Self::call_field_first(&self.public_fields(acc)) {
                acc = self.anon_type_with(&fields, module);
            }
        }
        self.declare_intersection_fields(acc, &operands);
        // A part that is the whole result (`T & { id: string }` at a `T` that has `id`) splits
        // nothing: such a result is not recorded as made of parts.
        if brand.is_none()
            && operands.len() > 1
            && self.is_object_type(acc)
            && operands.iter().all(|&(o, _)| o != acc)
        {
            let mut parts = vec![];
            for &(o, _) in &operands {
                match self.type_level.intersection_parts.get(&o) {
                    Some(ps) => parts.extend(ps.iter().copied()),
                    None => parts.push(o),
                }
            }
            self.type_level
                .intersection_parts
                .entry(acc)
                .or_insert(parts);
        }
        match brand {
            // `string & {}` (the literal-union idiom `T | (string & {})`) holds every string, as
            // in TypeScript: only a part with fields brands a primitive.
            Some(base) if self.brand_adds_nothing(base, acc) => base,
            Some(base) => self.brand_type(base, acc, module, span),
            None => acc,
        }
    }

    /// The dynamic parts, as TypeScript intersects them: `any & X` is `any`, `unknown & X` is
    /// `X`, and `object & X` is `X` for an `X` whose values are objects (`T & object` at an
    /// object type `T` is `T`). Those parts are removed from `operands`; `Some` when the result is
    /// known already.
    fn dynamic_meet(
        &mut self,
        operands: &mut Vec<(TyId, Span)>,
        names: &mut Vec<Option<String>>,
    ) -> Option<TyId> {
        use crate::any::Dynamic;
        // `X & X` is `X` (`T & object` at `T = object`).
        let mut i = 0;
        while i < operands.len() {
            if operands[..i].iter().any(|&(o, _)| o == operands[i].0) {
                operands.remove(i);
                names.remove(i);
            } else {
                i += 1;
            }
        }
        if let Some(&(any, _)) = operands
            .iter()
            .find(|(o, _)| matches!(self.dynamic(*o), Some(Dynamic::Any)))
        {
            return Some(any);
        }
        // `object & X` keeps the values of `X` that are objects: `(string | { a: number } |
        // null) & object` is `{ a: number }`, and `number & object` (`T & object` at a
        // primitive `T`) has no value, `never`.
        let never = self.ty.never;
        if operands.iter().any(|&(o, _)| o == never) {
            return Some(never);
        }
        // `undefined` or `null` alone (an option of `never`, as an optional field of a mapped
        // type reads) with a part that is not one has no value either: `undefined & string`.
        let nothing_alone = |cx: &Self, o: TyId| cx.ty.opt_payload(o) == Some(never);
        if operands.iter().any(|&(o, _)| nothing_alone(self, o))
            && operands
                .iter()
                .any(|&(o, _)| self.ty.opt_payload(o).is_none())
        {
            return Some(never);
        }
        if operands
            .iter()
            .any(|&(o, _)| matches!(self.dynamic(o), Some(Dynamic::Object)))
        {
            for op in operands.iter_mut() {
                if self.dynamic(op.0).is_none() {
                    match self.object_values(op.0) {
                        Some(t) if t == never => return Some(never),
                        Some(t) => op.0 = t,
                        None => {}
                    }
                }
            }
        }
        let others_are_objects = |cx: &Self, ops: &[(TyId, Span)]| {
            ops.iter().any(|&(o, _)| cx.dynamic(o).is_none())
                && ops
                    .iter()
                    .all(|&(o, _)| cx.dynamic(o).is_some() || cx.is_object_ty(o))
        };
        let droppable = |cx: &Self, t: TyId, ops: &[(TyId, Span)]| match cx.dynamic(t) {
            Some(Dynamic::Unknown) => true,
            Some(Dynamic::Object) => others_are_objects(cx, ops),
            _ => false,
        };
        let mut i = 0;
        while i < operands.len() && operands.len() > 1 {
            if droppable(self, operands[i].0, operands) {
                operands.remove(i);
                names.remove(i);
            } else {
                i += 1;
            }
        }
        (operands.len() == 1).then(|| operands[0].0)
    }

    /// The values of `t` that are objects, when `t` has values that are not (a primitive, a
    /// union with primitive members, `null` or `undefined`): `never` when it has none. `None`
    /// when every value of `t` is an object, or when `t` is not known yet.
    fn object_values(&mut self, t: TyId) -> Option<TyId> {
        let never = self.ty.never;
        let core = self.ty.core(t);
        let c = core.unwrap_or(t);
        if matches!(self.ty.kind(c), TyKind::Param(_) | TyKind::Op(..)) {
            return None;
        }
        let Some(members) = self.union_members(c) else {
            return match self.is_object_ty(c) {
                true => core,
                false => Some(never),
            };
        };
        let kept: Vec<TyId> = members
            .iter()
            .copied()
            .filter(|&m| self.is_object_ty(m))
            .collect();
        match kept.as_slice() {
            [] => Some(never),
            [one] => Some(*one),
            _ if kept.len() == members.len() => core,
            _ => Some(self.union_of(&kept, false, Span::DUMMY)),
        }
    }

    /// Record where each field of the intersection `t` is written (for editors: go to
    /// definition, hover and its doc comment): in the first operand that has it.
    fn declare_intersection_fields(&mut self, t: TyId, operands: &[(TyId, Span)]) {
        let TyKind::Adt(d, _) = self.ty.kind(t).clone() else {
            return;
        };
        let Some(a) = self.adt(d) else {
            return;
        };
        let spans: Vec<Span> = a
            .fields
            .iter()
            .map(|f| {
                operands
                    .iter()
                    .find_map(|&(o, _)| self.written_field(o, &f.name))
                    .unwrap_or(Span::DUMMY)
            })
            .collect();
        self.declare_anon_fields(t, &spans);
    }

    /// Where the field `name` of the object type `t` is written, if it is.
    fn written_field(&self, t: TyId, name: &str) -> Option<Span> {
        let TyKind::Adt(d, _) = self.ty.kind(t) else {
            return None;
        };
        let f = self.adt(*d)?.fields.iter().find(|f| f.name == name)?;
        (f.span != Span::DUMMY).then_some(f.span)
    }

    /// Is `t` (written as the name `written`, if it is one) an operand `&` can combine
    /// (reported at `span` if not)?
    fn intersectable(&mut self, t: TyId, span: Span, written: Option<&str>) -> bool {
        if let Some(p) = self.ty.opt_payload(t) {
            return self.intersectable(p, span, None);
        }
        if let Some(members) = self.union_members(t) {
            return members
                .into_iter()
                .all(|m| self.intersectable(m, span, None));
        }
        // An interface with methods makes an interface with every part's members
        // (`crate::synth_ifaces`).
        if self.is_object_type(t) || self.is_method_iface(t) {
            return true;
        }
        let shown = written.map_or_else(|| self.display(t), str::to_string);
        let object = "write the fields as an object type `{ … }`";
        let (msg, note) = match self.ty.kind(t).clone() {
            TyKind::Adt(d, _) => match self.adt(d).map(|a| a.kind) {
                Some(AdtKind::Class) if Some(d) == self.prelude_adt("Record") => (
                    format!("`&` combines object types; `{shown}` is a map of keys to values"),
                    "a `Record` is a dictionary, not an object type with fields: write the fields as an object type (`{ a: V; b: V }`), or keep the record in a field (`{ data: Record<string, V> } & { … }`)".to_string(),
                ),
                Some(AdtKind::Class) => (
                    format!("`&` combines object types; `{shown}` is a class"),
                    format!("Velt classes are nominal: use `Pick<{shown}, …>` for its fields, or a field-only interface"),
                ),
                Some(AdtKind::Struct) => (
                    format!("`&` combines object types; `{shown}` is a struct"),
                    format!("use `Pick<{shown}, …>` for its fields, or an object type"),
                ),
                _ => (
                    format!("`&` combines object types; found `{shown}`"),
                    object.into(),
                ),
            },
            TyKind::Dyn(..) => (
                format!("`&` combines object types; `{shown}` is an interface with methods"),
                "as a bound, `T extends A & B` combines interfaces".into(),
            ),
            TyKind::Array(_) | TyKind::Tuple(_) => (
                format!("`&` combines object types; `{shown}` is an array"),
                "an array with extra members is not supported: keep the array in a field (`{ items: T[] } & { … }`)".into(),
            ),
            TyKind::FnPtr { .. } | TyKind::Closure(_) => (
                "intersections of function types (overloads) are not supported".to_string(),
                "declare one function type with the parameter types every call needs".into(),
            ),
            TyKind::Param(_) if self.checking_unused_aliases => return false,
            TyKind::Param(_) => (
                format!("`&` needs concrete object types; `{shown}` is a type parameter"),
                "intersections on type parameters are not supported yet (#350); write the concrete object types".into(),
            ),
            _ => (
                format!("`&` combines object types; `{shown}` is not one"),
                object.into(),
            ),
        };
        self.error(Diagnostic::error(msg, span).with_note(note));
        false
    }

    /// An anonymous object type, or a field-only interface's.
    pub(crate) fn is_object_type(&self, t: TyId) -> bool {
        match self.ty.kind(t) {
            TyKind::Adt(d, _) => self
                .adt(*d)
                .is_some_and(|a| a.kind == AdtKind::Anon || self.field_only_of.contains_key(d)),
            _ => false,
        }
    }

    /// The intersection of `a` and `b`; `Err` says why no value has both types.
    pub(crate) fn meet(
        &mut self,
        a: TyId,
        b: TyId,
        module: usize,
        depth: u32,
    ) -> Result<TyId, String> {
        if a == b {
            return Ok(a);
        }
        if depth > MAX_DEPTH {
            return Err("the types nest too deeply to combine".into());
        }
        match (self.ty.opt_payload(a), self.ty.opt_payload(b)) {
            (Some(x), Some(y)) => {
                let m = self.meet(x, y, module, depth + 1)?;
                return Ok(self.ty.option(m));
            }
            (Some(x), None) => return self.meet(x, b, module, depth + 1),
            (None, Some(y)) => return self.meet(a, y, module, depth + 1),
            (None, None) => {}
        }
        for (u, other, swap) in [(a, b, false), (b, a, true)] {
            if let Some(members) = self.union_members(u) {
                return self.meet_union(&members, other, swap, module, depth);
            }
        }
        if let Some(r) = self.meet_iface(a, b, module, self.synth_ifaces.span) {
            return r;
        }
        if self.is_object_type(a) && self.is_object_type(b) {
            return self.merge_objects(a, b, module, depth);
        }
        for (l, base) in [(a, b), (b, a)] {
            if let Some(v) = self.lit_value(l) {
                if self.lit_base(&v) == base {
                    return Ok(l);
                }
            }
        }
        if let Some(m) = self.meet_param(a, b, module) {
            return Ok(m);
        }
        // Two function types are overloads in TypeScript; when one of them takes every argument
        // the other does (`(v: A) => void` and `(v: A & B) => void`, nested `Signal<Signal<…>>`'s
        // `$set`s), a call through either is a call of that one.
        for (wide, narrow) in [(a, b), (b, a)] {
            if self.takes_all_of(wide, narrow) {
                return Ok(wide);
            }
        }
        let (sa, sb) = (self.display(a), self.display(b));
        Err(format!("`{sa}` and `{sb}` have no value in common"))
    }

    /// `(M1 | M2 | …) & other` (or `other & (…)` when `swap`): the members that meet `other`.
    fn meet_union(
        &mut self,
        members: &[TyId],
        other: TyId,
        swap: bool,
        module: usize,
        depth: u32,
    ) -> Result<TyId, String> {
        let mut out = vec![];
        let mut first_err = None;
        for &m in members {
            let r = match swap {
                false => self.meet(m, other, module, depth + 1),
                true => self.meet(other, m, module, depth + 1),
            };
            match r {
                Ok(t) => out.push(t),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        match out.len() {
            0 => Err(first_err.unwrap_or_default()),
            1 => Ok(out[0]),
            _ => Ok(self.union_of(&out, false, Span::DUMMY)),
        }
    }

    /// The object type with the fields of both `a` and `b` (see the module docs).
    fn merge_objects(
        &mut self,
        a: TyId,
        b: TyId,
        module: usize,
        depth: u32,
    ) -> Result<TyId, String> {
        let (fa, fb) = (self.public_fields(a), self.public_fields(b));
        let mut out = fa;
        for f in fb {
            let Some(i) = out.iter().position(|g| g.name == f.name) else {
                out.push(f);
                continue;
            };
            let g = out[i].clone();
            let ty = match self.meet(g.ty, f.ty, module, depth + 1) {
                Ok(ty) => ty,
                Err(_) => {
                    let (ga, fb) = (self.display(g.ty), self.display(f.ty));
                    return Err(format!(
                        "field `{}` is `{ga}` in one part and `{fb}` in another",
                        f.name
                    ));
                }
            };
            out[i] = ShapeField {
                name: g.name,
                ty,
                readonly: g.readonly && f.readonly,
                optional: g.optional && f.optional,
            };
        }
        Ok(self.anon_type_with(&out, module))
    }

    /// The fields of object type `t`, its type arguments substituted.
    pub(crate) fn public_fields(&mut self, t: TyId) -> Vec<ShapeField> {
        let TyKind::Adt(d, args) = self.ty.kind(t).clone() else {
            return vec![];
        };
        let Ok(fields) = self.fields_now(d) else {
            return vec![];
        };
        fields
            .into_iter()
            .filter(|(_, public)| *public)
            .map(|(f, _)| ShapeField {
                ty: self.subst(f.ty, &args),
                ..f
            })
            .collect()
    }
}

/// The name an operand is written as (`T` in `T & { … }`), for messages.
fn written_name(t: &ast::TypeExpr) -> Option<&str> {
    match &t.kind {
        ast::TypeExprKind::Named { path, .. } if path.len() == 1 => Some(&path[0].name),
        _ => None,
    }
}
