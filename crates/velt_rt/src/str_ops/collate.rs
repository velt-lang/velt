//! `s.localeCompare(t)`: ICU's root collation (what Node uses without a locale) for Latin text.
//!
//! Three levels, as in the Unicode Collation Algorithm: base letters first (`"a" < "B"`), then
//! accents (`"e" < "é"`), then case (`"a" < "A"`). Characters up to U+024F (Basic Latin through
//! Latin Extended-B) take their weights from a table generated from ICU (`collate_table.rs`), so
//! ordinary Latin text, punctuation and digits order exactly like Node. Combining marks
//! (U+0300..U+036F) weigh only as accents, so a decomposed `e` + U+0301 sorts next to `é` but
//! not equal to it (ICU normalizes first). Any other letter or digit sorts after every Latin
//! letter and any other symbol (emoji included) just before the digits, each group by code
//! point; ICU orders scripts and symbols in about the same way. Controls are ignored, as in ICU.

#[path = "collate_table.rs"]
mod table;

use super::text;
use crate::str::VeltStr;
use std::cmp::Ordering;

/// The first character in the table (U+0020; the controls before it are ignorable).
const FIRST: u32 = 0x20;

/// One collation element: (primary, secondary, tertiary); primary 0 = ignorable at level 1.
/// A table primary `p` weighs `p << 21`, leaving room for characters outside the table, which
/// weigh by code point (21 bits) between two table primaries.
type Element = (u64, u32, u32);

/// The collation elements of `c` (two for an expansion such as `æ`), none if ignorable.
fn elements(c: char) -> [Option<Element>; 2] {
    let cp = c as u32;
    if (FIRST..FIRST + table::TABLE.len() as u32).contains(&cp) {
        let w = table::TABLE[(cp - FIRST) as usize];
        if w == 0 {
            return [None, None];
        }
        let (p1, p2) = ((w & 0xfff) << 21, ((w >> 12) & 0xfff) << 21);
        let (sec, ter) = (((w >> 24) & 0xff) as u32, ((w >> 32) & 0xf) as u32);
        let second = (p2 != 0).then_some((p2, 0, 0));
        return [Some((p1, sec, ter)), second];
    }
    if cp < FIRST || (0x7f..=0x9f).contains(&cp) {
        return [None, None];
    }
    if (0x300..=0x36f).contains(&cp) {
        return [Some((0, 1 + cp - 0x300, 0)), None];
    }
    // ICU sorts other letters after Latin ones, and symbols (emoji included) before digits.
    let after = if c.is_alphanumeric() {
        u64::from(table::LAST_PRIMARY)
    } else {
        (table::TABLE[('0' as u32 - FIRST) as usize] & 0xfff) - 1
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

/// Collation order of `a` and `b` (see the module docs).
pub fn collate(a: &str, b: &str) -> Ordering {
    (0..3)
        .map(|level| weights(a, level).cmp(weights(b, level)))
        .find(|o| o.is_ne())
        .unwrap_or(Ordering::Equal)
}

/// `s.localeCompare(t)`: -1, 0 or 1, like Node without a locale argument.
///
/// # Safety
/// `s` and `t` must be valid `VeltStr`s.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_locale_compare(s: *const VeltStr, t: *const VeltStr) -> i64 {
    match collate(text(s), text(t)) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::collate;
    use std::cmp::Ordering::{Equal, Greater, Less};

    /// Expected values from Node 22 (`a.localeCompare(b)`).
    #[test]
    fn matches_icu_root_collation() {
        let cases = [
            ("a", "A", Less),
            ("A", "a", Greater),
            ("a", "B", Less),
            ("B", "a", Greater),
            ("ab", "Aa", Greater),
            ("a-b", "ab", Less),
            ("a b", "a-b", Less),
            ("", "a", Less),
            ("x", "x\u{0}", Equal),
            ("a10", "a2", Less),
            ("résumé", "resume", Greater),
            ("résumé", "Resume", Greater),
            ("e", "é", Less),
            ("é", "f", Less),
            ("ä", "á", Greater),
            ("æ", "ae", Greater),
            ("æ", "af", Less),
            ("ß", "ss", Greater),
            ("ø", "p", Less),
            ("Zebra", "apple", Greater),
            ("_", "-", Less),
            ("9", "a", Less),
            ("$", "0", Less),
            ("z", "\u{3b1}", Less),
            ("\u{1f600}", "a", Less),
            ("\u{1f600}", "0", Less),
            ("e\u{301}", "f", Less),
        ];
        for (a, b, want) in cases {
            assert_eq!(collate(a, b), want, "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn sorts_like_node() {
        let mut words = vec![
            "Ab", "ab-", "a", "ä", "ab", "A", "æ", "b", "ae", "a1", "aB", "Á", "a b", "á", "af",
        ];
        words.sort_by(|a, b| collate(a, b));
        let node = "a A á Á ä a b a1 ab aB Ab ab- ae æ af b";
        assert_eq!(words.join(" "), node);
    }
}
