//! `s.localeCompare(t)` as velt_rt had it before std/lang/string/collate.velt: the same table and
//! levels, over `chars()` (the runtime's lossy view of a string). The program is the one in
//! `bench/stdport/locale_compare.velt`.

mod table;

use std::cmp::Ordering;

/// One collation element: (primary, secondary, tertiary); primary 0 = ignorable at level 1.
/// A table primary `p` weighs `p << 21`, leaving room for characters outside the table, which
/// weigh by code point (21 bits) between two table primaries.
type Element = (u64, u32, u32);

/// The table word of `c`, if the table covers it.
fn table_word(cp: u32) -> Option<u64> {
    table::BLOCKS.iter().find_map(|&(first, words)| {
        let i = cp.checked_sub(first)? as usize;
        words.get(i).copied()
    })
}

/// The rank of a letter or digit outside the table among the scripts the root collation orders
/// last: 0 for most, then Hangul, kana, and Han ideographs after every other script.
fn script_tier(cp: u32) -> u64 {
    match cp {
        0x1100..=0x11ff | 0x3130..=0x318f | 0xac00..=0xd7af => 1,
        0x3040..=0x30ff | 0x31f0..=0x31ff => 2,
        0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af => 3,
        _ => 0,
    }
}

/// The collation elements of `c` (two for an expansion such as `æ`), none if ignorable.
fn elements(c: char) -> [Option<Element>; 2] {
    let cp = c as u32;
    if let Some(w) = table_word(cp) {
        if w == 0 {
            return [None, None];
        }
        let (p1, p2) = ((w & 0xfff) << 21, ((w >> 12) & 0xfff) << 21);
        let (sec, ter) = (((w >> 24) & 0xff) as u32, ((w >> 32) & 0xf) as u32);
        let second = (p2 != 0).then_some((p2, 0, 0));
        return [Some((p1, sec, ter)), second];
    }
    if cp < 0x20 {
        return [None, None];
    }
    if (0x300..=0x36f).contains(&cp) {
        return [Some((0, 1 + cp - 0x300, 0)), None];
    }
    let last = u64::from(table::LAST_PRIMARY);
    let tier = script_tier(cp);
    let after = if tier > 0 || c.is_alphanumeric() {
        last + tier
    } else {
        // Just below the digits (`'0'` has the lowest digit primary).
        table_word('0' as u32).map_or(0, |w| w & 0xfff) - 1
    };
    [Some(((after << 21) | u64::from(cp), 0, 0)), None]
}

/// The weights of `s` at one level (0, 1, 2), skipping elements ignorable at that level.
fn weights(s: &str, level: usize) -> impl Iterator<Item = u64> + '_ {
    s.chars()
        .flat_map(elements)
        .flatten()
        .filter(move |e| level > 0 || e.0 != 0)
        .map(move |e| match level {
            0 => e.0,
            1 => u64::from(e.1),
            _ => u64::from(e.2),
        })
}

/// Collation order of `a` and `b`.
fn collate(a: &str, b: &str) -> Ordering {
    (0..3)
        .map(|level| weights(a, level).cmp(weights(b, level)))
        .find(|o| o.is_ne())
        .unwrap_or(Ordering::Equal)
}

const ASCII: [&str; 12] = [
    "apple", "Apple", "application", "banana", "Banana", "cherry", "date", "elder", "fig", "a-b",
    "ab", "a b",
];
const LATIN: [&str; 12] = [
    "résumé", "resume", "Résumé", "Straße", "strasse", "Ångström", "angle", "łódź", "lodz", "Việt",
    "viet", "Œuvre",
];
const OTHER: [&str; 12] = [
    "москва", "Москва", "ёлка", "елка", "αλφα", "Άλφα", "βήτα", "中文", "一", "가나", "あい", "😀",
];

fn main() {
    let groups = [ASCII, LATIN, OTHER];
    let mut seed: u64 = 2196;
    let mut next = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    let mut words: Vec<Vec<String>> = vec![];
    for g in groups {
        let mut list = vec![];
        for _ in 0..512 {
            let a = g[next(12) as usize];
            let b = g[next(12) as usize];
            // One word in four long enough to live on the heap.
            let long = next(4) == 0;
            list.push(if long { format!("{a}{b}{a}{b}{a}") } else { format!("{a}{b}") });
        }
        words.push(list);
    }
    let mut sum: i64 = 0;
    for list in &words {
        for round in 0..300usize {
            for i in 0..512usize {
                let j = (i * 7 + round) % 512;
                let order = match collate(&list[i], &list[j]) {
                    Ordering::Less => -1,
                    Ordering::Equal => 0,
                    Ordering::Greater => 1,
                };
                sum = sum * 3 % 1000003 + order;
            }
        }
    }
    println!("{sum}");
}
