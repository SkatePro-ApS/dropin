//! Partial changelists, as PhpStorm has them: one file's changes split into hunks that sit in
//! different lists, and a commit that takes only some of them. The rest stays uncommitted.
//!
//! Everything works on bytes, never on git's diff text: a latin1 or CRLF file comes out exactly as
//! it was. git's diff gives only the line numbers of each hunk.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::changes::*;
use crate::parse::*;
use crate::run::*;

/// The biggest file whose changes can be split.
const SPLIT_CAP: usize = 64 * 1024 * 1024;

/// One change in a file, as `git diff -U0` cuts them: a run of removed and added lines.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    /// Taken from the lines it removes and adds, so it stays the same while edits elsewhere in
    /// the file move it up or down.
    pub id: String,
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
}

/// The hunks of one file a commit takes; the file's other changes stay uncommitted.
#[derive(Clone, Debug, Deserialize)]
pub struct Part {
    pub path: String,
    pub hunks: Vec<String>,
}

/// Only a modified file can be split: a new, deleted or renamed one goes whole.
fn splittable(file: &FileChange) -> bool {
    file.status == "M"
}

/// `@@ -a[,b] +c[,d] @@` as numbers; a missing count is 1.
fn parse_head(line: &str) -> Option<(usize, usize, usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, _) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let pair = |s: &str| -> Option<(usize, usize)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (a, b) = pair(old)?;
    let (c, d) = pair(new)?;
    Some((a, b, c, d))
}

/// The 0-based index of a hunk's first line: with no lines, git names the line before it.
fn first(start: usize, count: usize) -> usize {
    if count == 0 { start } else { start.saturating_sub(1) }
}

/// Lines with their endings, so joining them gives the bytes back.
fn lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split_inclusive(|&b| b == b'\n').collect()
}

/// FNV-1a: small, stable across builds, and the same on every machine.
fn fnv(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for &byte in *part {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

/// A file at HEAD and in the work tree, the work tree as git sees it (its clean filters and
/// line-ending conversion applied), so the lines match what git diffs.
struct Sides {
    old: Vec<u8>,
    new: Vec<u8>,
}

fn sides(dir: &Path, path: &str) -> Result<Sides, String> {
    let old = git_bytes(dir, &["cat-file", "blob", &format!("HEAD:{path}")], SPLIT_CAP, "git cat-file")?;
    let sha = git(dir, &["hash-object", "-w", "--", path])?.must("git hash-object")?;
    let new = git_bytes(dir, &["cat-file", "blob", sha.trim()], SPLIT_CAP, "git cat-file")?;
    Ok(Sides { old, new })
}

/// The file's hunks with the lines they cover, in file order.
fn read_hunks(dir: &Path, path: &str, s: &Sides) -> Result<Vec<Hunk>, String> {
    let diff = git(dir, &["diff", "-U0", "--no-ext-diff", "--no-textconv", "--no-color", "HEAD", "--", path])?.must("git diff")?;
    let (old, new) = (lines(&s.old), lines(&s.new));
    let mut seen: Vec<u64> = Vec::new();
    let mut hunks = Vec::new();
    for line in diff.lines() {
        let Some((a, b, c, d)) = parse_head(line) else { continue };
        let (o, n) = (first(a, b), first(c, d));
        let (Some(removed), Some(added)) = (old.get(o..o + b), new.get(n..n + d)) else {
            return Err(format!("the changes in {path} do not match the file; read them again"));
        };
        let mut parts: Vec<&[u8]> = Vec::new();
        for l in removed {
            parts.push(b"-");
            parts.push(l);
        }
        for l in added {
            parts.push(b"+");
            parts.push(l);
        }
        let hash = fnv(&parts);
        // The same change twice in a file (two identical lines removed) gets a count.
        let twice = seen.iter().filter(|&&h| h == hash).count();
        seen.push(hash);
        let id = if twice == 0 { format!("{hash:016x}") } else { format!("{hash:016x}-{twice}") };
        hunks.push(Hunk { id, old_start: a, old_lines: b, new_start: c, new_lines: d });
    }
    Ok(hunks)
}

/// A modified file's hunks; none for a file that can only go whole.
pub fn file_hunks(dir: &Path, file: &FileChange) -> Result<Vec<Hunk>, String> {
    if !splittable(file) {
        return Ok(Vec::new());
    }
    let s = sides(dir, &file.path)?;
    read_hunks(dir, &file.path, &s)
}

/// The file as HEAD has it with only the `keep` hunks applied: what the commit stores.
pub fn partial_content(dir: &Path, path: &str, keep: &[String]) -> Result<Vec<u8>, String> {
    let s = sides(dir, path)?;
    let hunks = read_hunks(dir, path, &s)?;
    if let Some(gone) = keep.iter().find(|id| !hunks.iter().any(|h| &h.id == *id)) {
        return Err(format!("A change ticked in {path} is not there any more ({gone}). Check the list again."));
    }
    let (old, new) = (lines(&s.old), lines(&s.new));
    let mut out = Vec::with_capacity(s.old.len().max(s.new.len()));
    let mut at = 0;
    for h in hunks.iter().filter(|h| keep.contains(&h.id)) {
        let (o, n) = (first(h.old_start, h.old_lines), first(h.new_start, h.new_lines));
        for l in &old[at..o] {
            out.extend_from_slice(l);
        }
        for l in &new[n..n + h.new_lines] {
            out.extend_from_slice(l);
        }
        at = o + h.old_lines;
    }
    for l in &old[at..] {
        out.extend_from_slice(l);
    }
    Ok(out)
}

/// A separate index file in the repo's git folder, so the person's own index is never touched
/// while the commit is put together.
fn temp_index(dir: &Path) -> Result<PathBuf, String> {
    let at = git(dir, &["rev-parse", "--git-path", "dropin-index"])?.must("git rev-parse")?;
    let path = PathBuf::from(at.trim());
    Ok(if path.is_absolute() { path } else { dir.join(path) })
}

/// Commits `files` whole and, of each `parts` file, only the hunks named: those take HEAD's
/// version with just those hunks applied, and their other changes stay in the work tree. The
/// commit is built on a separate index, then the person's index takes the new HEAD for every path
/// committed, as `git commit --only` leaves it. With no parts this is `commit_changes`.
pub fn commit_parts(dir: &Path, message: &str, files: &[FileChange], parts: &[Part], amend: bool) -> Result<Committed, String> {
    if parts.is_empty() {
        return commit_changes(dir, message, files, amend);
    }
    if message.trim().is_empty() {
        return Err("Write a commit message first.".into());
    }
    let now = read_changes(dir)?;
    if now.merging {
        return Err("A merge is in progress, and git commits a merge only whole. Finish it from a terminal.".into());
    }
    let Some(head) = &now.head else {
        return Err("Part of a file can be committed only once the repo has a commit.".into());
    };
    if amend && head.pushed {
        return Err(format!("{} is pushed already; amending it would rewrite history others may have.", head.short));
    }
    for p in parts {
        match now.files.iter().find(|f| f.path == p.path) {
            Some(f) if splittable(f) => {}
            Some(_) => return Err(format!("{} can only be committed whole.", p.path)),
            None => return Err(format!("{} has no changes any more. Check the list again.", p.path)),
        }
        if p.hunks.is_empty() {
            return Err(format!("No change in {} is ticked.", p.path));
        }
    }
    let whole: Vec<&FileChange> = now
        .files
        .iter()
        .filter(|n| files.iter().any(|f| f.path == n.path) && !parts.iter().any(|p| p.path == n.path))
        .collect();
    let mut whole_paths: Vec<&str> = Vec::new();
    for f in &whole {
        if let Some(from) = &f.from {
            whole_paths.push(from);
        }
        whole_paths.push(&f.path);
    }

    // Each part's content first: a hunk that moved fails here, before anything is written.
    let mut contents = Vec::new();
    for p in parts {
        contents.push((p.path.as_str(), partial_content(dir, &p.path, &p.hunks)?));
    }

    let index = temp_index(dir)?;
    let _ = std::fs::remove_file(&index);
    let index_arg = index.to_string_lossy().into_owned();
    let env = [("GIT_INDEX_FILE", index_arg.as_str())];
    let note = message_file(message)?;
    let result = (|| -> Result<Ran, String> {
        git_env(dir, &["read-tree", "HEAD"], DEFAULT_TIMEOUT, None, &env)?.must("git read-tree")?;
        if !whole_paths.is_empty() {
            let list = nul_list(&whole_paths);
            git_env(dir, &["--literal-pathspecs", "add", "-A", "--pathspec-from-file=-", "--pathspec-file-nul"], DEFAULT_TIMEOUT, Some(list.as_bytes()), &env)?
                .must("git add")?;
        }
        for (path, bytes) in &contents {
            let tree = git(dir, &["ls-tree", "HEAD", "--", path])?.must("git ls-tree")?;
            let mode = tree.split_whitespace().next().unwrap_or("100644").to_string();
            let sha = git_env(dir, &["hash-object", "-w", "--stdin"], DEFAULT_TIMEOUT, Some(bytes), &[])?.must("git hash-object")?;
            let info = format!("{mode},{},{path}", sha.trim());
            git_env(dir, &["update-index", "--add", "--cacheinfo", &info], DEFAULT_TIMEOUT, None, &env)?.must("git update-index")?;
        }
        let note_arg = note.to_string_lossy().into_owned();
        let mut step = vec!["commit", "-F", &note_arg, "--cleanup=strip"];
        if amend {
            step.push("--amend");
        }
        let ran = git_env(dir, &step, COMMIT, None, &env)?;
        if !ran.ok() {
            return Err(format!("git commit failed: {}", tail(&format!("{}\n{}", ran.stderr, ran.stdout), 6)));
        }
        Ok(ran)
    })();
    let _ = std::fs::remove_file(&note);
    let _ = std::fs::remove_file(&index);
    let ran = result?;

    // The person's index: every committed path takes the new HEAD, so a split file's other
    // changes show as uncommitted and nothing committed is still staged.
    let mut all: Vec<&str> = whole_paths.clone();
    all.extend(parts.iter().map(|p| p.path.as_str()));
    let list = nul_list(&all);
    let _ = git_with(dir, &["--literal-pathspecs", "reset", "-q", "--pathspec-from-file=-", "--pathspec-file-nul"], DEFAULT_TIMEOUT, Some(&list));

    let line = git(dir, &["log", "-1", "--format=%H%x1f%h%x1f%s"])?.must("git log")?;
    let mut field = line.trim().split('\x1f');
    Ok(Committed {
        sha: field.next().unwrap_or_default().to_string(),
        short: field.next().unwrap_or_default().to_string(),
        subject: field.next().unwrap_or_default().to_string(),
        said: tail(&format!("{}\n{}", ran.stdout, ran.stderr), 6),
    })
}
