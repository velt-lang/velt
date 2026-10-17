//! `cargo xtask pr-lint`: the hygiene of a pull request's text and commits.
//!
//! Pull requests are squash-merged, so the title and description become the commit message on
//! `main`, in a public repository. This checks them, and the messages and authors of the
//! commits, for what review otherwise asks to remove by hand:
//!
//! - tool footers (`Generated with [Claude Code]…`) and tool trailers (`Co-authored-by:` …);
//! - links to chat or agent sessions (`claude.ai/…`, `chatgpt.com/…`);
//! - absolute paths of someone's machine (`/Users/<name>/`, `C:\Users\<name>\`, `/home/<name>/`,
//!   `/private/tmp/`);
//! - review-process wording ("review round", "addressed the review", "Stacked on #N");
//! - commits whose author is not the expected one (`--author`, `PR_LINT_AUTHORS`, else
//!   `git config user.email`).
//!
//! Text inside code (fenced blocks and inline `code` spans) is exempt from the wording rules, so
//! a description can quote what it talks about; machine paths are checked everywhere, because
//! pasted output is where they usually come from.

use std::path::Path;
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    ToolFooter,
    Trailer,
    ChatLink,
    MachinePath,
    ReviewWording,
    Author,
}

impl Rule {
    fn name(self) -> &'static str {
        match self {
            Rule::ToolFooter => "tool footer",
            Rule::Trailer => "tool or co-author trailer",
            Rule::ChatLink => "chat or session link",
            Rule::MachinePath => "absolute machine path",
            Rule::ReviewWording => "review-process wording",
            Rule::Author => "commit author",
        }
    }

    fn fix(self) -> &'static str {
        match self {
            Rule::ToolFooter => "remove the footer: the description becomes the commit message",
            Rule::Trailer => {
                "remove the trailer; commits carry the configured author only (reword a commit \
                 with `git rebase -i` or `git commit --amend`, then force-push)"
            }
            Rule::ChatLink => "remove the link and say in the text what matters from it",
            Rule::MachinePath => {
                "use a path relative to the repository, or a placeholder such as `<repo>/` or \
                 `$TMPDIR/`"
            }
            Rule::ReviewWording => {
                "say what the change does and how it was tested; review history and stacking \
                 notes belong in comments, not in what becomes the commit message"
            }
            Rule::Author => {
                "commit as the configured author: `git rebase --exec 'git commit --amend \
                 --no-edit --reset-author' <base>` with the right `user.email`, then force-push"
            }
        }
    }
}

#[derive(Debug)]
pub struct Violation {
    /// Where: `title`, `description` or `commit 1a2b3c4d`.
    pub place: String,
    /// 1-based line in that text (0 when the whole item is meant, as for an author).
    pub line: usize,
    pub rule: Rule,
    /// The offending text (or, for an author, what was found and expected).
    pub text: String,
}

/// Checks one text (a title, a description or a commit message). `place` names it in messages.
pub fn lint_text(place: &str, text: &str) -> Vec<Violation> {
    let mut out = vec![];
    let mut fence: Option<&str> = None;
    for (i, raw) in text.lines().enumerate() {
        let mut hit = |rule: Rule, found: &str| {
            out.push(Violation {
                place: place.to_string(),
                line: i + 1,
                rule,
                text: found.trim().to_string(),
            })
        };
        if let Some(p) = machine_path(raw) {
            hit(Rule::MachinePath, p);
        }
        let trimmed = raw.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = ["```", "~~~"].into_iter().find(|f| trimmed.starts_with(f)) {
            fence = Some(open);
            continue;
        }
        let prose = without_code_spans(raw);
        let lower = prose.to_lowercase();
        if is_tool_footer(&lower) {
            hit(Rule::ToolFooter, raw);
        } else if is_trailer(&lower) {
            hit(Rule::Trailer, raw);
        }
        if let Some(link) = chat_link(&lower) {
            hit(Rule::ChatLink, link);
        }
        if let Some(phrase) = review_wording(&lower) {
            hit(Rule::ReviewWording, phrase);
        }
    }
    out
}

pub struct Commit {
    pub sha: String,
    pub author_name: String,
    pub author_email: String,
    pub message: String,
}

impl Commit {
    fn place(&self) -> String {
        let subject = self.message.lines().next().unwrap_or("");
        let short = &self.sha[..self.sha.len().min(8)];
        format!("commit {short} (\"{subject}\")")
    }
}

/// Checks the commits' messages, and that each one's author email is one of `authors`
/// (compared ignoring ASCII case).
pub fn lint_commits(commits: &[Commit], authors: &[String]) -> Vec<Violation> {
    let mut out = vec![];
    for c in commits {
        let place = c.place();
        if !authors
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&c.author_email))
        {
            out.push(Violation {
                place: place.clone(),
                line: 0,
                rule: Rule::Author,
                text: format!(
                    "{} <{}>, expected {}",
                    c.author_name,
                    c.author_email,
                    authors.join(" or ")
                ),
            });
        }
        out.extend(lint_text(&place, &c.message));
    }
    out
}

/// The tool footers agents append: "Generated with [Claude Code](…)" and the like.
fn is_tool_footer(lower: &str) -> bool {
    const VERBS: &[&str] = &[
        "generated with",
        "generated by",
        "created with",
        "created by",
        "written with",
        "written by",
        "made with",
        "authored by",
    ];
    VERBS.iter().any(|v| {
        find_word(lower, v).is_some_and(|at| {
            let rest = &lower[at + v.len()..];
            TOOLS.iter().any(|t| find_word(rest, t).is_some())
        })
    })
}

/// Coding assistants and agents, as they are named in footers and links.
const TOOLS: &[&str] = &[
    "claude",
    "anthropic",
    "chatgpt",
    "openai",
    "codex",
    "copilot",
    "cursor",
    "gemini",
    "aider",
    "devin",
    "windsurf",
    "codeium",
];

/// A trailer line naming a co-author or a tool.
fn is_trailer(lower: &str) -> bool {
    const KEYS: &[&str] = &[
        "co-authored-by:",
        "generated-by:",
        "assisted-by:",
        "ai-assisted-by:",
        "made-with:",
        "claude-session:",
        "session-id:",
    ];
    let line = lower.trim_start_matches(|c: char| c.is_whitespace() || c == '>');
    KEYS.iter().any(|k| line.starts_with(k))
}

/// The first link to a chat or agent session.
fn chat_link(lower: &str) -> Option<&str> {
    const HOSTS: &[&str] = &[
        "claude.ai/",
        "chatgpt.com/",
        "chat.openai.com/",
        "gemini.google.com/",
        "g.co/gemini/",
        "copilot.microsoft.com/",
        "perplexity.ai/",
        "poe.com/",
        "grok.com/",
        "chat.deepseek.com/",
        "chat.mistral.ai/",
    ];
    HOSTS.iter().find_map(|h| {
        let mut from = 0;
        while let Some(at) = lower[from..].find(h).map(|a| a + from) {
            let before = lower[..at].chars().next_back();
            if !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
                let end = lower[at..]
                    .find(|c: char| c.is_whitespace() || c == ')' || c == '>')
                    .map_or(lower.len(), |e| at + e);
                return Some(&lower[at..end]);
            }
            from = at + h.len();
        }
        None
    })
}

/// Wording about the review process rather than the change. Kept to phrases that only describe
/// the process, so prose about reviewing (or a "review" of something in the code) passes.
fn review_wording(lower: &str) -> Option<&'static str> {
    const PHRASES: &[&str] = &[
        "review round",
        "round of review",
        "rounds of review",
        "addressed review",
        "addressed the review",
        "addressed all review",
        "addresses the review",
        "addressed reviewer",
        "addressed the reviewer",
        "review items addressed",
        "review comments addressed",
        "review feedback",
        "review findings",
        "review fixes",
        "per review",
        "per the review",
        "as requested in review",
        "as requested in the review",
        "as requested by the review",
        "independent review",
        "independent reviews",
        "adversarial review",
        "adversarial reviews",
        "stacked on #",
        "stacked on top of #",
        "restacked on #",
    ];
    PHRASES
        .iter()
        .find(|p| find_word(lower, p).is_some())
        .copied()
}

/// An absolute path of a particular machine or user: home directories (a user's name is
/// personal data), and macOS temporary directories. CI runners' homes are not personal.
fn machine_path(line: &str) -> Option<&str> {
    const PLACEHOLDERS: &[&str] = &[
        "runner", "shared", "user", "username", "name", "you", "me", "example",
    ];
    let mut best: Option<(usize, usize)> = None;
    let mut take = |at: usize, end: usize| {
        if best.is_none_or(|(b, _)| at < b) {
            best = Some((at, end));
        }
    };
    // After a home directory's parent: the length of `<user>/` (or `<user>\`), when that is a
    // name rather than a placeholder.
    let user_dir = |rest: &str, seps: &[char]| {
        let n = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
            .unwrap_or(rest.len());
        let user = rest[..n].to_ascii_lowercase();
        (n > 0 && rest[n..].starts_with(seps) && !PLACEHOLDERS.contains(&user.as_str()))
            .then_some(n + 1)
    };
    for prefix in ["/Users/", "/home/"] {
        let mut from = 0;
        while let Some(at) = line[from..].find(prefix).map(|a| a + from) {
            from = at + prefix.len();
            let before = line[..at].chars().next_back();
            if before.is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
                continue;
            }
            if let Some(n) = user_dir(&line[from..], &['/']) {
                take(at, from + n);
            }
        }
    }
    // Temporary directories, when a name follows (the bare directory names say nothing).
    for prefix in ["/private/tmp/", "/private/var/folders/", "/var/folders/"] {
        let mut from = 0;
        while let Some(at) = line[from..].find(prefix).map(|a| a + from) {
            from = at + prefix.len();
            if line[from..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
                take(at, from);
                break;
            }
        }
    }
    // `C:\Users\<name>\…` or `C:/Users/<name>/…`: a drive letter, then `Users`.
    let bytes = line.as_bytes();
    for at in 0..bytes.len().saturating_sub(8) {
        let drive = bytes[at].is_ascii_alphabetic()
            && bytes[at + 1] == b':'
            && matches!(bytes[at + 2], b'\\' | b'/')
            && (at == 0 || !bytes[at - 1].is_ascii_alphanumeric());
        if drive
            && line.is_char_boundary(at + 8)
            && line[at + 3..at + 8].eq_ignore_ascii_case("users")
            && matches!(bytes[at + 8], b'\\' | b'/')
        {
            if let Some(n) = user_dir(&line[at + 9..], &['\\', '/']) {
                take(at, at + 9 + n);
                break;
            }
        }
    }
    best.map(|(at, end)| {
        // Show the path up to where it ends, not just the matched prefix.
        let tail = line[end..]
            .find(|c: char| c.is_whitespace() || matches!(c, '`' | '"' | '\'' | ')'))
            .map_or(line.len(), |e| end + e);
        &line[at..tail]
    })
}

/// `line` with the contents of inline code spans blanked out.
fn without_code_spans(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let ticks = rest[open..].len() - rest[open..].trim_start_matches('`').len();
        let delim = &rest[open..open + ticks];
        let body = &rest[open + ticks..];
        match body.find(delim) {
            Some(close) => {
                out.push_str(&rest[..open]);
                out.push(' ');
                rest = &body[close + ticks..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// The first occurrence of `word` in `text` that does not continue a longer word on its left
/// or right (`per review` is found in "fixed per review." but not in "upper reviewer").
fn find_word(text: &str, word: &str) -> Option<usize> {
    // `_` is not part of a word here: it is Markdown emphasis (`_Written by Codex_`).
    let is_word = |c: char| c.is_alphanumeric();
    let mut from = 0;
    while let Some(at) = text[from..].find(word).map(|a| a + from) {
        let end = at + word.len();
        let left = text[..at].chars().next_back().is_none_or(|c| !is_word(c));
        let right = word.ends_with(|c: char| !is_word(c))
            || text[end..].chars().next().is_none_or(|c| !is_word(c));
        if left && right {
            return Some(at);
        }
        from = at + word.len().max(1);
    }
    None
}

/// Prints each violation with its fix; the summary line when there are any.
pub fn report(violations: &[Violation]) -> String {
    let mut s = String::new();
    for v in violations {
        let line = if v.line > 0 {
            format!(", line {}", v.line)
        } else {
            String::new()
        };
        s.push_str(&format!(
            "{}{line}: {}: {}\n  fix: {}\n",
            v.place,
            v.rule.name(),
            v.text,
            v.rule.fix()
        ));
    }
    s
}

const USAGE: &str = "\
cargo xtask pr-lint [options]

Checks a pull request's title and description (PR_TITLE, PR_BODY, or the options below) and the
messages and authors of its commits (`git log <base>..<head>`).

Options:
  --title <text>         The title (default: $PR_TITLE)
  --body-file <file>     Read the description from a file (default: $PR_BODY)
  --base <rev>           Commits after this revision (default: origin/main, else main)
  --head <rev>           Up to this revision (default: HEAD)
  --author <email>       An expected commit author email; repeat for several (default:
                         $PR_LINT_AUTHORS, comma-separated, else `git config user.email`)
  --no-commits           Check only the title and description
";

/// `cargo xtask pr-lint <args>`.
pub fn main(root: &Path, args: &[String]) -> Result<(), String> {
    let mut title = std::env::var("PR_TITLE").ok();
    let mut body = std::env::var("PR_BODY").ok();
    let mut base = None;
    let mut head = "HEAD".to_string();
    let mut authors: Vec<String> = vec![];
    let mut commits = true;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--title" => title = Some(value("--title")?),
            "--body-file" => {
                let path = value("--body-file")?;
                body = Some(std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?);
            }
            "--base" => base = Some(value("--base")?),
            "--head" => head = value("--head")?,
            "--author" => authors.push(value("--author")?),
            "--no-commits" => commits = false,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            other => return Err(format!("pr-lint: unknown option `{other}`\n\n{USAGE}")),
        }
    }

    let mut violations = vec![];
    let mut checked = vec![];
    if let Some(t) = title.as_deref().filter(|t| !t.is_empty()) {
        violations.extend(lint_text("title", t));
        checked.push("the title".to_string());
    }
    if let Some(b) = body.as_deref().filter(|b| !b.is_empty()) {
        violations.extend(lint_text("description", b));
        checked.push("the description".to_string());
    }
    if commits {
        if authors.is_empty() {
            authors = expected_authors(root)?;
        }
        let base = match base {
            Some(b) => b,
            None => crate::changes::default_base(root)
                .ok_or("pr-lint: no origin/main or main to compare with; pass --base")?,
        };
        let list = log(root, &format!("{base}..{head}"))?;
        violations.extend(lint_commits(&list, &authors));
        checked.push(format!("{} commit(s) in {base}..{head}", list.len()));
    }
    if checked.is_empty() {
        return Err(format!("pr-lint: nothing to check\n\n{USAGE}"));
    }
    println!("pr-lint checked {}", checked.join(", "));
    if violations.is_empty() {
        println!("\x1b[32mno problems\x1b[0m");
        return Ok(());
    }
    print!("{}", report(&violations));
    Err(format!(
        "pr-lint: {} problem(s); the title and description become the commit message on main",
        violations.len()
    ))
}

/// `PR_LINT_AUTHORS` (comma-separated), else the configured `user.email`.
fn expected_authors(root: &Path) -> Result<Vec<String>, String> {
    let from_env: Vec<String> = std::env::var("PR_LINT_AUTHORS")
        .unwrap_or_default()
        .split(',')
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .collect();
    if !from_env.is_empty() {
        return Ok(from_env);
    }
    let email = git(root, &["config", "user.email"]).unwrap_or_default();
    let email = email.trim();
    if email.is_empty() {
        return Err(
            "pr-lint: no expected author: pass --author, set PR_LINT_AUTHORS or user.email".into(),
        );
    }
    Ok(vec![email.to_string()])
}

/// The commits in `range`, oldest first.
fn log(root: &Path, range: &str) -> Result<Vec<Commit>, String> {
    let text = git(
        root,
        &[
            "log",
            "--reverse",
            "--format=%H%x1f%an%x1f%ae%x1f%B%x1e",
            range,
        ],
    )?;
    Ok(parse_log(&text))
}

fn parse_log(text: &str) -> Vec<Commit> {
    text.split('\x1e')
        .filter_map(|rec| {
            let mut f = rec.trim_start_matches('\n').splitn(4, '\x1f');
            let sha = f.next()?.trim();
            if sha.is_empty() {
                return None;
            }
            Some(Commit {
                sha: sha.to_string(),
                author_name: f.next()?.to_string(),
                author_email: f.next()?.to_string(),
                message: f.next()?.trim_end().to_string(),
            })
        })
        .collect()
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git {}: {}", args.join(" "), err.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
#[path = "pr_lint_tests.rs"]
mod tests;
