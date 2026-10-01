//! Line-based delta debugging. A program is a tree of *units*: a single line, or a line that opens
//! a block (more `{` than `}`) together with every line up to the one that closes it. The
//! shrinker removes chunks of sibling units (halving the chunk size, ddmin style), then recurses
//! into each surviving block's body and tries replacing a block by its body, until a fixpoint.
//! A candidate is kept only when the caller's predicate says the original failure still occurs.
//!
//! It relies on the generator's layout (one statement per line, blocks closing on their own line);
//! hand-written `velt fmt`-formatted files have the same shape.

/// Minimizes `text` while `fails` keeps returning true; returns the smallest failing program found.
pub fn shrink(text: &str, mut fails: impl FnMut(&str) -> bool) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    for _ in 0..4 {
        let before = lines.len();
        shrink_range(&mut lines, 0, None, &mut fails);
        if lines.len() == before {
            break;
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Shrinks the sibling units in `lines[start..end]` (`end` = `None` → to the end).
fn shrink_range(
    lines: &mut Vec<String>,
    start: usize,
    end: Option<usize>,
    fails: &mut impl FnMut(&str) -> bool,
) {
    let end_of = |lines: &Vec<String>, end: Option<usize>, removed: usize| {
        end.map_or(lines.len(), |e| e - removed)
    };
    let mut removed = 0;
    let mut chunk = units(lines, start, end_of(lines, end, 0)).len().max(1);
    while chunk >= 1 {
        let mut i = 0;
        loop {
            let us = units(lines, start, end_of(lines, end, removed));
            if i >= us.len() {
                break;
            }
            let (from, to) = (us[i].0, us[(i + chunk).min(us.len()) - 1].1);
            let mut candidate = lines.clone();
            candidate.drain(from..to);
            if fails(&render(&candidate)) {
                removed += to - from;
                *lines = candidate;
            } else {
                i += chunk;
            }
        }
        chunk /= 2;
    }
    // Recurse into blocks (last first, so earlier indices stay valid), then try unwrapping them.
    let us = units(lines, start, end_of(lines, end, removed));
    for &(from, to) in us.iter().rev() {
        if to - from < 3 {
            continue;
        }
        shrink_range(lines, from + 1, Some(to - 1), fails);
        let to = from + block_len(lines, from);
        let mut unwrapped = lines.clone();
        unwrapped.remove(to - 1);
        unwrapped.remove(from);
        if fails(&render(&unwrapped)) {
            *lines = unwrapped;
        }
    }
}

/// `(start, end)` line ranges of the sibling units in `lines[start..end]`.
fn units(lines: &[String], start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        let len = block_len(lines, i).min(end - i);
        out.push((i, i + len));
        i += len;
    }
    out
}

/// Number of lines of the unit starting at `i` (1 unless line `i` opens a block).
fn block_len(lines: &[String], i: usize) -> usize {
    let mut depth = 0i64;
    for (k, line) in lines[i..].iter().enumerate() {
        depth += net_braces(line);
        if depth <= 0 {
            return k + 1;
        }
    }
    lines.len() - i
}

fn net_braces(line: &str) -> i64 {
    line.chars()
        .map(|c| match c {
            '{' => 1,
            '}' => -1,
            _ => 0,
        })
        .sum()
}

fn render(lines: &[String]) -> String {
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::shrink;

    #[test]
    fn keeps_only_what_the_failure_needs() {
        let prog =
            "function main() {\n  let a = 1;\n  if (x) {\n    bad();\n    ok();\n  }\n  ok();\n}\n";
        let min = shrink(prog, |p| {
            p.contains("bad()") && p.matches('{').count() == p.matches('}').count()
        });
        assert_eq!(min, "    bad();\n");
    }

    #[test]
    fn respects_block_structure() {
        let prog = "a {\n  b {\n    X\n  }\n}\nc\n";
        let min = shrink(prog, |p| p.contains("X") && p.contains("b {"));
        assert_eq!(min, "  b {\n    X\n  }\n");
    }
}
