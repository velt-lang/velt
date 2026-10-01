//! `switch` statements: on integers, strings, the literal union `Lvl`, the enum `Col`, the
//! discriminant of `Sh` and `typeof` of an `i64 | string` union.
//!
//! Cases group one or two labels, `default` may sit anywhere, and each body either ends in
//! `break;` or falls through into the next case. A switch over a tag without `default` covers
//! every member (Velt requires exhaustiveness; JS doesn't care). Case bodies see the switched
//! local narrowed to the members that can reach them — their own labels plus everything falling
//! into them — so `v.x` / `v.s` / the narrowed union are in scope exactly where both languages
//! allow them; a `default` no member reaches may assert `const n: never = v;`.

use super::scope::{Ty, Var};
use super::tagged::{has_x, COLORS, LEVELS, SHAPE_KINDS};
use super::Gen;

/// What a switch dispatches on.
enum Subject {
    /// Integer or string values (never exhaustive): labels are literals.
    Values(Vec<String>),
    /// A tag with a finite set of members, and the local the cases narrow, if any.
    Tags {
        members: Vec<&'static str>,
        label: fn(&str) -> String,
        narrow: Option<Narrowed>,
    },
}

/// A local that `switch` narrows in each case.
struct Narrowed {
    name: String,
    ty: Ty,
    /// Its narrowed fields / member type may be used: the local can't be reassigned (a
    /// reassignment in the body would end the narrowing).
    usable: bool,
}

/// One `case` group: its labels (`None` = `default`).
struct Case {
    labels: Vec<Option<usize>>,
}

impl Gen {
    /// A random `switch` statement.
    pub(super) fn switch_stmt(&mut self) {
        let (disc, subject) = self.switch_subject();
        let n = match &subject {
            Subject::Values(v) => v.len(),
            Subject::Tags { members, .. } => members.len(),
        };
        let exhaustive = matches!(subject, Subject::Tags { .. }) && self.rng.chance(40);
        let cases = self.case_groups(n, exhaustive);
        self.open(&format!("switch ({disc}) {{"));
        let mut reaching: Vec<usize> = Vec::new();
        let mut falls = false;
        for (i, case) in cases.iter().enumerate() {
            if !falls {
                reaching.clear();
            }
            for (j, label) in case.labels.iter().enumerate() {
                let text = match label {
                    Some(k) => format!("case {}:", label_text(&subject, *k)),
                    None => "default:".into(),
                };
                let last = j + 1 == case.labels.len();
                if last {
                    self.open(&format!("{text} {{"));
                } else {
                    self.line(&text);
                }
            }
            reaching.extend(case_members(case, &cases, n));
            reaching.sort_unstable();
            reaching.dedup();
            let hidden = self.narrow_case(&subject, &reaching);
            self.block(3);
            if let Some(mark) = hidden {
                self.scope.restore(mark);
            }
            falls = i + 1 < cases.len() && self.rng.chance(40);
            if !falls {
                self.line("break;");
            }
            self.close("}");
        }
        self.close("}");
    }

    /// The discriminant expression and what its cases can be.
    fn switch_subject(&mut self) -> (String, Subject) {
        match self.rng.below(6) {
            0 => {
                let e = self.int(2);
                let values = (-2..=4).map(|k: i64| k.to_string()).collect();
                (format!("(({e}) % 5)"), Subject::Values(values))
            }
            1 => {
                let e = self.string(2).text;
                let values = ["\"\"", "\"a\"", "\"ab\"", "\"hello\"", "\"Q\"", "\"42\""];
                let values = values.iter().map(|s| s.to_string()).collect();
                (e, Subject::Values(values))
            }
            2 => self.shape_subject(),
            3 => self.typeof_subject(),
            4 => {
                let label: fn(&str) -> String = |m| format!("\"{m}\"");
                let mut vars = self.unnarrowed(Ty::Lvl);
                if vars.is_empty() {
                    // Not `"lo" as Lvl`: Velt has no type assertions (gap).
                    let value = self.tag_value(Ty::Lvl);
                    let fresh = self.bind("c", Ty::Lvl, false);
                    self.line(&format!("const {fresh}: Lvl = {value};"));
                    vars = self.unnarrowed(Ty::Lvl);
                }
                let l = self.rng.pick(&vars).clone();
                (l.name.clone(), tags(LEVELS.to_vec(), label, narrowed(&l)))
            }
            _ => {
                let c = self.tag_value(Ty::Col);
                let label: fn(&str) -> String = |m| format!("Col.{m}");
                (c, tags(COLORS.to_vec(), label, None))
            }
        }
    }

    /// `switch (v.kind)`: narrowing is only generated for a local that can't be reassigned in
    /// the cases and outside closures.
    fn shape_subject(&mut self) -> (String, Subject) {
        let vars = self.unnarrowed(Ty::Shape);
        let Some(v) = (!vars.is_empty()).then(|| self.rng.pick(&vars).clone()) else {
            let lit = self.shape_literal(1);
            let fresh = self.bind("c", Ty::Shape, false);
            self.line(&format!("const {fresh}: Sh = {lit};"));
            return self.shape_subject();
        };
        let label: fn(&str) -> String = |m| format!("\"{m}\"");
        (
            format!("{}.kind", v.name),
            tags(SHAPE_KINDS.to_vec(), label, narrowed(&v)),
        )
    }

    /// `switch (typeof u)` over an `i64 | string` local (an integer switch when none is in scope:
    /// a `typeof` case no member can match is an error in Velt).
    fn typeof_subject(&mut self) -> (String, Subject) {
        let vars = self.unnarrowed(Ty::Union);
        if vars.is_empty() {
            let values = (0..3).map(|k: i64| k.to_string()).collect();
            return (format!("({} & 3)", self.int(1)), Subject::Values(values));
        }
        let u = self.rng.pick(&vars).clone();
        let label: fn(&str) -> String = |m| format!("\"{m}\"");
        (
            format!("typeof {}", u.name),
            tags(vec!["number", "string"], label, narrowed(&u)),
        )
    }

    /// Splits a random subset of `n` labels (all of them when `exhaustive`), plus `default`
    /// unless exhaustive, into case groups of one or two labels, in random order.
    fn case_groups(&mut self, n: usize, exhaustive: bool) -> Vec<Case> {
        let mut labels: Vec<Option<usize>> = (0..n)
            .filter(|_| exhaustive || self.rng.chance(60))
            .map(Some)
            .collect();
        if !exhaustive {
            labels.push(None);
        }
        for i in (1..labels.len()).rev() {
            let j = self.rng.below(i + 1);
            labels.swap(i, j);
        }
        let mut cases = Vec::new();
        let mut rest = labels.as_slice();
        while !rest.is_empty() {
            let take = if rest.len() > 1 && self.rng.chance(30) {
                2
            } else {
                1
            };
            cases.push(Case {
                labels: rest[..take].to_vec(),
            });
            rest = &rest[take..];
        }
        cases
    }

    /// Declares what the case body may use of the narrowed local, given the members reaching it:
    /// a partially narrowed `Sh` shadows the local (so no tag test the narrowing makes
    /// impossible — an error in TS and Velt — is generated on it) and exposes its fields; a
    /// `never` local is hidden (returns the exclusion mark to restore after the body).
    fn narrow_case(&mut self, subject: &Subject, reaching: &[usize]) -> Option<usize> {
        let Subject::Tags {
            members,
            narrow: Some(Narrowed { name, ty, usable }),
            ..
        } = subject
        else {
            return None;
        };
        let names: Vec<&str> = reaching.iter().map(|&i| members[i]).collect();
        if names.is_empty() {
            if *usable && *ty == Ty::Shape && self.rng.chance(60) {
                let n = self.scope.fresh("nv");
                self.line(&format!("const {n}: never = {name};"));
            }
            return Some(self.scope.exclude(name));
        }
        if names.len() == members.len() {
            return None;
        }
        match (*ty, names.as_slice(), *usable) {
            // Converting a narrowed literal back to `Lvl` fails VIR verification (bug
            // narrowed-literal-widen): the local isn't used in the case at all.
            (Ty::Lvl, _, _) => return Some(self.scope.exclude(name)),
            (Ty::Union, ["number"], true) => self.scope.declare_narrowed(name, Ty::Int),
            (Ty::Union, ["string"], true) => self.scope.declare_narrowed(name, Ty::Str),
            _ => self.scope.declare_narrowed(name, *ty),
        }
        if *ty == Ty::Shape && *usable {
            if names.iter().all(|k| has_x(k)) {
                self.scope.declare_narrowed(&format!("{name}.x"), Ty::Int);
            }
            if names == ["box"] {
                self.scope.declare_narrowed(&format!("{name}.s"), Ty::Str);
            }
        }
        None
    }
}

fn tags(
    members: Vec<&'static str>,
    label: fn(&str) -> String,
    narrow: Option<Narrowed>,
) -> Subject {
    Subject::Tags {
        members,
        label,
        narrow,
    }
}

/// The narrowing a switch on local `v` performs.
fn narrowed(v: &Var) -> Option<Narrowed> {
    Some(Narrowed {
        name: v.name.clone(),
        ty: v.ty,
        usable: !v.rebind,
    })
}

fn label_text(subject: &Subject, k: usize) -> String {
    match subject {
        Subject::Values(values) => values[k].clone(),
        Subject::Tags { members, label, .. } => label(members[k]),
    }
}

/// The members a case group matches: its labels, or for `default` every member no case names.
fn case_members(case: &Case, cases: &[Case], n: usize) -> Vec<usize> {
    let named: Vec<usize> = cases
        .iter()
        .flat_map(|c| c.labels.iter().flatten().copied())
        .collect();
    case.labels
        .iter()
        .flat_map(|l| match l {
            Some(k) => vec![*k],
            None => (0..n).filter(|k| !named.contains(k)).collect(),
        })
        .collect()
}
