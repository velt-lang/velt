//! The encoding of a definition table ([`DefTable`], #2943 M2): its length, the byte size of its
//! entries (8 bytes, so a reader skips them), the entries, then where each ends. A reader over a
//! whole unit ([`Reader::over_unit`]) does not decode the entries: the table keeps where each is
//! and decodes one on its first use ([`DefTable::lazy`]), so a program pays for the definitions
//! of a unit it reads, not for all of them. Any other reader decodes the table whole.

use super::{Codec, Reader, Src, Writer};
use crate::ctx::def_table::Lazy;
use crate::ctx::DefTable;

impl<T: Codec> Codec for DefTable<T> {
    fn enc(&self, w: &mut Writer) {
        w.uint(self.len() as u64);
        let at = w.out.len();
        w.out.extend_from_slice(&[0; 8]);
        let start = w.out.len();
        let mut ends = Vec::with_capacity(self.len());
        for x in self.items_all() {
            x.enc(w);
            ends.push((w.out.len() - start) as u64);
        }
        let size = (w.out.len() - start) as u64;
        w.out[at..at + 8].copy_from_slice(&size.to_le_bytes());
        for e in ends {
            w.uint(e);
        }
    }

    fn dec(r: &mut Reader) -> Self {
        let n = r.len();
        let size = u64::from_le_bytes(r.take(8).try_into().unwrap_or([0; 8]));
        let size = usize::try_from(size).unwrap_or(usize::MAX);
        let start = r.pos();
        let mut t = DefTable::default();
        match r.src.clone() {
            Some(src) => {
                r.take(size);
                let mut ends = Vec::with_capacity(n);
                let mut last = start;
                for _ in 0..n {
                    let end = start.saturating_add(r.len_unchecked());
                    // Each entry ends after the one before it, inside the entries' bytes.
                    if end < last {
                        r.fail_damaged();
                    }
                    last = end;
                    ends.push(end);
                }
                if r.failed() || last != start.saturating_add(size) {
                    r.fail_damaged();
                    return t;
                }
                return DefTable::lazy(n, Lazy::new(src, start, ends, dec_entry::<T>));
            }
            None => {
                for _ in 0..n {
                    t.push(T::dec(r));
                }
                if r.pos() != start.saturating_add(size) {
                    r.fail_damaged();
                }
                for _ in 0..n {
                    r.len_unchecked();
                }
            }
        }
        t
    }
}

/// Entry `range` of a table of a unit decoded on demand (`src`'s bytes).
fn dec_entry<T: Codec>(src: &Src, range: std::ops::Range<usize>) -> T {
    let mut r = src.reader(range);
    let x = T::dec(&mut r);
    // The unit was checked whole when it was read (the store's checksum); an entry that does not
    // decode now means the encoding and this decoder disagree.
    assert!(r.done(), "ICE: an entry of a unit's table does not decode");
    x
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::super::Arena;
    use super::*;
    use crate::ctx::def_table::decoded;

    fn table() -> DefTable<(u32, String)> {
        (0..50u32)
            .map(|i| (i * 3, "x".repeat(i as usize)))
            .collect::<Vec<_>>()
            .into()
    }

    #[test]
    fn a_table_decoded_whole_comes_back_as_it_was() {
        let t = table();
        let mut w = Writer::new();
        t.enc(&mut w);
        7u32.enc(&mut w);
        let (bytes, files) = w.finish();
        let mut r = Reader::new(&bytes, &files);
        let back: DefTable<(u32, String)> = Codec::dec(&mut r);
        assert_eq!(
            u32::dec(&mut r),
            7,
            "what follows the table is read after it"
        );
        assert!(r.done());
        assert_eq!(back.len(), t.len());
        assert!(back.iter().eq(t.iter()));
    }

    #[test]
    fn a_unit_s_table_decodes_an_entry_when_it_is_first_read() {
        let t = table();
        let mut w = Writer::new();
        t.enc(&mut w);
        7u32.enc(&mut w);
        let (bytes, files) = w.finish();
        let (bytes, files) = (Rc::new(bytes), Rc::new(files));
        let arena = Arena::default();
        // SAFETY: what is decoded is dropped in this function, before `arena`.
        let mut r = unsafe { Reader::over_unit(&bytes, 0, &files, &arena) };
        let before = decoded::get();
        let mut back: DefTable<(u32, String)> = Codec::dec(&mut r);
        assert_eq!(
            u32::dec(&mut r),
            7,
            "what follows the table is read after it"
        );
        assert!(r.done());
        assert_eq!(back.len(), 50);
        assert_eq!(decoded::get(), before, "nothing is decoded yet");
        assert_eq!(back[40], t[40]);
        assert_eq!(back[40], t[40]);
        assert_eq!(decoded::get(), before + 1, "one entry, once");
        // A write decodes the entry it writes; a copy of the table decodes its own.
        back[3].0 = 1000;
        let copy = back.clone();
        assert_eq!(copy[3], (1000, "xxx".to_string()));
        assert_eq!(copy[4], t[4]);
        assert!(back.iter().skip(5).eq(t.iter().skip(5)));
        assert_eq!(back.into_vec().len(), 50);
    }

    #[test]
    fn a_damaged_table_fails_to_decode() {
        let t = table();
        let mut w = Writer::new();
        t.enc(&mut w);
        let (bytes, files) = w.finish();
        let files = Rc::new(files);
        let arena = Arena::default();
        for cut in [1, 9, bytes.len() / 2, bytes.len() - 1] {
            let part = Rc::new(bytes[..cut].to_vec());
            // SAFETY: as above.
            let mut r = unsafe { Reader::over_unit(&part, 0, &files, &arena) };
            let _: DefTable<(u32, String)> = Codec::dec(&mut r);
            assert!(!r.done(), "cut at {cut}");
        }
    }
}
