//! The quick look the repo picker shows: one `git status` call.

use std::path::Path;
use std::time::Instant;

use serde_json::{json, Value};

use crate::run::git;

/// The work tree's top level for any folder inside it, as git prints it (`C:/dev/dropin`).
pub fn toplevel(dir: &Path) -> Result<String, String> {
    Ok(git(dir, &["rev-parse", "--show-toplevel"])?.must("git rev-parse")?.trim().to_string())
}

/// Branch, upstream, ahead/behind and the count of changed tracked files, from one git call.
pub fn status(dir: &Path) -> Result<Value, String> {
    let started = Instant::now();
    let text = git(dir, &["status", "--porcelain=v2", "--branch", "--untracked-files=no"])?.must("git status")?;
    let (mut branch, mut upstream, mut ahead, mut behind, mut changed) = (None, None, 0, 0, 0);
    for line in text.lines() {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            branch = Some(head.to_string());
        } else if let Some(up) = line.strip_prefix("# branch.upstream ") {
            upstream = Some(up.to_string());
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            // "+2 -1": ahead 2, behind 1.
            let mut parts = ab.split(' ');
            ahead = parts.next().and_then(|p| p.trim_start_matches('+').parse().ok()).unwrap_or(0);
            behind = parts.next().and_then(|p| p.trim_start_matches('-').parse().ok()).unwrap_or(0);
        } else if !line.starts_with('#') && !line.is_empty() {
            changed += 1;
        }
    }
    Ok(json!({
        "branch": branch,
        "upstream": upstream,
        "ahead": ahead,
        "behind": behind,
        "changed": changed,
        "ms": started.elapsed().as_millis() as u64,
    }))
}
