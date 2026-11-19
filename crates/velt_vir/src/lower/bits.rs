//! A fixed-size bit set, for dense dataflow over a function's locals.

#[cfg(test)]
thread_local! {
    /// Words of every `Bits` made or cloned: the work of a dense dataflow, which tests bound.
    pub(in crate::lower) static WORDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn count_words(_n: usize) {
    #[cfg(test)]
    WORDS.with(|w| w.set(w.get() + _n));
}

/// A fixed-size bit set over local indexes (or over the indexes of some numbering of them).
/// Up to 128 bits are inline: the async spill analysis makes several per block, and most
/// functions have that few locals (heap sets were 13% of a todo_api build's allocations).
#[derive(PartialEq)]
pub(in crate::lower) struct Bits(smallvec::SmallVec<[u64; 2]>);

impl Clone for Bits {
    fn clone(&self) -> Self {
        count_words(self.0.len());
        Bits(self.0.clone())
    }
}

impl Bits {
    pub(in crate::lower) fn new(n: usize) -> Self {
        count_words(n.div_ceil(64));
        Bits(smallvec::smallvec![0; n.div_ceil(64)])
    }
    pub(in crate::lower) fn get(&self, i: usize) -> bool {
        self.0[i / 64] >> (i % 64) & 1 == 1
    }
    pub(in crate::lower) fn set(&mut self, i: usize) {
        self.0[i / 64] |= 1 << (i % 64);
    }
    pub(in crate::lower) fn unset(&mut self, i: usize) {
        self.0[i / 64] &= !(1 << (i % 64));
    }
    /// Empty the set.
    pub(in crate::lower) fn clear(&mut self) {
        self.0.fill(0);
    }
    /// `self = other`, of the same size.
    pub(in crate::lower) fn assign(&mut self, other: &Bits) {
        self.0.copy_from_slice(&other.0);
    }
    /// `self |= other`; returns whether anything was added.
    pub(in crate::lower) fn union(&mut self, other: &Bits) -> bool {
        let mut changed = false;
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            let n = *a | b;
            changed |= n != *a;
            *a = n;
        }
        changed
    }
    pub(in crate::lower) fn intersect(&mut self, other: &Bits) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a &= b;
        }
    }
    pub(in crate::lower) fn minus(&mut self, other: &Bits) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a &= !b;
        }
    }
    /// The set indexes in ascending order.
    pub(in crate::lower) fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(w, &word)| {
            let mut rest = word;
            std::iter::from_fn(move || {
                (rest != 0).then(|| {
                    let i = rest.trailing_zeros() as usize;
                    rest &= rest - 1;
                    w * 64 + i
                })
            })
        })
    }
}
