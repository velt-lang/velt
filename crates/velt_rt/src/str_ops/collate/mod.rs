//! `s.localeCompare(t)` in the CLDR root collation, like `Intl.Collator("und")` (Node's own
//! `localeCompare` uses the host's locale instead).
//!
//! Three levels, as in the Unicode Collation Algorithm: base letters first (`"a" < "B"`), then
//! accents (`"e" < "é"`), then case (`"a" < "A"`). The characters of `table.rs`, U+0020..U+024F,
//! U+0370..U+04FF, U+1E00..U+1EFF, U+2000..U+206F and U+20A0..U+20CF (Latin with Vietnamese,
//! Greek, Cyrillic, general punctuation, currency signs), take their weights from a table
//! generated from ICU, so strings made of them order exactly like `Intl.Collator("und")`, except
//! the few characters that expand to three or more collation elements (`¼`, `½`, `¾`, `ϗ`; the
//! table holds two). Everything else is approximate: combining marks (U+0300..U+036F) weigh
//! only as accents, so a decomposed `e` + U+0301 sorts next to `é` but not equal to it (ICU
//! normalizes first); other letters and digits sort after the table's letters (Hangul, kana and
//! Han last, in that order), and other symbols (emoji included) just before the digits, each
//! group by code point. Controls are ignored, as in ICU.

mod table;

use super::text;
use crate::str::VeltStr;
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

    /// Expected values from Node 22 (`new Intl.Collator("und").compare(a, b)`).
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
            ("Β", "α", Greater),
            ("Б", "а", Greater),
            ("ạ", "b", Less),
            ("ά", "α", Greater),
            ("ё", "е", Greater),
            ("α", "а", Less),
            ("中", "가", Greater),
            ("가", "あ", Less),
            ("あ", "中", Less),
            ("‼", "!!", Greater),
            ("€", "$", Greater),
            ("—", "-", Greater),
            ("…", "...", Greater),
            ("“a", "a", Less),
        ];
        for (a, b, want) in cases {
            assert_eq!(collate(a, b), want, "{a:?} vs {b:?}");
        }
    }

    /// A list sorted by Node 22's `Intl.Collator("und")`.
    #[test]
    fn sorts_a_fixture_like_node() {
        let node = [
            "_x",
            "-y",
            "😀",
            "10",
            "9",
            "ạch",
            "Ångel",
            "angle",
            "Ångström",
            "dž",
            "ǆ",
            "lodz",
            "łódź",
            "oeuvre",
            "Œuvre",
            "resume",
            "Resume",
            "résumé",
            "strasse",
            "Straße",
            "viet",
            "Việt",
            "Zoë",
            "zoo",
            "αλφα",
            "Άλφα",
            "βήτα",
            "елка",
            "ёлка",
            "жук",
            "москва",
            "Москва",
            "Ўзбек",
            "가나",
            "あい",
            "アイ",
            "一",
            "中文",
        ];
        let mut words = node.to_vec();
        words.reverse();
        words.sort_by(|a, b| collate(a, b));
        assert_eq!(words, node);
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
