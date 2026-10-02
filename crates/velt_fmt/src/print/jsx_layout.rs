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
    let mut i = parts.len().saturating_sub(1);
    while i > 0 {
        i -= 1;
        let (a, b, c) = (parts.get(i), parts.get(i + 1), parts.get(i + 2));
        let empty_between = matches!(b, Some(Part::Empty));
        let space = |p: Option<&Part>| matches!(p, Some(Part::Space(_)));
        let soft_or_hard = |p: Option<&Part>| p.is_some_and(Part::is_line);
        let hard = |p: Option<&Part>| matches!(p, Some(Part::Hard));
        let soft = |p: Option<&Part>| matches!(p, Some(Part::Soft));
        let pair_of_empties = matches!(a, Some(Part::Empty)) && empty_between;
        let pair_of_hardlines = hard(a) && empty_between && hard(c);
        let line_then_space = soft_or_hard(a) && empty_between && space(c);
        let space_then_line = space(a) && empty_between && soft_or_hard(c);
        let double_space = space(a) && empty_between && space(c);
        let soft_and_hard = empty_between && ((soft(a) && hard(c)) || (hard(a) && soft(c)));
        if double_space {
            // Prettier keeps one space here; both are kept so the text does not change.
            if let (Some(Part::Space(first)), Some(Part::Space(second))) = (a, c) {
                let joined = format!("{first}{second}");
                parts[i + 2] = Part::Space(joined);
            }
            parts.drain(i..i + 2);
        } else if (pair_of_hardlines && contains_text)
            || pair_of_empties
            || line_then_space
            || soft_and_hard
        {
            parts.drain(i..i + 2);
        } else if space_then_line {
            parts.drain(i + 1..i + 3);
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
