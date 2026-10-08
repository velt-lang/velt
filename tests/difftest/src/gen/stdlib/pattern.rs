//! `--std` statements with structured inputs: regular expressions from a small grammar and
//! date-times (epoch values, rolled-over fields, ISO strings, format patterns).
//!
//! The regex grammar never matches the empty string (every sequence has a mandatory atom), so
//! the known empty-match difference can't show; subjects have no `\r` and no supplementary
//! characters (JavaScript's non-`u` `.` and negated classes match half of a pair, #401), but
//! other non-ASCII text, so match offsets are checked in code units.

use super::StdGen;

/// Atoms that always consume one character.
const ATOMS: [&str; 16] = [
    "a",
    "b",
    "1",
    " ",
    "-",
    ".",
    "\\\\d",
    "\\\\w",
    "\\\\s",
    "\\\\D",
    "[ab]",
    "[^a\\\\n]",
    "[a-c1]",
    "[_ -]",
    "\\\\n",
    "\\\\.",
];
const SUBJECT: [&str; 13] = [
    "a", "b", "1", "2", " ", "_", "-", "\\n", "ab", ".", "é", "日本", "ü-",
];
const REPLACEMENTS: [&str; 8] = ["-", "$&", "[$1]", "$$", "<$`|$'>", "$2$1", "", "($<n>)"];
const DATE_TOKENS: [&str; 26] = [
    "YYYY", "YY", "M", "MM", "MMM", "MMMM", "D", "DD", "DDDD", "ddd", "dddd", "H", "HH", "h", "hh",
    "A", "a", "m", "mm", "s", "ss", "SSS", "Z", "ZZ", "[at]", "-",
];

impl StdGen {
    /// A pattern that can't match the empty string, and whether it has capture groups.
    fn regex(&mut self, depth: u32) -> (String, bool) {
        let mut out = String::new();
        let mut groups = false;
        let n = self.rng.range(1, 3);
        let anchor = self.rng.chance(15);
        if anchor {
            out.push_str(self.rng.pick(&["^", "\\\\b"]));
        }
        for i in 0..n {
            let (atom, g) = if depth > 0 && self.rng.chance(25) {
                self.group(depth - 1)
            } else {
                (self.rng.pick(&ATOMS).to_string(), false)
            };
            groups |= g;
            out.push_str(&atom);
            // The first atom stays mandatory; later ones may be optional.
            let quant = match (i, self.rng.below(8)) {
                (_, 0) => "+",
                (_, 1) => "+?",
                (_, 2) => "{1,3}",
                (_, 3) => "{2}",
                (0, _) => "",
                (_, 4) => "?",
                (_, 5) => "*",
                _ => "",
            };
            out.push_str(quant);
        }
        if self.rng.chance(10) {
            out.push_str(self.rng.pick(&["$", "\\\\b"]));
        }
        (out, groups)
    }

    /// `(…)`, `(?:…|…)` or `(?<n>…)` around non-empty alternatives.
    fn group(&mut self, depth: u32) -> (String, bool) {
        let (a, ga) = self.regex(depth);
        let (inner, gb) = match self.rng.chance(40) {
            true => {
                let (b, gb) = self.regex(depth);
                (format!("{a}|{b}"), gb)
            }
            false => (a, false),
        };
        match self.rng.below(3) {
            0 => (format!("({inner})"), true),
            1 => (format!("(?:{inner})"), ga || gb),
            _ => (format!("(?<n>{inner})"), true),
        }
    }

    pub(super) fn regex_stmt(&mut self) -> String {
        let (raw, groups) = self.regex(2);
        // Distinct group names: JS allows a repeated name only across alternatives, Rust never
        // (a known divergence, not generated).
        let mut pattern = String::new();
        for (i, part) in raw.split("(?<n>").enumerate() {
            if i > 0 {
                pattern.push_str(&format!("(?<n{i}>"));
            }
            pattern.push_str(part);
        }
        let flags: String = ["g", "i", "m", "s", "y"]
            .iter()
            .filter(|_| self.rng.chance(25))
            .copied()
            .collect();
        let subject = self.text(&SUBJECT, 10);
        let from = self.rng.range(0, 4);
        let re = format!("const re = new RegExp(\"{pattern}\", \"{flags}\");");
        match self.rng.below(5) {
            0 => format!("{re} console.log(re.test({subject}, {from}), showMatch(re.exec({subject}, {from})), re.groupCount);"),
            1 => format!("{re} console.log(JSON.stringify(re.matches({subject})), re.matchAll({subject}).map((m) => showMatch(m)).join(\" \"));"),
            2 => {
                let r = *self.rng.pick(&REPLACEMENTS);
                let r = match pattern.contains("(?<n1>") {
                    true => r.replace("$<n>", "$<n1>"),
                    false => r.replace("$<n>", "$1"),
                };
                format!("{re} console.log(JSON.stringify(re.replace({subject}, \"{r}\")), JSON.stringify(re.replaceAll({subject}, \"{r}\")));")
            }
            3 if !groups => {
                let limit = self.rng.range(0, 3);
                format!("{re} console.log(JSON.stringify(re.split({subject})), JSON.stringify(re.split({subject}, {limit})));")
            }
            _ => format!("{re} console.log(re.replaceWith({subject}, (m: RegExpMatch): string => `<${{m.index}}:${{m.value.length}}>`));"),
        }
    }

    /// Epoch milliseconds from years ~1653 to ~2286, biased to day and month edges.
    fn epoch_ms(&mut self) -> i64 {
        const DAY: i64 = 86_400_000;
        match self.rng.below(3) {
            0 => self.rng.range(-10_000_000_000_000, 10_000_000_000_000),
            1 => self.rng.range(-40_000, 40_000) * DAY + self.rng.range(-2, 2),
            _ => *self
                .rng
                .pick(&[0, -1, 951_782_400_000, 1_709_164_800_000, 4_107_542_400_000]),
        }
    }

    fn date_pattern(&mut self) -> String {
        let n = self.rng.range(1, 5);
        let parts: Vec<&str> = (0..n).map(|_| *self.rng.pick(&DATE_TOKENS)).collect();
        parts.join(*self.rng.pick(&[" ", ":", "/", "", ", "]))
    }

    fn iso_text(&mut self) -> String {
        let y = self.rng.range(1900, 2100);
        let (mo, d) = (self.rng.range(1, 12), self.rng.range(1, 28));
        let mut s = format!("{y:04}-{mo:02}");
        if self.rng.chance(80) {
            s.push_str(&format!("-{d:02}"));
        }
        if s.len() == 10 && self.rng.chance(70) {
            let (h, mi, sec) = (
                self.rng.range(0, 23),
                self.rng.range(0, 59),
                self.rng.range(0, 59),
            );
            s.push_str(&format!("T{h:02}:{mi:02}"));
            if self.rng.chance(70) {
                s.push_str(&format!(":{sec:02}"));
                if self.rng.chance(50) {
                    s.push_str(self.rng.pick(&[".5", ".25", ".125", ".999"]));
                }
            }
            s.push_str(self.rng.pick(&["Z", "", "+02:00", "-05:30", "+00:00", "z"]));
        }
        format!("\"{s}\"")
    }

    pub(super) fn datetime_stmt(&mut self) -> String {
        match self.rng.below(6) {
            0 => format!("showDate(DateTime.fromEpochMs({}));", self.epoch_ms()),
            1 => {
                let fields: Vec<String> = [
                    (1900, 2100),
                    (-14, 26),
                    (-40, 70),
                    (-30, 50),
                    (-100, 200),
                    (-100, 200),
                    (-2000, 3000),
                ]
                .iter()
                .map(|&(lo, hi)| self.rng.range(lo, hi).to_string())
                .collect();
                format!("showDate(DateTime.utc({}));", fields.join(", "))
            }
            2 => {
                let (t, n) = (self.epoch_ms(), self.rng.range(-30, 30));
                let unit = *self
                    .rng
                    .pick(&["Months", "Years", "Days", "Hours", "Minutes"]);
                format!("showDate(DateTime.fromEpochMs({t}).add{unit}({n}));")
            }
            3 => format!("showDate(DateTime.parse({}));", self.iso_text()),
            4 => {
                let (t, p) = (self.epoch_ms(), self.date_pattern());
                format!("console.log(DateTime.fromEpochMs({t}).format(\"{p}\"));")
            }
            _ => {
                let (a, b) = (self.epoch_ms(), self.epoch_ms());
                format!("const d = DateTime.fromEpochMs({a}).since(DateTime.fromEpochMs({b})); console.log(d.toString(), `${{d.toSeconds()}}`, JSON.stringify(DateTime.fromEpochMs({a}).startOfDay().parts()), Duration.ofMs({}).toString());", self.rng.range(-100_000_000, 100_000_000))
            }
        }
    }
}
