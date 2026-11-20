//! The encoding of the values every saved type is made of: the scalars, strings, spans and ids,
//! and the standard containers of them. A hash map or set is written in the order of its keys'
//! encodings, so a unit's bytes do not depend on a table's iteration order.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{FileId, Span};

use super::{Codec, Reader, Writer};
use crate::hir::{DefId, LocalId, TyId};

impl Codec for bool {
    fn enc(&self, w: &mut Writer) {
        w.byte(u8::from(*self));
    }
    fn dec(r: &mut Reader) -> Self {
        match r.byte() {
            0 => false,
            1 => true,
            _ => {
                r.fail();
                false
            }
        }
    }
}

impl Codec for u8 {
    fn enc(&self, w: &mut Writer) {
        w.byte(*self);
    }
    fn dec(r: &mut Reader) -> Self {
        r.byte()
    }
}

impl Codec for u16 {
    fn enc(&self, w: &mut Writer) {
        w.uint(u64::from(*self));
    }
    fn dec(r: &mut Reader) -> Self {
        let v = r.u32();
        u16::try_from(v).unwrap_or_else(|_| {
            r.fail_damaged();
            0
        })
    }
}

impl Codec for u32 {
    fn enc(&self, w: &mut Writer) {
        w.uint(u64::from(*self));
    }
    fn dec(r: &mut Reader) -> Self {
        r.u32()
    }
}

impl Codec for u64 {
    fn enc(&self, w: &mut Writer) {
        w.uint(*self);
    }
    fn dec(r: &mut Reader) -> Self {
        r.uint()
    }
}

impl Codec for usize {
    fn enc(&self, w: &mut Writer) {
        w.uint(*self as u64);
    }
    fn dec(r: &mut Reader) -> Self {
        let v = r.uint();
        usize::try_from(v).unwrap_or_else(|_| {
            r.fail();
            0
        })
    }
}

impl Codec for i64 {
    fn enc(&self, w: &mut Writer) {
        // Zigzag: small negative numbers stay short.
        w.uint(((*self << 1) ^ (*self >> 63)) as u64);
    }
    fn dec(r: &mut Reader) -> Self {
        let v = r.uint();
        ((v >> 1) as i64) ^ -((v & 1) as i64)
    }
}

/// 128-bit integers (`hir::Lit::Int`, literal types' and C constants' values) as LEB128 too:
/// almost all are small.
impl Codec for u128 {
    fn enc(&self, w: &mut Writer) {
        let mut v = *self;
        while v >= 0x80 {
            w.out.push(v as u8 | 0x80);
            v >>= 7;
        }
        w.out.push(v as u8);
    }
    fn dec(r: &mut Reader) -> Self {
        let mut v = 0u128;
        let mut shift = 0;
        loop {
            let b = r.byte();
            if shift == 126 && b > 3 {
                r.fail();
                return 0;
            }
            v |= u128::from(b & 0x7f) << shift;
            if b < 0x80 {
                return v;
            }
            shift += 7;
        }
    }
}

impl Codec for i128 {
    fn enc(&self, w: &mut Writer) {
        (((*self << 1) ^ (*self >> 127)) as u128).enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        let v = u128::dec(r);
        ((v >> 1) as i128) ^ -((v & 1) as i128)
    }
}

impl Codec for f64 {
    fn enc(&self, w: &mut Writer) {
        w.out.extend_from_slice(&self.to_bits().to_le_bytes());
    }
    fn dec(r: &mut Reader) -> Self {
        let b = r.take(8);
        let mut a = [0u8; 8];
        if b.len() == 8 {
            a.copy_from_slice(b);
        }
        f64::from_bits(u64::from_le_bytes(a))
    }
}

impl Codec for String {
    fn enc(&self, w: &mut Writer) {
        w.uint(self.len() as u64);
        w.out.extend_from_slice(self.as_bytes());
    }
    fn dec(r: &mut Reader) -> Self {
        let n = r.len();
        match std::str::from_utf8(r.take(n)) {
            Ok(s) => s.to_string(),
            Err(_) => {
                r.fail();
                String::new()
            }
        }
    }
}

impl Codec for std::rc::Rc<str> {
    fn enc(&self, w: &mut Writer) {
        w.uint(self.len() as u64);
        w.out.extend_from_slice(self.as_bytes());
    }
    fn dec(r: &mut Reader) -> Self {
        String::dec(r).into()
    }
}

impl Codec for Span {
    fn enc(&self, w: &mut Writer) {
        if *self == Span::DUMMY {
            w.byte(0);
            return;
        }
        w.byte(1);
        w.file(self.file);
        w.uint(u64::from(self.lo));
        w.uint(u64::from(self.hi.wrapping_sub(self.lo)));
    }
    fn dec(r: &mut Reader) -> Self {
        match r.byte() {
            0 => Span::DUMMY,
            1 => {
                let file = r.file();
                let lo = r.u32();
                let len = r.u32();
                Span::new(file, lo, lo.wrapping_add(len))
            }
            _ => {
                r.fail();
                Span::DUMMY
            }
        }
    }
}

/// A file (a map key: `throws::FnSites::by_file`), through the unit's file table.
impl Codec for FileId {
    fn enc(&self, w: &mut Writer) {
        w.file(*self);
    }
    fn dec(r: &mut Reader) -> Self {
        r.file()
    }
}

/// Ids of a unit's own tables, stored as they are.
macro_rules! ids {
    ($($t:ident),*) => {$(
        impl Codec for $t {
            fn enc(&self, w: &mut Writer) {
                w.uint(u64::from(self.0));
            }
            fn dec(r: &mut Reader) -> Self {
                $t(r.u32())
            }
        }
    )*};
}
ids!(DefId, TyId, LocalId);

/// A memo a unit keeps (`Ctx::js_api_modules`): as an `Option` of what it holds.
impl<T: Codec + Clone> Codec for std::cell::OnceCell<T> {
    fn enc(&self, w: &mut Writer) {
        self.get().cloned().enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        let cell = std::cell::OnceCell::new();
        if let Some(v) = Option::<T>::dec(r) {
            let _ = cell.set(v);
        }
        cell
    }
}

impl<T: Codec> Codec for Option<T> {
    fn enc(&self, w: &mut Writer) {
        match self {
            None => w.byte(0),
            Some(v) => {
                w.byte(1);
                v.enc(w);
            }
        }
    }
    fn dec(r: &mut Reader) -> Self {
        match r.byte() {
            0 => None,
            1 => Some(T::dec(r)),
            _ => {
                r.fail();
                None
            }
        }
    }
}

/// Byte arrays (`hir::Intrinsic::SimdShuffle`'s lanes) as their bytes.
impl<const N: usize> Codec for [u8; N] {
    fn enc(&self, w: &mut Writer) {
        w.out.extend_from_slice(self);
    }
    fn dec(r: &mut Reader) -> Self {
        let mut a = [0u8; N];
        let b = r.take(N);
        if b.len() == N {
            a.copy_from_slice(b);
        }
        a
    }
}

impl<T: Codec> Codec for Box<T> {
    fn enc(&self, w: &mut Writer) {
        (**self).enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        r.enter();
        let v = Box::new(T::dec(r));
        r.leave();
        v
    }
}

/// A cell's contents (the caches of `ownership::local_async::targets::Literals`): no identity
/// is shared through one.
impl<T: Codec> Codec for std::cell::RefCell<T> {
    fn enc(&self, w: &mut Writer) {
        self.borrow().enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        std::cell::RefCell::new(T::dec(r))
    }
}

impl<T: Codec> Codec for Vec<T> {
    fn enc(&self, w: &mut Writer) {
        w.uint(self.len() as u64);
        for v in self {
            v.enc(w);
        }
    }
    fn dec(r: &mut Reader) -> Self {
        r.enter();
        let n = r.len();
        let mut out = Vec::with_capacity(r.capacity::<T>(n));
        for _ in 0..n {
            // A damaged value is dropped whole: no more of its elements are made.
            if r.failed() {
                break;
            }
            out.push(T::dec(r));
        }
        r.leave();
        out
    }
}

impl<A: Codec, B: Codec> Codec for (A, B) {
    fn enc(&self, w: &mut Writer) {
        self.0.enc(w);
        self.1.enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        let a = A::dec(r);
        (a, B::dec(r))
    }
}

impl<A: Codec, B: Codec, C: Codec> Codec for (A, B, C) {
    fn enc(&self, w: &mut Writer) {
        self.0.enc(w);
        self.1.enc(w);
        self.2.enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        let a = A::dec(r);
        let b = B::dec(r);
        (a, b, C::dec(r))
    }
}

/// Maps and sets are stored sorted by key, so that the same unit always encodes to the same
/// bytes (a hash map's order changes from run to run): by the bytes each key encodes to, with its
/// spans' files as their ids (keys need no `Ord`: spans have none).
fn sorted_by_key<'v, K: Codec, V>(entries: impl Iterator<Item = (&'v K, V)>) -> Vec<(&'v K, V)> {
    let mut entries: Vec<(&K, V)> = entries.collect();
    let mut w = Writer {
        out: vec![],
        files: FxHashMap::default(),
        file_list: vec![],
        nodes: FxHashMap::default(),
        frags: vec![],
        frag_starts: vec![],
        raw_files: true,
        ast_bytes: 0,
    };
    let mut at = vec![0];
    for (k, _) in &entries {
        k.enc(&mut w);
        at.push(w.out.len());
    }
    let key = |i: usize| &w.out[at[i]..at[i + 1]];
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|&i, &j| key(i).cmp(key(j)));
    let mut rank = vec![0; entries.len()];
    for (r, &i) in order.iter().enumerate() {
        rank[i] = r;
    }
    let mut ranked: Vec<(usize, (&K, V))> = rank.into_iter().zip(entries.drain(..)).collect();
    ranked.sort_unstable_by_key(|e| e.0);
    ranked.into_iter().map(|(_, e)| e).collect()
}

impl<K: Codec + std::hash::Hash + Eq, V: Codec> Codec for FxHashMap<K, V> {
    fn enc(&self, w: &mut Writer) {
        let v = sorted_by_key(self.iter());
        w.uint(v.len() as u64);
        for (k, val) in v {
            k.enc(w);
            val.enc(w);
        }
    }
    fn dec(r: &mut Reader) -> Self {
        r.enter();
        let n = r.len();
        let cap = r.capacity::<(K, V)>(n);
        let mut out = FxHashMap::with_capacity_and_hasher(cap, Default::default());
        for _ in 0..n {
            if r.failed() {
                break;
            }
            let k = K::dec(r);
            out.insert(k, V::dec(r));
        }
        r.leave();
        out
    }
}

impl<K: Codec + std::hash::Hash + Eq> Codec for FxHashSet<K> {
    fn enc(&self, w: &mut Writer) {
        let v = sorted_by_key(self.iter().map(|k| (k, ())));
        w.uint(v.len() as u64);
        for (k, ()) in v {
            k.enc(w);
        }
    }
    fn dec(r: &mut Reader) -> Self {
        r.enter();
        let n = r.len();
        let cap = r.capacity::<K>(n);
        let mut out = FxHashSet::with_capacity_and_hasher(cap, Default::default());
        for _ in 0..n {
            if r.failed() {
                break;
            }
            out.insert(K::dec(r));
        }
        r.leave();
        out
    }
}

impl<K: Codec + Ord, V: Codec> Codec for std::collections::BTreeMap<K, V> {
    fn enc(&self, w: &mut Writer) {
        w.uint(self.len() as u64);
        for (k, v) in self {
            k.enc(w);
            v.enc(w);
        }
    }
    fn dec(r: &mut Reader) -> Self {
        r.enter();
        let n = r.len();
        let mut out = std::collections::BTreeMap::new();
        for _ in 0..n {
            if r.failed() {
                break;
            }
            let k = K::dec(r);
            out.insert(k, V::dec(r));
        }
        r.leave();
        out
    }
}

impl<K: Codec + Ord> Codec for std::collections::BTreeSet<K> {
    fn enc(&self, w: &mut Writer) {
        w.uint(self.len() as u64);
        for k in self {
            k.enc(w);
        }
    }
    fn dec(r: &mut Reader) -> Self {
        r.enter();
        let n = r.len();
        let mut out = std::collections::BTreeSet::new();
        for _ in 0..n {
            if r.failed() {
                break;
            }
            out.insert(K::dec(r));
        }
        r.leave();
        out
    }
}

impl<A: Codec, B: Codec, C: Codec, D: Codec> Codec for (A, B, C, D) {
    fn enc(&self, w: &mut Writer) {
        self.0.enc(w);
        self.1.enc(w);
        self.2.enc(w);
        self.3.enc(w);
    }
    fn dec(r: &mut Reader) -> Self {
        let a = A::dec(r);
        let b = B::dec(r);
        let c = C::dec(r);
        (a, b, c, D::dec(r))
    }
}
