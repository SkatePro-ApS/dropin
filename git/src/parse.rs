//! Pure helpers: parse git's output.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// `git log` format: fields split by 0x1f, records ended by 0x1e, so subjects may hold
/// anything. The body comes last, since it is the one field with line breaks.
pub const LOG_FORMAT: &str = "--format=%H%x1f%h%x1f%an%x1f%at%x1f%s%x1f%ae%x1f%ad%x1f%b%x1e";

/// `%ad` as PhpStorm's details show it, in the computer's own time zone: `13/10/2026 at 13:42`.
pub const LOG_DATE: &str = "--date=format-local:%d/%m/%Y at %H:%M";

/// A message body beyond this is cut: the details are a glance, `git show` has the rest.
const BODY_LIMIT: usize = 4_000;

/// One commit as `git log` lists it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub sha: String,
    pub short: String,
    pub author: String,
    pub email: String,
    /// Author time, in milliseconds since the epoch.
    pub at: i64,
    /// Author time as the details show it: `13/10/2026 at 13:42`.
    pub date: String,
    pub subject: String,
    /// The message after the subject, trimmed; '' when there is none.
    pub body: String,
}

/// One file a commit changed, as `git show --name-status` lists it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FileChange {
    /// Git's status letter: M, A, D, R, C or T; an uncommitted change can also be U (a conflict)
    /// or ? (untracked).
    pub status: String,
    pub path: String,
    /// The path before a rename or copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// Drops control characters and terminal escapes (`ESC[K` from git's progress lines), keeping
/// newlines and tabs.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // A CSI sequence ends at its first byte in @..~.
            while let Some(p) = chars.next() {
                if ('@'..='~').contains(&p) {
                    break;
                }
            }
        } else if !matches!(c, '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'..='\u{9f}') {
            out.push(c);
        }
    }
    out
}

/// Cuts to `n` characters with an ellipsis, never through a character.
pub fn fit(text: &str, n: usize) -> String {
    if text.chars().count() <= n {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(n.max(2) - 1).collect();
    cut.push('…');
    cut
}

/// `1 commit`, `3 commits`.
pub fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// Commits from `git log` run with LOG_FORMAT, in git's order (newest first).
pub fn parse_log(stdout: &str) -> Vec<Commit> {
    stdout
        .split('\x1e')
        .filter_map(|record| {
            let mut field = record.trim_start().split('\x1f');
            let sha = field.next().filter(|s| !s.is_empty())?;
            let short = field.next()?;
            let author = field.next().unwrap_or("");
            let at = field.next().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0) * 1000;
            let subject = field.next().unwrap_or("");
            let email = field.next().unwrap_or("");
            let date = field.next().unwrap_or("");
            let body: String = field.collect();
            Some(Commit {
                sha: sha.to_string(),
                short: short.to_string(),
                author: clean(author),
                email: clean(email),
                at,
                date: clean(date),
                subject: clean(subject),
                body: fit(clean(&body).trim(), BODY_LIMIT),
            })
        })
        .collect()
}

/// Files from `git show --name-status -z`: a status, then one path, or two for a rename or copy.
pub fn parse_name_status(stdout: &str) -> Vec<FileChange> {
    let parts: Vec<&str> = stdout.split('\0').collect();
    let mut files = Vec::new();
    let mut i = 0;
    while i < parts.len() {
        let status = parts[i].trim();
        if status.is_empty() {
            i += 1;
            continue;
        }
        let letter = &status[..1];
        if letter == "R" || letter == "C" {
            if let (Some(from), Some(path)) = (parts.get(i + 1), parts.get(i + 2)) {
                files.push(FileChange { status: letter.into(), path: path.to_string(), from: Some(from.to_string()) });
            }
            i += 3;
        } else {
            if let Some(path) = parts.get(i + 1) {
                files.push(FileChange { status: letter.into(), path: path.to_string(), from: None });
            }
            i += 2;
        }
    }
    files
}

/// Uncommitted changes from `git status --porcelain=v2 -z`, each against HEAD with the index and
/// work tree taken together, since a commit of a path takes its work-tree version whole. Status
/// letters as `parse_name_status` gives them, plus `U` for a conflict and `?` for an untracked file.
pub fn parse_status_v2(stdout: &str) -> Vec<FileChange> {
    let mut records = stdout.split('\0');
    let mut files = Vec::new();
    while let Some(record) = records.next() {
        let change = |status: &str, path: &str, from: Option<String>| FileChange { status: status.into(), path: path.into(), from };
        match record.as_bytes().first() {
            Some(b'1') => {
                // 1 XY sub mH mI mW hH hI path
                let fields: Vec<&str> = record.splitn(9, ' ').collect();
                let (Some(xy), Some(path)) = (fields.get(1), fields.get(8)) else { continue };
                let (x, y) = (&xy[..1], &xy[1..2]);
                let status = if x == "A" {
                    "A"
                } else if x == "D" || y == "D" {
                    "D"
                } else if x == "T" || y == "T" {
                    "T"
                } else {
                    "M"
                };
                files.push(change(status, path, None));
            }
            Some(b'2') => {
                // 2 XY sub mH mI mW hH hI Xscore path, then the path in HEAD as its own record.
                let fields: Vec<&str> = record.splitn(10, ' ').collect();
                let from = records.next().map(String::from);
                let (Some(xy), Some(path)) = (fields.get(1), fields.get(9)) else { continue };
                files.push(change(if xy.starts_with('C') { "C" } else { "R" }, path, from));
            }
            Some(b'u') => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 path
                if let Some(path) = record.splitn(11, ' ').nth(10) {
                    files.push(change("U", path, None));
                }
            }
            Some(b'?') => {
                if let Some(path) = record.strip_prefix("? ") {
                    files.push(change("?", path, None));
                }
            }
            _ => {}
        }
    }
    files
}

/// NUL-separated paths (`--name-only -z`, `ls-files -z`).
pub fn split_z(stdout: &str) -> Vec<String> {
    stdout.split('\0').filter(|p| !p.is_empty()).map(String::from).collect()
}

/// Incoming paths that also have local changes. Compared without case: on Windows two
/// spellings are one file, and a false stop is cheaper than a broken rebase.
pub fn overlap_of(incoming: &[String], local: &[String]) -> Vec<String> {
    let mine: HashSet<String> = local.iter().map(|p| p.to_lowercase()).collect();
    let mut hit: Vec<String> = incoming.iter().filter(|p| mine.contains(&p.to_lowercase())).cloned().collect();
    hit.sort();
    hit.dedup();
    hit
}

/// The patch ids `git patch-id --stable` prints, one per commit, sorted for comparing.
pub fn patch_ids_of(stdout: &str) -> Vec<String> {
    let mut ids: Vec<String> = stdout
        .lines()
        .filter_map(|line| line.split(' ').next())
        .filter(|id| (40..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(String::from)
        .collect();
    ids.sort();
    ids
}

/// What `before` holds that `after` lacks, counting repeats: a commit list's dropped subjects.
pub fn missing_from(before: &[String], after: &[String]) -> Vec<String> {
    let mut left: Vec<&String> = after.iter().collect();
    before
        .iter()
        .filter(|subject| match left.iter().position(|s| s == subject) {
            Some(at) => {
                left.remove(at);
                false
            }
            None => true,
        })
        .cloned()
        .collect()
}

/// The last few non-empty lines of git's output, for a notice.
pub fn tail(text: &str, lines: usize) -> String {
    let text = clean(&text.replace('\r', "\n"));
    let rows: Vec<&str> = text.split('\n').map(str::trim_end).filter(|r| !r.is_empty()).collect();
    rows[rows.len().saturating_sub(lines)..].join("\n")
}

/// One subject per line, as `git log --format=%s` prints them.
pub fn lines_of(stdout: &str) -> Vec<String> {
    stdout.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}
