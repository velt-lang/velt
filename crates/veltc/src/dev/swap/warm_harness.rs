//! What the warm-against-cold test (`warm_tests.rs`) compares programs by, and the random body
//! edits it makes.

/// `vir` with every aggregate, extern and static named by what it is rather than by its index,
/// and their own lines left out: a kept lowering adds new ones after its own and keeps those an
/// edit no longer uses, where a cold lowering numbers what it meets in order. (An aggregate names
/// the aggregates of its fields; a cycle stays a bare `agg#…`.) Functions are named too, and
/// compared in name order.
pub(super) fn by_content(vir: &str) -> String {
    use std::collections::HashMap;
    let kinds = ["agg#", "extern#", "static#"];
    let mut defs: HashMap<String, String> = HashMap::new();
    for line in vir.lines() {
        for kind in kinds {
            if let Some(rest) = line.strip_prefix(kind) {
                if let Some((n, def)) = rest.split_once(' ') {
                    defs.insert(format!("{kind}{n}"), def.to_string());
                }
            }
        }
    }
    fn names(text: &str, defs: &HashMap<String, String>, open: &mut Vec<String>) -> String {
        let mut out = String::new();
        let mut rest = text;
        loop {
            let at = ["agg#", "extern#", "static#"]
                .iter()
                .filter_map(|k| rest.find(k).map(|i| (i, k.len())))
                .min();
            let Some((i, len)) = at else {
                out.push_str(rest);
                return out;
            };
            out.push_str(&rest[..i]);
            let digits = rest[i + len..]
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len() - i - len);
            let id = &rest[i..i + len + digits];
            match defs.get(id) {
                Some(def) if !open.iter().any(|o| o == id) => {
                    open.push(id.to_string());
                    let named = names(def, defs, open);
                    open.pop();
                    out.push_str(&format!("{}[{named}]", &id[..len - 1]));
                }
                _ => out.push_str(id),
            }
            rest = &rest[i + len + digits..];
        }
    }
    // Functions by name: `fn#N <linkage> <name>(…`.
    let mut fns: HashMap<String, String> = HashMap::new();
    for line in vir.lines() {
        if let Some(rest) = line.strip_prefix("fn#") {
            let mut words = rest.splitn(3, ' ');
            if let (Some(n), Some(_), Some(name)) = (words.next(), words.next(), words.next()) {
                let name = name.split('(').next().unwrap_or(name);
                fns.insert(format!("fn#{n}"), format!("fn[{name}]"));
            }
        }
    }
    let fn_names = |line: &str| -> String {
        let mut out = String::new();
        let mut rest = line;
        while let Some(i) = rest.find("fn#") {
            out.push_str(&rest[..i]);
            let digits = rest[i + 3..]
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len() - i - 3);
            let id = &rest[i..i + 3 + digits];
            out.push_str(fns.get(id).map_or(id, String::as_str));
            rest = &rest[i + 3 + digits..];
        }
        out.push_str(rest);
        out
    };
    // The functions in name order (a kept program keeps its numbering, adding new ones last),
    // each a block from its header to the next top-level line.
    let mut blocks: Vec<String> = vec![];
    for line in vir.lines() {
        if kinds.iter().any(|k| line.starts_with(k)) {
            continue;
        }
        let line = fn_names(&names(line, &defs, &mut vec![]));
        let top = !line.starts_with(' ') && !line.starts_with('}');
        match blocks.last_mut() {
            Some(b) if !top => b.push_str(&line),
            _ => blocks.push(line),
        }
        if let Some(b) = blocks.last_mut() {
            b.push('\n');
        }
    }
    // An internal function no code calls or names never runs: a kept program keeps those the
    // edit stopped reaching (some stay named by its tables), a cold one never builds them.
    blocks.retain(|b| !b.trim().is_empty());
    loop {
        let name = |b: &str| -> Option<String> {
            let header = b.lines().next()?;
            let rest = header.strip_prefix("fn[")?;
            let end = rest.find(']')?;
            let internal = rest[end..].starts_with("] internal ");
            internal.then(|| format!("fn[{}]", &rest[..end]))
        };
        let unused: Vec<usize> = (0..blocks.len())
            .filter(|&i| {
                name(&blocks[i]).is_some_and(|n| {
                    // Named in another block (past its header), or on a top-level line.
                    !(blocks.iter().enumerate()).any(|(j, b)| {
                        let body = match b.starts_with("fn[") {
                            true => b.split_once('\n').map_or("", |(_, rest)| rest),
                            false => b.as_str(),
                        };
                        j != i && body.contains(&n)
                    })
                })
            })
            .collect();
        if unused.is_empty() {
            break;
        }
        for i in unused.into_iter().rev() {
            blocks.remove(i);
        }
    }
    blocks.sort();
    blocks.concat()
}

/// A small deterministic generator (xorshift), for the random edits.
pub(super) struct Rng(pub(super) u64);

impl Rng {
    pub(super) fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n.max(1) as u64) as usize
    }
}

/// The byte ranges of the bodies of `src`'s functions and methods (the script's statements and
/// nested functions aside).
fn bodies(src: &str) -> Vec<(usize, usize)> {
    use velt_syntax::ast::ItemKind;
    let (module, _) = velt_syntax::parse_file(velt_common::FileId(0), src);
    let mut out = vec![];
    for item in &module.items {
        match &item.kind {
            ItemKind::Function(f) if !velt_syntax::is_script_main(&f.sig.name) => {
                out.push(f.body.span)
            }
            ItemKind::Class(t) | ItemKind::Struct(t) => {
                out.extend(t.constructor.as_ref().map(|c| c.body.span));
                out.extend(t.methods.iter().map(|m| m.decl.body.span));
            }
            _ => {}
        }
    }
    (out.into_iter())
        .map(|s| (s.lo as usize, s.hi as usize))
        .filter(|&(lo, hi)| lo < hi && hi <= src.len())
        .collect()
}

/// One random edit inside a body of `src` (the last one, or any): a number literal changed (to one of the same length,
/// or another), or a string literal changed, or nothing when the body it picked has neither.
pub(super) fn mutate(src: &str, rng: &mut Rng) -> Option<String> {
    let mut bodies = bodies(src);
    bodies.sort_unstable();
    // Half the edits are in the file's last function, which may change length and still reload
    // warm (an edit elsewhere that does refuses: it moves what follows).
    let pick = match rng.below(2) {
        0 => bodies.len().checked_sub(1)?,
        _ => rng.below(bodies.len()),
    };
    let &(lo, hi) = bodies.get(pick)?;
    let text = &src[lo..hi];
    let b = text.as_bytes();
    let word = |i: usize| b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.';
    let mut numbers = vec![];
    let mut strings = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !word(i - 1)) {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i == b.len() || !word(i) {
                numbers.push((start, i));
            }
            continue;
        }
        if b[i] == b'"' {
            let start = i;
            i += 1;
            while i < b.len() && b[i] != b'"' && b[i] != b'\\' && b[i] != b'\n' {
                i += 1;
            }
            if i < b.len() && b[i] == b'"' && i - start > 1 {
                strings.push((start + 1, i));
            }
        }
        i += 1;
    }
    let (s, e, new) = match rng.below(3) {
        0 | 1 if !numbers.is_empty() => {
            let (s, e) = numbers[rng.below(numbers.len())];
            let n: u64 = text[s..e].parse().ok()?;
            let new = match rng.below(3) {
                0 => (n % 9 + 1 + n / 10 * 10).to_string(),
                1 => (n + 1).to_string(),
                _ => (n * 10 + 7).to_string(),
            };
            (s, e, new)
        }
        _ if !strings.is_empty() => {
            let (s, e) = strings[rng.below(strings.len())];
            let new = match rng.below(2) {
                0 => format!("{}Z", &text[s..e - 1]),
                _ => format!("{}!", &text[s..e]),
            };
            (s, e, new)
        }
        _ => return None,
    };
    Some(format!(
        "{}{}{}{}",
        &src[..lo],
        &text[..s],
        new,
        &src[lo + e..]
    ))
}
