//! What prettier does with the children list of [`super::jsx_children`] (`printJsxElementInternal`):
//! neighbouring separators are merged, lines at the edges are trimmed, and in the multi-line form
//! a space that matters at the start, at the end or after a forced line break is written `{" "}`,
//! the only way it survives the line break; then the element is laid out on one line if it fits,
//! else over several.
//!
//! A component's children are kept as written (`exact`): it receives one child as itself and
//! several as an array, so splitting `"Save "` into `"Save"` and `{" "}` would change its props.
//! Such spaces stay text on their line, and a space at an edge stays next to its tag.

use super::jsx_children::Part;
use crate::doc::{
    cat, concat, conditional, fill, group, group_broken, hardline, if_break, indent, line, nil,
    softline, text, Doc,
};

/// Merges neighbouring separators (two separators with nothing between them) and trims lines
/// and empty contents at both ends. `contains_text`: is any child meaningful text?
pub(super) fn tidy(parts: &mut Vec<Part>, contains_text: bool) {
    // Prettier walks the list backwards and merges each part with the two after it, which are
    // already tidied. Those are kept on a stack (the next part on top), so the pass is linear.
    let mut rest: Vec<Part> = Vec::with_capacity(parts.len());
    let mut iter = std::mem::take(parts).into_iter().rev();
    rest.extend(iter.next());
    for a in iter {
        let n = rest.len();
        let (b, c) = (
            n.checked_sub(1).map(|i| &rest[i]),
            n.checked_sub(2).map(|i| &rest[i]),
        );
        match merge(&a, b, c, contains_text) {
            Merge::Keep => rest.push(a),
            Merge::DropBoth => {
                rest.pop();
            }
            Merge::JoinSpaces => {
                rest.pop();
                if let (Part::Space(first), Some(Part::Space(second))) = (&a, rest.last_mut()) {
                    *second = format!("{first}{second}");
                }
            }
            Merge::DropNextTwo => {
                rest.pop();
                rest.pop();
                rest.push(a);
            }
        }
    }
    rest.reverse();
    let edge = |p: &Part| matches!(p, Part::Empty | Part::Line | Part::Soft | Part::Hard);
    while rest.last().is_some_and(edge) {
        rest.pop();
    }
    let mut lead = 0;
    while rest.len() - lead > 1 && edge(&rest[lead]) && edge(&rest[lead + 1]) {
        lead += 2;
    }
    rest.drain(..lead);
    *parts = rest;
}

/// What happens to a part `a` and the two parts after it, `b` and `c`.
enum Merge {
    Keep,
    /// `a` and `b` go.
    DropBoth,
    /// Two spaces with nothing between: one space holding both (prettier keeps one; both are
    /// kept so the text does not change).
    JoinSpaces,
    /// `b` and `c` go.
    DropNextTwo,
}

fn merge(a: &Part, b: Option<&Part>, c: Option<&Part>, contains_text: bool) -> Merge {
    if !matches!(b, Some(Part::Empty)) {
        return Merge::Keep;
    }
    let space = |p: Option<&Part>| matches!(p, Some(Part::Space(_)));
    let soft_or_hard = |p: Option<&Part>| p.is_some_and(Part::is_line);
    let (hard, soft) = (
        |p: Option<&Part>| matches!(p, Some(Part::Hard)),
        |p: Option<&Part>| matches!(p, Some(Part::Soft)),
    );
    let a = Some(a);
    if space(a) && space(c) {
        Merge::JoinSpaces
    } else if (hard(a) && hard(c) && contains_text)
        || matches!(a, Some(Part::Empty))
        || (soft_or_hard(a) && space(c))
        || (soft(a) && hard(c))
        || (hard(a) && soft(c))
    {
        Merge::DropBoth
    } else if space(a) && soft_or_hard(c) {
        Merge::DropNextTwo
    } else {
        Merge::Keep
    }
}

/// Appends `doc` to the last content of `out`.
fn glue(out: &mut Vec<Doc>, doc: Doc) {
    let last = out.pop().unwrap_or_else(nil);
    out.push(cat![last, doc]);
}

/// `{"…"}`: spaces that must survive a line break next to them.
fn raw_space(s: &str) -> Doc {
    text(format!("{{\"{s}\"}}"))
}

/// A part as a document: a [`Part::Space`] prints as is while its line holds, else (unless
/// `exact`) as `{"…"}` and a line break.
pub(super) fn part_doc(part: &Part, exact: bool) -> Doc {
    match part {
        Part::Space(s) if exact => text(s.as_str()),
        Part::Empty => nil(),
        Part::Content(doc) => doc.clone(),
        Part::Line => line(),
        Part::Soft => softline(),
        Part::Hard => hardline(),
        Part::Space(s) => if_break(cat![raw_space(s), softline()], s.as_str()),
    }
}

/// The children of an element, ready to lay out.
pub(super) struct Children {
    pub parts: Vec<Part>,
    /// Is any child meaningful text (then the children are a paragraph fill)?
    pub contains_text: bool,
    /// Keep the children as written (a component's).
    pub exact: bool,
}

/// An element from its tags and children: on one line if it fits and nothing forces a break
/// (`forced`), else with the children indented on their own lines between the tags.
pub(super) fn element(open: Doc, close: Doc, children: Children, forced: bool) -> Doc {
    let Children {
        mut parts,
        contains_text,
        exact,
    } = children;
    tidy(&mut parts, contains_text);
    let (lead, trail) = if exact {
        peel_spaces(&mut parts)
    } else {
        (None, None)
    };
    let edge_text = |s: &Option<String>| s.as_deref().map_or_else(nil, text);
    if parts.is_empty() {
        return cat![open, edge_text(&lead), edge_text(&trail), close];
    }
    let (mut lines, breaks) = multiline(&parts, exact);
    // A space before the closing tag stays next to it: the last line measures them together.
    let tail = match &trail {
        Some(t) => {
            glue(&mut lines, cat![text(t.as_str()), close.clone()]);
            nil()
        }
        None => cat![hardline(), close.clone()],
    };
    let content = if contains_text {
        fill(lines)
    } else {
        group_broken(concat(lines))
    };
    let lead_or_break = lead.as_deref().map_or_else(hardline, text);
    let multi = group(cat![
        open.clone(),
        indent(cat![lead_or_break, content]),
        tail
    ]);
    if forced || breaks {
        return multi;
    }
    let flat: Vec<Doc> = parts.iter().map(|p| part_doc(p, exact)).collect();
    let flat = cat![
        open,
        edge_text(&lead),
        concat(flat),
        edge_text(&trail),
        close
    ];
    conditional(vec![group(flat), multi])
}

/// Takes the spaces at both edges out of `parts` (they stay next to the tags).
fn peel_spaces(parts: &mut Vec<Part>) -> (Option<String>, Option<String>) {
    let mut lead = None;
    if let [Part::Empty, Part::Space(s), ..] = parts.as_slice() {
        lead = Some(s.clone());
        parts.drain(..2);
    }
    let mut trail = None;
    if let Some(Part::Space(s)) = parts.last() {
        trail = Some(s.clone());
        parts.pop();
    }
    (lead, trail)
}

/// The fill parts of the multi-line form, and whether they contain a forced break.
fn multiline(parts: &[Part], exact: bool) -> (Vec<Doc>, bool) {
    let mut out = vec![nil()];
    let mut breaks = false;
    for (i, part) in parts.iter().enumerate() {
        if let (false, Part::Space(s)) = (exact, part) {
            let leading = i == 1 && matches!(parts[0], Part::Empty);
            if leading && parts.len() == 2 {
                glue(&mut out, raw_space(s));
                continue;
            }
            if leading {
                out.push(cat![raw_space(s), hardline()]);
                out.push(nil());
                continue;
            }
            let after_break =
                i >= 2 && matches!(parts[i - 1], Part::Empty) && matches!(parts[i - 2], Part::Hard);
            if i == parts.len() - 1 || after_break {
                glue(&mut out, raw_space(s));
                continue;
            }
        }
        let doc = part_doc(part, exact);
        breaks |= doc.breaks();
        if i % 2 == 0 {
            glue(&mut out, doc);
        } else {
            out.push(doc);
            out.push(nil());
        }
    }
    (out, breaks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(s: &str) -> Part {
        Part::Content(text(s))
    }

    fn shape(parts: &[Part]) -> String {
        parts
            .iter()
            .map(|p| match p {
                Part::Empty => "_".to_string(),
                Part::Content(_) => "w".to_string(),
                Part::Line => "line".to_string(),
                Part::Soft => "soft".to_string(),
                Part::Hard => "hard".to_string(),
                Part::Space(s) => format!("space({})", s.len()),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The quadratic algorithm `tidy` replaced (prettier's loop as written), for comparison.
    fn tidy_reference(parts: &mut Vec<Part>, contains_text: bool) {
        let mut i = parts.len().saturating_sub(1);
        while i > 0 {
            i -= 1;
            let (a, b, c) = (parts.get(i).cloned(), parts.get(i + 1), parts.get(i + 2));
            match a.map(|a| merge(&a, b, c, contains_text)) {
                Some(Merge::JoinSpaces) => {
                    if let (Some(Part::Space(first)), Some(Part::Space(second))) =
                        (parts.get(i).cloned(), parts.get(i + 2).cloned())
                    {
                        parts[i + 2] = Part::Space(format!("{first}{second}"));
                    }
                    parts.drain(i..i + 2);
                }
                Some(Merge::DropBoth) => {
                    parts.drain(i..i + 2);
                }
                Some(Merge::DropNextTwo) => {
                    parts.drain(i + 1..i + 3);
                }
                _ => {}
            }
        }
        let edge = |p: &Part| matches!(p, Part::Empty | Part::Line | Part::Soft | Part::Hard);
        while parts.last().is_some_and(edge) {
            parts.pop();
        }
        while parts.len() > 1 && edge(&parts[0]) && edge(&parts[1]) {
            parts.drain(..2);
        }
    }

    #[test]
    fn linear_tidy_matches_the_reference() {
        let kinds = [
            Part::Empty,
            Part::Line,
            Part::Soft,
            Part::Hard,
            Part::Space(" ".into()),
            Part::Space("  ".into()),
        ];
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for round in 0..2000 {
            let len = 1 + round % 23;
            let parts: Vec<Part> = (0..len)
                .map(|i| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    if i % 2 == 0 && seed.is_multiple_of(3) {
                        word("w")
                    } else {
                        kinds[(seed % kinds.len() as u64) as usize].clone()
                    }
                })
                .collect();
            for contains_text in [false, true] {
                let (mut fast, mut slow) = (parts.clone(), parts.clone());
                tidy(&mut fast, contains_text);
                tidy_reference(&mut slow, contains_text);
                let joined = |p: &[Part]| {
                    p.iter()
                        .map(|x| match x {
                            Part::Space(s) => format!("space({})", s.len()),
                            other => shape(std::slice::from_ref(other)),
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                assert_eq!(joined(&fast), joined(&slow), "{}", joined(&parts));
            }
        }
    }

    /// 200,000 children take milliseconds; the quadratic `tidy_reference` takes minutes. CPU
    /// time of this thread, which a loaded machine doesn't stretch the way it does wall-clock time.
    #[test]
    fn tidy_is_linear_in_the_number_of_children() {
        let mut parts = vec![Part::Empty];
        for _ in 0..200_000 {
            parts.extend([Part::Hard, Part::Empty, Part::Hard, word("w")]);
        }
        let cpu = super::super::thread_cpu::measure(|| tidy(&mut parts, true));
        assert!(cpu.as_secs() < 2, "{cpu:?} of CPU time");
        // Each `hard, "", hard` pair becomes one line break; the leading `"", hard` is trimmed.
        assert_eq!(parts.len(), 399_999);
    }

    #[test]
    fn separators_merge_and_edges_trim() {
        let mut parts = vec![
            Part::Empty,
            Part::Space(" ".into()),
            Part::Empty,
            Part::Space(" ".into()),
            word("a"),
            Part::Soft,
            Part::Empty,
            Part::Space(" ".into()),
            word("b"),
            Part::Hard,
            Part::Empty,
        ];
        tidy(&mut parts, true);
        assert_eq!(shape(&parts), "_ space(2) w space(1) w");
    }

    #[test]
    fn edge_spaces_become_containers_when_broken() {
        let parts = vec![
            Part::Empty,
            Part::Space(" ".into()),
            word("a"),
            Part::Space(" ".into()),
        ];
        let (docs, breaks) = multiline(&parts, false);
        assert!(!breaks);
        let rendered = crate::doc::render(&crate::doc::fill(docs), 80);
        assert_eq!(rendered, "{\" \"}\na{\" \"}");
    }
}
