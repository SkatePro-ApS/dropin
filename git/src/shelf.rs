//! Shelves: uncommitted work set aside in git's stash while the branch moves under it, then put
//! back. Only the files in the way go on a shelf, and only when the person asks: nothing is
//! stashed behind anyone's back. A shelf that could not go back stays in the stash, listed.

use std::path::Path;

use serde::Serialize;

use crate::parse::*;
use crate::run::*;

/// Every shelf's stash message starts with this, so `shelves` lists Drop In's own and no others.
const MARK: &str = "dropin: shelved";

/// A shelf still in git's stash.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Shelf {
    pub sha: String,
    /// `stash@{n}` when it was read; the number moves as stashes come and go.
    pub name: String,
    /// What it was shelved for, as written in its message.
    pub why: String,
    /// When it was shelved, in ms.
    pub at: i64,
    pub files: Vec<String>,
}

/// How putting a shelf back went.
#[derive(Debug, PartialEq)]
pub enum Unshelved {
    /// Every change is back and the shelf is gone.
    Back,
    /// Back, but these files clash with what came in and hold conflict markers; the shelf stays.
    Clashed { sha: String, files: Vec<String> },
    /// git refused to put it back (a file changed meanwhile); nothing changed, and the shelf stays.
    Kept { sha: String, detail: String },
}

/// The files with uncommitted work, staged or not, and the unversioned ones.
pub(crate) fn changed_files(dir: &Path) -> Result<Vec<String>, String> {
    let list = |args: &[&str], what: &str| git(dir, args)?.must(what).map(|s| split_z(&s));
    let mut files = list(&["diff", "--name-only", "-z"], "git diff")?;
    files.extend(list(&["diff", "--cached", "--name-only", "-z"], "git diff --cached")?);
    files.extend(list(&["ls-files", "--others", "--exclude-standard", "-z"], "git ls-files")?);
    files.sort();
    files.dedup();
    Ok(files)
}

/// The files with uncommitted work that a move of the branch from `from` to `to` changes.
pub fn in_the_way(dir: &Path, from: &str, to: &str) -> Result<Vec<String>, String> {
    let moved = split_z(&git(dir, &["diff", "--name-only", "-z", from, to])?.must("git diff")?);
    Ok(overlap_of(&changed_files(dir)?, &moved))
}

fn stash_top(dir: &Path) -> String {
    git(dir, &["rev-parse", "-q", "--verify", "refs/stash"]).map(|r| r.stdout.trim().to_string()).unwrap_or_default()
}

/// Shelves the uncommitted work in `files`, staged and unversioned included, and leaves the rest
/// alone. Answers the shelf's sha.
pub fn shelve(dir: &Path, files: &[String], why: &str) -> Result<String, String> {
    let before = stash_top(dir);
    let message = format!("{MARK} {why}");
    let mut args = vec!["stash", "push", "--include-untracked", "-m", message.as_str(), "--"];
    args.extend(files.iter().map(String::as_str));
    git(dir, &args)?.must("git stash push")?;
    let after = stash_top(dir);
    if after.is_empty() || after == before {
        return Err("git stash push saved nothing".into());
    }
    Ok(after)
}

/// Puts a shelf back, staged files staged again, and drops it once it is all back.
pub fn unshelve(dir: &Path, sha: &str) -> Unshelved {
    let kept = |detail: String| Unshelved::Kept { sha: sha.to_string(), detail };
    let mut ran = match git(dir, &["stash", "apply", "--index", sha]) {
        Ok(ran) => ran,
        Err(e) => return kept(e),
    };
    // The staged side can clash where the files themselves merge (the upstream changed them):
    // then without --index, and a file that was staged whole is staged again.
    if !ran.ok() && ran.stderr.contains("--index") {
        ran = match git(dir, &["stash", "apply", sha]) {
            Ok(ran) => ran,
            Err(e) => return kept(e),
        };
        if ran.ok() {
            let names = |a: &str, b: &str| {
                git(dir, &["diff", "--name-only", "-z", a, b]).ok().filter(Ran::ok).map(|r| split_z(&r.stdout)).unwrap_or_default()
            };
            let partly = names(&format!("{sha}^2"), sha);
            let whole: Vec<String> = names(&format!("{sha}^1"), &format!("{sha}^2")).into_iter().filter(|f| !partly.contains(f)).collect();
            if !whole.is_empty() {
                let mut args = vec!["add", "--"];
                args.extend(whole.iter().map(String::as_str));
                let _ = git(dir, &args);
            }
        }
    }
    if ran.ok() {
        let _ = drop_shelf(dir, sha);
        return Unshelved::Back;
    }
    let clashed = git(dir, &["diff", "--name-only", "--diff-filter=U", "-z"])
        .ok()
        .filter(Ran::ok)
        .map(|r| split_z(&r.stdout))
        .unwrap_or_default();
    if clashed.is_empty() {
        return kept(tail(&format!("{}\n{}", ran.stderr, ran.stdout), 4));
    }
    Unshelved::Clashed { sha: sha.to_string(), files: clashed }
}

/// Drop In's shelves still in the stash, newest first.
pub fn shelves(dir: &Path) -> Result<Vec<Shelf>, String> {
    let ran = git(dir, &["stash", "list", "--format=%H%x1f%gd%x1f%gs%x1f%ct"])?;
    if !ran.ok() {
        return Ok(vec![]);
    }
    let mut found = vec![];
    for line in ran.stdout.lines() {
        let f: Vec<&str> = line.split('\x1f').collect();
        let [sha, name, subject, at] = f[..] else { continue };
        let Some(why) = subject.split_once(MARK).map(|(_, why)| why.trim().to_string()) else { continue };
        let files = git(dir, &["stash", "show", "--include-untracked", "--name-only", "-z", sha])
            .ok()
            .filter(Ran::ok)
            .map(|r| split_z(&r.stdout))
            .unwrap_or_default();
        found.push(Shelf {
            sha: sha.to_string(),
            name: name.to_string(),
            why,
            at: at.parse::<i64>().unwrap_or(0) * 1000,
            files,
        });
    }
    Ok(found)
}

/// Takes a shelf out of the stash. Its commit stays in the object store for a while:
/// `git stash apply <sha>` still brings it back.
pub fn drop_shelf(dir: &Path, sha: &str) -> Result<(), String> {
    let shelf = shelves(dir)?.into_iter().find(|s| s.sha == sha).ok_or("That shelf is gone from the stash already.")?;
    git(dir, &["stash", "drop", "-q", &shelf.name])?.must("git stash drop")?;
    Ok(())
}
