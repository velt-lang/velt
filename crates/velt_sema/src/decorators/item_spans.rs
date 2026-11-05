//! Which of a module's items hold a position, for the scope walk ([`super::scope`]): a lookup per
//! decorator, not a test of every item. The items aren't in source order and their spans aren't
//! disjoint when sema gets them: a script's generated `main` comes last and spans the declarations
//! between its statements, an imported module's chunks and its variables move to the end,
//! `namespaces` hoists items, `overloads` repeats an item's span for each signature, and an item
//! `interface_merge` adds comes from another file. So the spans are sorted once per module, the
//! first time one of its decorators asks, and searched as an interval tree: the sorted entries are
//! the tree, each range's middle entry being its root, which holds the furthest end in its range.

use velt_common::{FileId, Span};

#[derive(Clone, Copy)]
struct Entry {
    lo: u32,
    hi: u32,
    /// The furthest `hi` in the range this entry is the middle of.
    reach: u32,
    /// Its place in the module's items.
    item: u32,
}

/// The spans of a module's items in file `file`, by position; see the module docs.
pub(crate) struct ItemSpans(Vec<Entry>);

impl ItemSpans {
    pub(super) fn new(spans: impl Iterator<Item = Span>, file: FileId) -> Self {
        let mut entries: Vec<Entry> = spans
            .enumerate()
            .filter(|(_, s)| s.file == file)
            .map(|(item, s)| Entry {
                lo: s.lo,
                hi: s.hi,
                reach: s.hi,
                item: item as u32,
            })
            .collect();
        entries.sort_unstable_by_key(|e| e.lo);
        set_reach(&mut entries);
        ItemSpans(entries)
    }

    /// The items whose span holds `lo..hi`, in the module's order.
    pub(super) fn holding(&self, lo: u32, hi: u32) -> Vec<usize> {
        let mut found = vec![];
        let starting = self.0.partition_point(|e| e.lo <= lo);
        let looked_at = search(&self.0, starting, hi, &mut found);
        crate::check_work::add_decorator_scope_items(looked_at);
        found.sort_unstable();
        found
    }
}

/// Sets `reach` in the tree on `entries`; returns its root's.
fn set_reach(entries: &mut [Entry]) -> u32 {
    let mid = entries.len() / 2;
    let (left, rest) = entries.split_at_mut(mid);
    let Some((root, right)) = rest.split_first_mut() else {
        return 0;
    };
    root.reach = root.hi.max(set_reach(left)).max(set_reach(right));
    root.reach
}

/// Adds to `found` the items among the first `starting` of `entries` that end at `hi` or after.
/// Returns how many entries it looked at.
fn search(entries: &[Entry], starting: usize, hi: u32, found: &mut Vec<usize>) -> u64 {
    let mid = entries.len() / 2;
    let Some(root) = entries.get(mid).filter(|r| starting > 0 && r.reach >= hi) else {
        return 0;
    };
    let mut looked_at = 1 + search(&entries[..mid], starting.min(mid), hi, found);
    if mid < starting {
        if root.hi >= hi {
            found.push(root.item as usize);
        }
        looked_at += search(&entries[mid + 1..], starting - mid - 1, hi, found);
    }
    looked_at
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: FileId = FileId(7);

    fn span(lo: u32, hi: u32) -> Span {
        Span { file: FILE, lo, hi }
    }

    /// What testing every item finds.
    fn every_item(spans: &[Span], lo: u32, hi: u32) -> Vec<usize> {
        (0..spans.len())
            .filter(|&i| spans[i].file == FILE && spans[i].lo <= lo && hi <= spans[i].hi)
            .collect()
    }

    fn agrees(spans: &[Span], limit: u32) {
        let index = ItemSpans::new(spans.iter().copied(), FILE);
        for lo in 0..limit {
            for hi in lo..limit {
                let expected = every_item(spans, lo, hi);
                assert_eq!(index.holding(lo, hi), expected, "{lo}..{hi} in {spans:?}");
            }
        }
    }

    #[test]
    fn items_in_source_order() {
        agrees(&[span(0, 4), span(5, 9), span(10, 14), span(15, 19)], 22);
    }

    #[test]
    fn a_script_main_comes_last_and_spans_the_declarations() {
        let spans = [span(4, 8), span(9, 12), span(14, 18), span(2, 20)];
        agrees(&spans, 24);
        let index = ItemSpans::new(spans.iter().copied(), FILE);
        assert_eq!(index.holding(10, 11), [1, 3]);
        assert_eq!(index.holding(13, 13), [3]);
        assert_eq!(index.holding(1, 1), [0usize; 0]);
    }

    #[test]
    fn chunks_moved_to_the_end() {
        agrees(&[span(6, 8), span(14, 16), span(0, 4), span(9, 12)], 20);
    }

    #[test]
    fn overload_signatures_share_their_function_s_span() {
        let spans = [span(0, 4), span(5, 9), span(5, 9), span(5, 9), span(10, 12)];
        agrees(&spans, 14);
        let index = ItemSpans::new(spans.iter().copied(), FILE);
        assert_eq!(index.holding(6, 7), [1, 2, 3]);
    }

    #[test]
    fn an_item_of_another_file_holds_nothing() {
        let other = Span {
            file: FileId(8),
            lo: 0,
            hi: 100,
        };
        let spans = [span(0, 4), other, span(5, 9), Span::DUMMY];
        agrees(&spans, 12);
        let index = ItemSpans::new(spans.iter().copied(), FILE);
        assert_eq!(index.holding(1, 2), [0]);
    }

    #[test]
    fn no_items() {
        assert!(ItemSpans::new(std::iter::empty(), FILE)
            .holding(1, 2)
            .is_empty());
    }

    /// Spans in any order, nested, overlapping and repeated, of every length of list (a wrong
    /// middle shows at some length).
    #[test]
    fn any_spans_find_what_testing_every_item_finds() {
        let mut x = 0x2545_f491_u32;
        let mut next = move |bound: u32| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x % bound
        };
        for len in 0..40 {
            for _ in 0..8 {
                let spans: Vec<Span> = (0..len)
                    .map(|_| {
                        let lo = next(30);
                        span(lo, lo + next(12))
                    })
                    .collect();
                agrees(&spans, 44);
            }
        }
    }
}
