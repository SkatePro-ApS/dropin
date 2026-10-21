//! The Branches column: the repo's local and remote-tracking branches, and the steps on them
//! (switch, create, rename, delete here and on the remote). A refusal is a `Notice` with git's
//! own say; `Err` is only for a git that could not run.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::ops::{Notice, Tone};
use crate::parse::*;
use crate::run::*;

const SWITCH: Duration = Duration::from_secs(120);
const PUSH: Duration = Duration::from_secs(180);

/// Fields of one `for-each-ref` line, split by \x1f.
const REF_FORMAT: &str = "--format=%(refname)%1f%(upstream:short)%1f%(upstream:track,nobracket)%1f%(HEAD)%1f\
                          %(objectname:short)%1f%(committerdate:unix)%1f%(symref)%1f%(worktreepath)%1f%(contents:subject)";

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Branch {
    /// Without the remote: `feat/x` for both `refs/heads/feat/x` and `refs/remotes/origin/feat/x`.
    pub name: String,
    /// `origin` for a remote-tracking branch, empty for a local one.
    pub remote: String,
    /// `origin/main`; empty when none is set.
    pub upstream: String,
    pub ahead: usize,
    pub behind: usize,
    /// The upstream is set but no longer exists.
    pub gone: bool,
    /// Checked out here.
    pub current: bool,
    /// The remote's default branch (its HEAD).
    pub default: bool,
    pub sha: String,
    /// Committer date of the tip, in ms.
    pub at: i64,
    pub subject: String,
    /// Checked out in another worktree, which git will not switch to.
    pub worktree: String,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Branches {
    /// The current branch; empty when HEAD is detached.
    pub head: String,
    /// HEAD's short sha when detached.
    pub detached: String,
    pub local: Vec<Branch>,
    pub remote: Vec<Branch>,
}

/// `ahead 2, behind 1` / `gone` / empty, as `%(upstream:track,nobracket)` prints it.
fn parse_track(track: &str) -> (usize, usize, bool) {
    let mut found = (0, 0, track.trim() == "gone");
    for part in track.split(',') {
        let mut words = part.split_whitespace();
        match (words.next(), words.next().and_then(|n| n.parse().ok())) {
            (Some("ahead"), Some(n)) => found.0 = n,
            (Some("behind"), Some(n)) => found.1 = n,
            _ => {}
        }
    }
    found
}

/// Branches from `git for-each-ref` run with REF_FORMAT. A remote's `HEAD` ref is not listed;
/// it marks the branch it points to as that remote's default.
pub fn parse_branches(stdout: &str) -> Branches {
    let mut found = Branches::default();
    let mut defaults = Vec::new();
    for line in stdout.lines() {
        let f: Vec<&str> = line.split('\x1f').collect();
        if f.len() < 9 {
            continue;
        }
        let (ahead, behind, gone) = parse_track(f[2]);
        let mut b = Branch {
            upstream: f[1].to_string(),
            ahead,
            behind,
            gone,
            current: f[3] == "*",
            sha: f[4].to_string(),
            at: f[5].parse::<i64>().unwrap_or(0) * 1000,
            subject: f[8].to_string(),
            ..Branch::default()
        };
        if let Some(name) = f[0].strip_prefix("refs/heads/") {
            b.name = name.to_string();
            if !b.current {
                b.worktree = f[7].to_string();
            }
            if b.current {
                found.head = b.name.clone();
            }
            found.local.push(b);
        } else if let Some(rest) = f[0].strip_prefix("refs/remotes/") {
            let Some((remote, name)) = rest.split_once('/') else { continue };
            if name == "HEAD" {
                if !f[6].is_empty() {
                    defaults.push(f[6].to_string());
                }
                continue;
            }
            b.remote = remote.to_string();
            b.name = name.to_string();
            b.upstream.clear();
            found.remote.push(b);
        }
    }
    for b in &mut found.remote {
        b.default = defaults.iter().any(|d| *d == format!("refs/remotes/{}/{}", b.remote, b.name));
    }
    found
}

/// The repo's branches, with ahead/behind against each upstream as last fetched.
pub fn list_branches(dir: &Path) -> Result<Branches, String> {
    let stdout = git(dir, &["for-each-ref", REF_FORMAT, "refs/heads", "refs/remotes"])?.must("git for-each-ref")?;
    let mut found = parse_branches(&stdout);
    if found.head.is_empty() {
        found.detached = git(dir, &["rev-parse", "--short", "HEAD"])?.stdout.trim().to_string();
    }
    Ok(found)
}

fn notice(tone: Tone, text: impl Into<String>, detail: impl Into<String>) -> Notice {
    Notice::new(tone, text, detail)
}

/// git's complaint, for a refusal's detail.
fn said(ran: &Ran) -> String {
    tail(if ran.stderr.trim().is_empty() { &ran.stdout } else { &ran.stderr }, 12)
}

/// A name git accepts for a new branch, or why not.
fn valid_name(dir: &Path, name: &str) -> Result<(), Notice> {
    let refused = || notice(Tone::Error, format!("'{name}' is not a valid branch name."), "");
    if name.is_empty() || name.starts_with('-') || name.trim() != name {
        return Err(refused());
    }
    match git(dir, &["check-ref-format", "--branch", name]) {
        Ok(ran) if ran.ok() => Ok(()),
        _ => Err(refused()),
    }
}

fn local_exists(dir: &Path, name: &str) -> Result<bool, String> {
    Ok(git(dir, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{name}")])?.ok())
}

/// Switches the checkout to a branch. A remote one (`remote` set) switches to the local branch
/// tracking it, made when missing. Uncommitted changes come along, and git refuses when one is
/// in a file the switch changes: nothing is stashed behind the person's back.
pub fn switch_branch(dir: &Path, name: &str, remote: &str) -> Result<Notice, String> {
    let ran = if remote.is_empty() {
        git_with(dir, &["switch", "--end-of-options", name], SWITCH, None)?
    } else if local_exists(dir, name)? {
        let wanted = format!("{remote}/{name}");
        let upstream = git(dir, &["rev-parse", "--abbrev-ref", &format!("refs/heads/{name}@{{upstream}}")])?;
        if upstream.stdout.trim() != wanted {
            return Ok(notice(
                Tone::Error,
                format!("A local {name} already exists and does not track {wanted}; switch to it under Local."),
                "",
            ));
        }
        git_with(dir, &["switch", "--end-of-options", name], SWITCH, None)?
    } else {
        let start = format!("{remote}/{name}");
        git_with(dir, &["switch", "-c", name, "--track", "--end-of-options", &start], SWITCH, None)?
    };
    if !ran.ok() {
        return Ok(notice(Tone::Error, format!("Could not switch to {name}."), said(&ran)));
    }
    // git lists the uncommitted changes it carried over on stdout as "M\tpath", next to its
    // "Your branch is up to date" line; only the file lines mean changes came along.
    let carried = ran.stdout.lines().any(is_status_line);
    let text = if !carried {
        format!("Switched to {name}.")
    } else {
        format!("Switched to {name}; your uncommitted changes came along.")
    };
    Ok(notice(Tone::Success, text, tail(&ran.stdout, 12)))
}

/// One of git's short file statuses: one or two capital letters, a tab, then the path.
fn is_status_line(line: &str) -> bool {
    line.split_once('\t')
        .is_some_and(|(code, path)| !path.is_empty() && (1..=2).contains(&code.len()) && code.bytes().all(|c| c.is_ascii_uppercase()))
}

/// Makes a branch at `base` (a branch name, `origin/x` or `HEAD`), switching to it when asked.
/// It never tracks the base: a branch made from origin/main must not push to main.
pub fn create_branch(dir: &Path, name: &str, base: &str, switch: bool) -> Result<Notice, String> {
    if let Err(refused) = valid_name(dir, name) {
        return Ok(refused);
    }
    if local_exists(dir, name)? {
        return Ok(notice(Tone::Error, format!("A branch named {name} already exists."), ""));
    }
    let ran = if switch {
        git_with(dir, &["switch", "--no-track", "-c", name, "--end-of-options", base], SWITCH, None)?
    } else {
        git(dir, &["branch", "--no-track", "--end-of-options", name, base])?
    };
    if !ran.ok() {
        return Ok(notice(Tone::Error, format!("Could not create {name}."), said(&ran)));
    }
    let text = if switch { format!("Created {name} from {base} and switched to it.") } else { format!("Created {name} from {base}.") };
    Ok(notice(Tone::Success, text, ""))
}

/// Renames a local branch. Its upstream stays as it was.
pub fn rename_branch(dir: &Path, from: &str, to: &str) -> Result<Notice, String> {
    if let Err(refused) = valid_name(dir, to) {
        return Ok(refused);
    }
    let ran = git(dir, &["branch", "-m", "--end-of-options", from, to])?;
    if !ran.ok() {
        return Ok(notice(Tone::Error, format!("Could not rename {from}."), said(&ran)));
    }
    let upstream = git(dir, &["rev-parse", "--abbrev-ref", &format!("refs/heads/{to}@{{upstream}}")])?;
    let upstream = upstream.stdout.trim();
    let text = if upstream.is_empty() || !upstream.contains('/') {
        format!("Renamed {from} to {to}.")
    } else {
        format!("Renamed {from} to {to}. It still tracks {upstream}.")
    };
    Ok(notice(Tone::Success, text, ""))
}

/// Deletes a local branch. Without `force`, one with commits that are nowhere else answers with a
/// `Warning` notice: the page asks before deleting it with `force`.
pub fn delete_branch(dir: &Path, name: &str, force: bool) -> Result<Notice, String> {
    let sha = git(dir, &["rev-parse", "--short", &format!("refs/heads/{name}")])?.stdout.trim().to_string();
    let ran = git(dir, &["branch", if force { "-D" } else { "-d" }, "--end-of-options", name])?;
    if !ran.ok() {
        let why = said(&ran);
        if !force && why.to_lowercase().contains("not fully merged") {
            return Ok(notice(
                Tone::Warning,
                format!("{name} has commits that are on no other branch. Delete it anyway?"),
                format!("To get it back afterwards: git branch {name} {sha}"),
            ));
        }
        return Ok(notice(Tone::Error, format!("Could not delete {name}."), why));
    }
    Ok(notice(Tone::Success, format!("Deleted {name} (was {sha})."), format!("To get it back: git branch {name} {sha}")))
}

/// The local branches under a folder (`feat` holds `feat/x` and `feat/a/b`), sorted.
pub fn branches_in_folder(dir: &Path, folder: &str) -> Result<Vec<String>, String> {
    let pattern = format!("refs/heads/{}/", folder.trim_matches('/'));
    let stdout = git(dir, &["for-each-ref", "--format=%(refname:strip=2)", &pattern])?.must("git for-each-ref")?;
    Ok(stdout.lines().filter(|l| !l.is_empty()).map(str::to_string).collect())
}

fn current_branch(dir: &Path) -> Result<String, String> {
    Ok(git(dir, &["symbolic-ref", "-q", "--short", "HEAD"])?.stdout.trim().to_string())
}

/// Renames a folder of local branches: `from/x` becomes `to/x` for each. Every new name is checked
/// before the first rename, so a bad name or a taken one changes nothing. Upstreams stay as they were.
pub fn rename_folder(dir: &Path, from: &str, to: &str) -> Result<Notice, String> {
    let (from, to) = (from.trim_matches('/'), to.trim_matches('/'));
    let names = branches_in_folder(dir, from)?;
    if names.is_empty() {
        return Ok(notice(Tone::Error, format!("No branches under {from}/."), ""));
    }
    if to == from {
        return Ok(notice(Tone::Success, format!("{from} keeps its name."), ""));
    }
    if valid_name(dir, &format!("{to}/x")).is_err() {
        return Ok(notice(Tone::Error, format!("'{to}' is not a valid folder name."), ""));
    }
    if local_exists(dir, to)? {
        return Ok(notice(Tone::Error, format!("A branch named {to} exists, so there can be no {to}/ folder."), ""));
    }
    let moves: Vec<(String, String)> = names.iter().map(|n| (n.clone(), format!("{to}{}", &n[from.len()..]))).collect();
    let taken: Vec<&str> = moves.iter().filter(|(_, new)| local_exists(dir, new).unwrap_or(true)).map(|(_, new)| new.as_str()).collect();
    if !taken.is_empty() {
        return Ok(notice(Tone::Error, format!("{} of the new names are taken already.", taken.len()), taken.join("\n")));
    }
    for (done, (old, new)) in moves.iter().enumerate() {
        let ran = git(dir, &["branch", "-m", "--end-of-options", old, new])?;
        if !ran.ok() {
            let text = format!("Renamed {done} of {} branches, then git refused {old}.", moves.len());
            return Ok(notice(Tone::Error, text, said(&ran)));
        }
    }
    let text = format!("Renamed folder {from} to {to}: {}.", plural(moves.len(), "branch"));
    Ok(notice(Tone::Success, text, ""))
}

/// What deleting a folder of branches did.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderDelete {
    pub notice: Notice,
    /// Branches kept because their commits are on no other branch: the page asks before deleting
    /// them with `force`.
    pub unmerged: Vec<String>,
}

/// Deletes local branches, many to one git run. The checked-out branch is kept. Without `force`, a
/// branch whose commits are on no other branch is kept and named in `unmerged`.
pub fn delete_branches(dir: &Path, names: &[String], force: bool) -> Result<FolderDelete, String> {
    let head = current_branch(dir)?;
    let wanted: Vec<&str> = names.iter().map(String::as_str).filter(|n| *n != head).collect();
    let mut deleted = Vec::new();
    let mut unmerged = Vec::new();
    let mut refused = Vec::new();
    // Chunks keep the command line far below Windows' 32K limit.
    for chunk in wanted.chunks(150) {
        let mut args = vec!["branch", if force { "-D" } else { "-d" }, "--end-of-options"];
        args.extend_from_slice(chunk);
        let ran = git(dir, &args)?;
        // "Deleted branch feat/x (was 1a2b3c4)."
        for line in ran.stdout.lines() {
            if let Some((name, sha)) = line.strip_prefix("Deleted branch ").and_then(|rest| rest.rsplit_once(" (was ")) {
                deleted.push(format!("git branch {name} {}", sha.trim_end_matches(").")));
            }
        }
        for line in ran.stderr.lines().filter(|l| l.starts_with("error:")) {
            match quoted(line) {
                Some(name) if line.to_lowercase().contains("not fully merged") => unmerged.push(name.to_string()),
                _ => refused.push(line.trim_start_matches("error: ").to_string()),
            }
        }
    }
    let mut text = format!("Deleted {} of {}.", deleted.len(), plural(names.len(), "branch"));
    if deleted.len() == names.len() {
        text = format!("Deleted {}.", plural(deleted.len(), "branch"));
    }
    if names.len() > wanted.len() {
        text.push_str(&format!(" Kept {head}: it is checked out."));
    }
    if !unmerged.is_empty() {
        let them = if unmerged.len() == 1 { "it" } else { "them" };
        text.push_str(&format!(" {} commits that are on no other branch. Delete {them} anyway?", have(unmerged.len())));
    }
    let mut detail = Vec::new();
    if !unmerged.is_empty() {
        detail.push(format!("Not merged:\n{}", unmerged.join("\n")));
    }
    if !refused.is_empty() {
        detail.push(format!("git refused:\n{}", refused.join("\n")));
    }
    if !deleted.is_empty() {
        let them = if deleted.len() == 1 { "it" } else { "them" };
        detail.push(format!("To get {them} back:\n{}", deleted.join("\n")));
    }
    let tone = if !unmerged.is_empty() {
        Tone::Warning
    } else if !refused.is_empty() || deleted.is_empty() {
        Tone::Error
    } else {
        Tone::Success
    };
    Ok(FolderDelete { notice: notice(tone, text, detail.join("\n\n")), unmerged })
}

/// The text between the first pair of single quotes.
fn quoted(line: &str) -> Option<&str> {
    let (_, rest) = line.split_once('\'')?;
    Some(rest.split_once('\'')?.0)
}

fn plural(n: usize, word: &str) -> String {
    match (n, word.ends_with("ch")) {
        (1, _) => format!("1 {word}"),
        (_, true) => format!("{n} {word}es"),
        _ => format!("{n} {word}s"),
    }
}

/// "1 has", "5 have".
fn have(n: usize) -> String {
    if n == 1 { "1 has".to_string() } else { format!("{n} have") }
}

/// Deletes a branch on its remote. The remote's default branch is refused.
pub fn delete_remote_branch(dir: &Path, remote: &str, name: &str) -> Result<Notice, String> {
    let head = git(dir, &["symbolic-ref", "-q", &format!("refs/remotes/{remote}/HEAD")])?;
    if head.stdout.trim() == format!("refs/remotes/{remote}/{name}") {
        return Ok(notice(Tone::Error, format!("{name} is {remote}'s default branch; it is not deleted from here."), ""));
    }
    let sha = git(dir, &["rev-parse", "--short", &format!("refs/remotes/{remote}/{name}")])?.stdout.trim().to_string();
    let target = format!("refs/heads/{name}");
    let ran = git_with(dir, &["push", "--porcelain", remote, "--delete", &target], PUSH, None)?;
    if !ran.ok() {
        return Ok(notice(Tone::Error, format!("Could not delete {name} on {remote}."), said(&ran)));
    }
    Ok(notice(
        Tone::Success,
        format!("Deleted {name} on {remote} (was {sha})."),
        format!("To put it back: git push {remote} {sha}:refs/heads/{name}"),
    ))
}

/// Fetches every remote and drops remote-tracking branches deleted there. The push dialog's own
/// fetch reads only the upstream branch; the Branches column wants them all.
pub fn fetch_all(dir: &Path) -> Result<Notice, String> {
    let ran = git_with(dir, &["fetch", "--all", "--prune", "--quiet"], PUSH, None)?;
    if !ran.ok() {
        return Ok(notice(Tone::Error, "Fetching the remotes failed.", said(&ran)));
    }
    Ok(notice(Tone::Success, "Fetched every remote.", ""))
}
