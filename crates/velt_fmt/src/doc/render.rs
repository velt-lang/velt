//! Lays out a [`Doc`] within a line width: a port of prettier's `printDocToString` / `fits`
//! (command stack, flat/break modes, re-measuring after hard lines, conditional groups, line
//! suffixes). Trailing spaces are trimmed at every newline.

use super::{Doc, LineKind, Node};

/// Spaces per indentation level.
const INDENT_WIDTH: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

#[derive(Clone, Copy)]
struct Cmd<'d> {
    indent: usize,
    mode: Mode,
    doc: &'d Doc,
}

impl<'d> Cmd<'d> {
    fn with(self, mode: Mode, doc: &'d Doc) -> Cmd<'d> {
        Cmd {
            indent: self.indent,
            mode,
            doc,
        }
    }
}

/// Renders `doc` so that lines stay within `width` columns where the layout allows it.
pub(crate) fn render(doc: &Doc, width: usize) -> String {
    let mut r = Renderer {
        width: width as isize,
        out: String::new(),
        pos: 0,
        cmds: vec![Cmd {
            indent: 0,
            mode: Mode::Break,
            doc,
        }],
        suffixes: vec![],
        remeasure: false,
    };
    r.run();
    trim_end_spaces(&mut r.out);
    r.out
}

struct Renderer<'d> {
    width: isize,
    out: String,
    /// Current column.
    pos: isize,
    cmds: Vec<Cmd<'d>>,
    /// Pending line-suffix content, flushed before the next newline.
    suffixes: Vec<Cmd<'d>>,
    /// Set after a hard line printed in flat mode: the following groups must be measured again.
    remeasure: bool,
}

impl<'d> Renderer<'d> {
    fn run(&mut self) {
        while let Some(cmd) = self.cmds.pop() {
            self.step(cmd);
            if self.cmds.is_empty() && !self.suffixes.is_empty() {
                let pending: Vec<_> = self.suffixes.drain(..).rev().collect();
                self.cmds.extend(pending);
            }
        }
    }

    fn step(&mut self, cmd: Cmd<'d>) {
        match cmd.doc.node() {
            Node::Nil | Node::BreakParent => {}
            Node::Text(s) => self.text(s),
            Node::Concat(parts) => self
                .cmds
                .extend(parts.iter().rev().map(|p| cmd.with(cmd.mode, p))),
            Node::Indent(d) => self.cmds.push(Cmd {
                indent: cmd.indent + INDENT_WIDTH,
                mode: cmd.mode,
                doc: d,
            }),
            Node::Group { contents, broken } => self.group(cmd, contents, *broken),
            Node::Conditional(states) => self.conditional(cmd, states),
            Node::Line(kind) => self.line(cmd, *kind),
            Node::IfBreak { broken, flat } => {
                let d = if cmd.mode == Mode::Break {
                    broken
                } else {
                    flat
                };
                self.cmds.push(cmd.with(cmd.mode, d));
            }
            Node::LineSuffix(d) => self.suffixes.push(cmd.with(cmd.mode, d)),
        }
    }

    fn text(&mut self, s: &str) {
        self.out.push_str(s);
        self.pos = match s.rfind('\n') {
            Some(nl) => text_width(&s[nl + 1..]),
            None => self.pos + text_width(s),
        };
    }

    fn group(&mut self, cmd: Cmd<'d>, contents: &'d Doc, broken: bool) {
        if cmd.mode == Mode::Flat && !self.remeasure {
            let mode = if broken { Mode::Break } else { Mode::Flat };
            self.cmds.push(cmd.with(mode, contents));
            return;
        }
        self.remeasure = false;
        let next = cmd.with(Mode::Flat, contents);
        if !broken && fits(next, &self.cmds, self.width - self.pos) {
            self.cmds.push(next);
        } else {
            self.cmds.push(cmd.with(Mode::Break, contents));
        }
    }

    fn conditional(&mut self, cmd: Cmd<'d>, states: &'d [Doc]) {
        let Some(first) = states.first() else {
            return;
        };
        if cmd.mode == Mode::Flat && !self.remeasure {
            self.cmds.push(cmd.with(Mode::Flat, first));
            return;
        }
        self.remeasure = false;
        let rem = self.width - self.pos;
        for state in states {
            let attempt = cmd.with(Mode::Flat, state);
            if fits(attempt, &self.cmds, rem) {
                self.cmds.push(attempt);
                return;
            }
        }
        let last = states.last().unwrap_or(first);
        self.cmds.push(cmd.with(Mode::Break, last));
    }

    fn line(&mut self, cmd: Cmd<'d>, kind: LineKind) {
        if cmd.mode == Mode::Flat {
            match kind {
                LineKind::Space => {
                    self.out.push(' ');
                    self.pos += 1;
                    return;
                }
                LineKind::Soft => return,
                LineKind::Hard => self.remeasure = true,
            }
        }
        if !self.suffixes.is_empty() {
            self.cmds.push(cmd);
            let pending: Vec<_> = self.suffixes.drain(..).rev().collect();
            self.cmds.extend(pending);
            return;
        }
        trim_end_spaces(&mut self.out);
        self.out.push('\n');
        self.out.extend(std::iter::repeat_n(' ', cmd.indent));
        self.pos = cmd.indent as isize;
    }
}

/// Does `next` fit in `width` columns, up to the first line break (continuing into the `rest` of
/// the command stack, in their own modes, when `next` ends first)?
fn fits(next: Cmd<'_>, rest: &[Cmd<'_>], mut width: isize) -> bool {
    let mut rest_idx = rest.len();
    let mut stack: Vec<(Mode, &Doc)> = vec![(next.mode, next.doc)];
    while width >= 0 {
        let Some((mode, doc)) = stack.pop() else {
            if rest_idx == 0 {
                return true;
            }
            rest_idx -= 1;
            stack.push((rest[rest_idx].mode, rest[rest_idx].doc));
            continue;
        };
        match doc.node() {
            Node::Nil | Node::BreakParent | Node::LineSuffix(_) => {}
            Node::Text(s) => match s.find('\n') {
                Some(nl) => return width - text_width(&s[..nl]) >= 0,
                None => width -= text_width(s),
            },
            Node::Concat(parts) => stack.extend(parts.iter().rev().map(|p| (mode, p))),
            Node::Indent(d) => stack.push((mode, d)),
            Node::Group { contents, broken } => {
                let mode = if *broken { Mode::Break } else { mode };
                stack.push((mode, contents));
            }
            Node::Conditional(states) => {
                let state = if mode == Mode::Break {
                    states.last()
                } else {
                    states.first()
                };
                if let Some(d) = state {
                    stack.push((mode, d));
                }
            }
            Node::Line(kind) => {
                if mode == Mode::Break || *kind == LineKind::Hard {
                    return true;
                }
                if *kind == LineKind::Space {
                    width -= 1;
                }
            }
            Node::IfBreak { broken, flat } => {
                stack.push((mode, if mode == Mode::Break { broken } else { flat }));
            }
        }
    }
    false
}

fn text_width(s: &str) -> isize {
    s.chars().count() as isize
}

fn trim_end_spaces(out: &mut String) {
    let trimmed = out.trim_end_matches([' ', '\t']).len();
    out.truncate(trimmed);
}
