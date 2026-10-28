//! The git steps of a push: read what would go out, push exactly that, rebase onto the
//! upstream in a worktree of its own, and adopt a rebase someone else finished there.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::parse::*;
use crate::run::*;
use crate::shelf::*;

/// The push dialog lists at most this many commits each way.
const LIST_LIMIT: usize = 200;
/// The upstream's newest commits shown as history.
const HISTORY: usize = 20;
/// A search reads this many of the upstream's newest commits.
const SEARCH_DEPTH: usize = 10_000;
const FETCH: Duration = Duration::from_secs(60);
const PUSH: Duration = Duration::from_secs(180);
const WORKTREE: Duration = Duration::from_secs(180);
const REBASE: Duration = Duration::from_secs(300);
const LONG: Duration = Duration::from_secs(60);
const READ_TREE: Duration = Duration::from_secs(120);

/// What a push would send, read at one pinned HEAD.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outgoing {
    pub branch: String,
    /// `origin/main`; in preview mode, the ref the list starts after.
    pub upstream: String,
    pub remote: String,
    pub remote_branch: String,
    /// The HEAD every list below was read at; a push sends this commit and no other.
    pub head: String,
    /// Commits the upstream lacks, newest first.
    pub commits: Vec<Commit>,
    /// Commits the branch lacks, newest first.
    pub incoming: Vec<Commit>,
    pub ahead: usize,
    pub behind: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetch_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_ref: Option<String>,
    pub history: Vec<Commit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Success,
    Warning,
    Error,
}

/// How a step ended, in words for the person; `detail` is git's own say.
#[derive(Clone, Debug, Serialize)]
pub struct Notice {
    pub tone: Tone,
    pub text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl Notice {
    pub(crate) fn new(tone: Tone, text: impl Into<String>, detail: impl Into<String>) -> Self {
        Notice { tone, text: text.into(), detail: detail.into() }
    }
    pub fn error(text: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(Tone::Error, text, detail)
    }
}

/// A rebase that stopped on a conflict: its worktree is kept, back at `from`, for someone to
/// finish; `adopt` then checks the result and moves the branch there.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    pub branch: String,
    pub upstream: String,
    /// The branch's commit when the rebase started; the branch moves only from here.
    pub from: String,
    /// The upstream's commit the rebase went onto.
    pub onto: String,
    pub worktree: String,
    pub files: Vec<String>,
}

/// How `rebase_onto` ended: a notice, or a conflict left in its worktree.
#[derive(Debug, Serialize)]
pub struct Rebased {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<Notice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict: Option<Conflict>,
    /// Refused for uncommitted work in these files: shelving them lets it go ahead.
    #[serde(rename = "inTheWay", skip_serializing_if = "Vec::is_empty")]
    pub in_the_way: Vec<String>,
}

/// How `adopt` ended; `to` is set once the branch moved.
#[derive(Debug, Serialize)]
pub struct Adopted {
    pub notice: Notice,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Subjects of the branch's commits the result lacks.
    pub dropped: Vec<String>,
    /// Refused for uncommitted work in these files: shelving them lets it go ahead.
    #[serde(rename = "inTheWay", skip_serializing_if = "Vec::is_empty")]
    pub in_the_way: Vec<String>,
}

/// How `push` ended; `refetch` when the upstream turned out to have moved.
#[derive(Debug, Serialize)]
pub struct Pushed {
    pub notice: Notice,
    pub refetch: bool,
    /// The commit pushed, once a push went through.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sent: String,
}

/// Why `move_branch` left the branch alone.
#[derive(Debug, PartialEq)]
pub enum Unmoved {
    /// The branch is no longer at `from`, or another branch is checked out.
    Moved(String),
    /// An uncommitted change is in a file the move changes.
    InTheWay(String),
    /// update-ref refused; the files were put back.
    Refused(String),
}

/// git's stdout, or an error naming the step.
fn out(dir: &Path, args: &[&str], what: &str) -> Result<String, String> {
    git(dir, args)?.must(what)
}

fn trimmed(dir: &Path, args: &[&str], what: &str) -> Result<String, String> {
    Ok(out(dir, args, what)?.trim().to_string())
}

fn log_of(dir: &Path, range: &str) -> Result<Vec<Commit>, String> {
    let max = format!("--max-count={LIST_LIMIT}");
    Ok(parse_log(&out(dir, &["log", LOG_FORMAT, LOG_DATE, &max, range], "git log")?))
}

/// The upstream's newest commits; a failure shows in the history and leaves the push alone.
fn read_history(dir: &Path, upstream: &str, o: &mut Outgoing) {
    let max = format!("--max-count={HISTORY}");
    o.history_ref = Some(upstream.to_string());
    match git(dir, &["log", LOG_FORMAT, LOG_DATE, &max, upstream, "--"]) {
        Ok(ran) if ran.ok() => o.history = parse_log(&ran.stdout),
        Ok(ran) => o.history_error = Some(format!("git log failed: {}", tail(&ran.stderr, 2))),
        Err(e) => o.history_error = Some(e),
    }
}

/// Commits on a ref that match a search, newest first.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    pub commits: Vec<Commit>,
    /// Matches in all; `commits` holds at most LIST_LIMIT of them.
    pub matched: usize,
    /// Commits read.
    pub searched: usize,
    /// False when the history goes on beyond the commits read.
    pub whole: bool,
}

/// True when every word of the search is in the commit's subject, body, author or email
/// (case aside), or starts its sha. `words` are lowercase.
pub fn commit_matches(c: &Commit, words: &[String]) -> bool {
    let text = format!("{}\n{}\n{}\n{}", c.subject, c.body, c.author, c.email).to_lowercase();
    words.iter().all(|w| text.contains(w.as_str()) || c.sha.starts_with(w.as_str()))
}

/// The commits on `upstream` (the history the dialog shows) that match `query`, searched in its
/// newest SEARCH_DEPTH commits. `since` and `until` bound the author time in milliseconds
/// (`since` included, `until` not), as `Commit::at` holds it.
pub fn search_history(dir: &Path, upstream: &str, query: &str, since: Option<i64>, until: Option<i64>) -> Result<Found, String> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    // One more than the depth tells whether the history goes on.
    let max = format!("--max-count={}", SEARCH_DEPTH + 1);
    let mut all = parse_log(&out(dir, &["log", LOG_FORMAT, LOG_DATE, &max, "--end-of-options", upstream, "--"], "git log")?);
    let whole = all.len() <= SEARCH_DEPTH;
    all.truncate(SEARCH_DEPTH);
    let searched = all.len();
    // Author time, as the list's Age shows it; git's --since/--until read the committer's.
    let in_range = |c: &Commit| since.is_none_or(|s| c.at >= s) && until.is_none_or(|u| c.at < u);
    let mut commits: Vec<Commit> = all.into_iter().filter(|c| in_range(c) && commit_matches(c, &words)).collect();
    let matched = commits.len();
    commits.truncate(LIST_LIMIT);
    Ok(Found { commits, matched, searched, whole })
}

/// What a push of the current branch would send, and what the upstream has that it lacks.
/// `fetch` refreshes the upstream first (a failure is reported, not fatal). `preview` lists
/// `preview..HEAD` instead, for a look at commits already pushed.
pub fn read_outgoing(dir: &Path, fetch: bool, preview: Option<&str>) -> Result<Outgoing, String> {
    let branch = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if !branch.ok() {
        // A new repo's branch has no commit for HEAD to name yet.
        if git(dir, &["rev-parse", "--git-dir"])?.ok() {
            return Err("This branch has no commits yet, so there is nothing to push.".into());
        }
        return Err(format!("This folder is not a git repository: {}", tail(&branch.stderr, 2)));
    }
    let branch = branch.stdout.trim().to_string();
    if branch == "HEAD" {
        return Err("HEAD is detached: check out a branch to push it.".into());
    }
    let tracked = git(dir, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])?;
    let upstream = if tracked.ok() { tracked.stdout.trim().to_string() } else { String::new() };
    let mut o = Outgoing {
        branch,
        upstream: upstream.clone(),
        remote: String::new(),
        remote_branch: String::new(),
        head: String::new(),
        commits: vec![],
        incoming: vec![],
        ahead: 0,
        behind: 0,
        fetch_error: None,
        preview: None,
        history_ref: None,
        history: vec![],
        history_error: None,
    };

    if let Some(preview) = preview {
        let base = git(dir, &["rev-parse", "--verify", "--quiet", &format!("{preview}^{{commit}}")])?;
        if !base.ok() {
            return Err(format!("git knows no commit called {preview}."));
        }
        o.head = trimmed(dir, &["rev-parse", "HEAD"], "git rev-parse")?;
        let range = format!("{preview}..{}", o.head);
        o.ahead = trimmed(dir, &["rev-list", "--count", &range], "git rev-list")?.parse().unwrap_or(0);
        if o.ahead > 0 {
            o.commits = log_of(dir, &range)?;
        }
        o.upstream = preview.to_string();
        o.preview = Some(preview.to_string());
        if !upstream.is_empty() {
            read_history(dir, &upstream, &mut o);
        }
        return Ok(o);
    }

    let Some((remote, remote_branch)) = upstream.split_once('/').filter(|(r, _)| !r.is_empty()) else {
        return Err(format!(
            "{} has no upstream branch to compare with. Push it once from a terminal: git push -u origin {}",
            o.branch, o.branch
        ));
    };
    o.remote = remote.to_string();
    o.remote_branch = remote_branch.to_string();

    if fetch {
        match git_with(dir, &["fetch", "--quiet", remote, remote_branch], FETCH, None) {
            Ok(ran) if !ran.ok() => {
                let said = tail(&ran.stderr, 3);
                o.fetch_error = Some(if said.is_empty() { format!("exit {}", ran.code) } else { said });
            }
            Ok(_) => {}
            Err(e) => o.fetch_error = Some(e),
        }
    }

    // Everything below reads at this one commit, so a commit another session makes
    // meanwhile cannot slip into the list without its own review.
    o.head = trimmed(dir, &["rev-parse", "HEAD"], "git rev-parse")?;
    let counts = trimmed(dir, &["rev-list", "--left-right", "--count", &format!("{upstream}...{}", o.head)], "git rev-list")?;
    let mut counts = counts.split_whitespace().map(|n| n.parse().unwrap_or(0));
    o.behind = counts.next().unwrap_or(0);
    o.ahead = counts.next().unwrap_or(0);
    if o.ahead > 0 {
        o.commits = log_of(dir, &format!("{upstream}..{}", o.head))?;
    }
    if o.behind > 0 {
        o.incoming = log_of(dir, &format!("{}..{upstream}", o.head))?;
    }
    read_history(dir, &upstream, &mut o);
    Ok(o)
}

/// Lowercase compare, as the tree sorts.
fn by_name(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

/// Files in the order the tree draws them: folders A–Z, top-level files last, then by name.
pub fn in_tree_order(mut files: Vec<FileChange>) -> Vec<FileChange> {
    let split = |p: &str| -> (String, String) {
        match p.rsplit_once('/') {
            Some((dir, name)) => (dir.to_string(), name.to_string()),
            None => (String::new(), p.to_string()),
        }
    };
    files.sort_by(|a, b| {
        let ((da, na), (db, nb)) = (split(&a.path), split(&b.path));
        (da.is_empty()).cmp(&db.is_empty()).then_with(|| by_name(&da, &db)).then_with(|| by_name(&na, &nb))
    });
    files
}

/// The files a commit changed (a merge: against its first parent), in tree order.
pub fn files_of(dir: &Path, sha: &str) -> Result<Vec<FileChange>, String> {
    let listed = out(dir, &["show", "--format=", "--name-status", "-z", "-M", "--diff-merges=first-parent", sha], "git show")?;
    Ok(in_tree_order(parse_name_status(&listed)))
}

/// One file's raw diff in a commit, and whether it was cut short; the UI shapes it.
pub fn diff_of(dir: &Path, sha: &str, file: &FileChange) -> Result<(String, bool), String> {
    let mut args = vec!["show", "--format=", "-M", "--diff-merges=first-parent", sha, "--"];
    if let Some(from) = &file.from {
        args.push(from);
    }
    args.push(&file.path);
    let ran = git(dir, &args)?;
    let truncated = ran.truncated;
    Ok((ran.must("git show")?, truncated))
}

/// Pushes the reviewed commits, leaving out `held` (shas from `o.commits`). Refuses when HEAD
/// moved since `o` was read: whatever came since has not been reviewed. Held commits that are
/// all newer than the rest stay as they are and the push stops below them; otherwise the branch
/// is first reordered in a worktree under `base`, the pushed commits first and the held ones on
/// top, and moved there. The working files never change: the reordered branch has the same tree.
pub fn push(dir: &Path, o: &Outgoing, held: &[String], base: &Path) -> Result<Pushed, String> {
    let refuse = |text: &str| Ok(Pushed { notice: Notice::error(text, ""), refetch: false, sent: String::new() });
    if o.preview.is_some() {
        return refuse("A preview list cannot be pushed.");
    }
    if o.ahead == 0 {
        return refuse("Nothing to push.");
    }
    if o.behind > 0 {
        return refuse(&format!("{} has commits this branch lacks: rebase onto it first.", o.upstream));
    }
    let head = trimmed(dir, &["rev-parse", "HEAD"], "git rev-parse")?;
    if head != o.head {
        return Ok(Pushed {
            notice: Notice::new(
                Tone::Warning,
                format!("{} moved since this list was read, so nothing was pushed. Check the new list, then push again.", o.branch),
                "",
            ),
            refetch: false,
            sent: String::new(),
        });
    }
    if held.is_empty() {
        return push_sha(dir, o, &o.head, o.ahead, "");
    }
    if o.commits.len() < o.ahead {
        return refuse(&format!("Only the newest {LIST_LIMIT} commits are listed, so commits cannot be held back here."));
    }
    if held.iter().any(|sha| !o.commits.iter().any(|c| &c.sha == sha)) {
        return refuse("A held commit is not in the list any more. Check the new list, then push again.");
    }
    // Oldest first, the way they are applied.
    let (send, keep): (Vec<&Commit>, Vec<&Commit>) = o.commits.iter().rev().partition(|c| !held.contains(&c.sha));
    if send.is_empty() {
        return refuse("Every commit is held back, so there is nothing to push.");
    }
    let held_note = format!(" {} held back.", plural(keep.len(), "commit"));
    let oldest_held = o.commits.iter().rev().position(|c| held.contains(&c.sha)).unwrap_or(o.commits.len());
    if oldest_held == send.len() {
        // The held commits are the newest ones: push the commit below them.
        return push_sha(dir, o, &send[send.len() - 1].sha, send.len(), &held_note);
    }

    let merges = trimmed(dir, &["rev-list", "--merges", "--count", &format!("{}..{}", o.upstream, o.head)], "git rev-list")?;
    if merges != "0" {
        return refuse("The commits to push hold a merge, so they cannot be reordered to hold some back. Push them all, or none.");
    }
    let onto = trimmed(dir, &["rev-parse", &format!("{}^{{commit}}", o.upstream)], "git rev-parse")?;
    std::fs::create_dir_all(base).map_err(|e| format!("cannot make {}: {e}", base.display()))?;
    let worktree = slash(&base.join(format!("push-reorder-{}", short(&o.head))));
    remove_worktree(dir, &worktree);
    let added = git_with(dir, &["worktree", "add", "--detach", &worktree, &onto], WORKTREE, None)?;
    if !added.ok() {
        return refuse("A separate checkout for reordering could not be made, so nothing was pushed.");
    }
    let wt = Path::new(&worktree);
    let reordered: Result<Result<(String, String), Notice>, String> = (|| {
        let pick = |list: &[&Commit]| -> Result<Option<Notice>, String> {
            let mut args = vec!["-c", "core.editor=true", "cherry-pick", "--allow-empty"];
            args.extend(list.iter().map(|c| c.sha.as_str()));
            let ran = git_with(wt, &args, REBASE, None)?;
            if ran.ok() {
                return Ok(None);
            }
            let files = git(wt, &["diff", "--name-only", "--diff-filter=U", "-z"])
                .ok()
                .filter(Ran::ok)
                .map(|r| split_z(&r.stdout))
                .unwrap_or_default();
            let _ = git(wt, &["cherry-pick", "--abort"]);
            let text = if files.is_empty() {
                "Reordering the commits failed, so nothing was pushed and the branch was not touched.".to_string()
            } else {
                format!(
                    "The commits to push need the held ones under them (they change the same lines in {}), so nothing was pushed and the branch was not touched.",
                    plural(files.len(), "file")
                )
            };
            let said = if files.is_empty() { tail(&format!("{}\n{}", ran.stderr, ran.stdout), 4) } else { files.join("\n") };
            Ok(Some(Notice::error(text, said)))
        };
        if let Some(n) = pick(&send)? {
            return Ok(Err(n));
        }
        let mid = trimmed(wt, &["rev-parse", "HEAD"], "git rev-parse")?;
        if let Some(n) = pick(&keep)? {
            return Ok(Err(n));
        }
        let to = trimmed(wt, &["rev-parse", "HEAD"], "git rev-parse")?;
        let tree = |sha: &str| trimmed(dir, &["rev-parse", &format!("{sha}^{{tree}}")], "git rev-parse");
        if tree(&to)? != tree(&o.head)? {
            return Ok(Err(Notice::error(
                "Reordered, the commits add up to different files, so nothing was pushed and the branch was not touched.",
                "",
            )));
        }
        Ok(Ok((mid, to)))
    })();
    remove_worktree(dir, &worktree);
    let (mid, to) = match reordered? {
        Ok(found) => found,
        Err(notice) => return Ok(Pushed { notice, refetch: false, sent: String::new() }),
    };
    if let Err(unmoved) = move_branch(dir, &o.branch, &o.head, &to, "dropin: hold commits back from a push") {
        return Ok(Pushed { notice: unmoved_notice(&o.branch, unmoved, "the commits were reordered"), refetch: false, sent: String::new() });
    }
    let note = format!("{held_note} {} now sits on top of the pushed ones, with new shas.", if keep.len() == 1 { "It" } else { "They" });
    push_sha(dir, o, &mid, send.len(), &note)
}

/// Pushes `sha`, `n` commits ahead of the upstream, to the upstream branch; `note` ends the notice.
fn push_sha(dir: &Path, o: &Outgoing, sha: &str, n: usize, note: &str) -> Result<Pushed, String> {
    let target = format!("{sha}:refs/heads/{}", o.remote_branch);
    let ran = git_with(dir, &["push", &o.remote, &target], PUSH, None)?;
    let said = tail(&format!("{}\n{}", ran.stderr, ran.stdout), 4);
    if ran.ok() {
        let text = format!("Pushed {} to {}.{note}", plural(n, "commit"), o.upstream);
        return Ok(Pushed { notice: Notice::new(Tone::Success, text, said), refetch: false, sent: sha.to_string() });
    }
    if ["[rejected]", "non-fast-forward", "fetch first"].iter().any(|s| ran.stderr.contains(s)) {
        let text = format!("{} has commits this branch lacks, so the push was refused. Rebase onto it, then push.{note}", o.upstream);
        return Ok(Pushed { notice: Notice::new(Tone::Warning, text, said), refetch: true, sent: String::new() });
    }
    let detail = if said.is_empty() { format!("exit {}", ran.code) } else { said };
    Ok(Pushed { notice: Notice::error(format!("git push failed.{note}"), detail), refetch: false, sent: String::new() })
}

/// The patch ids of a range, sorted; None when they cannot be read whole.
pub fn patch_ids(dir: &Path, range: &str) -> Option<Vec<String>> {
    let log = git_with(dir, &["log", "-p", "--reverse", range], LONG, None).ok()?;
    if !log.ok() || log.truncated {
        return None;
    }
    let ids = git_with(dir, &["patch-id", "--stable"], LONG, Some(&log.stdout)).ok()?;
    ids.ok().then(|| patch_ids_of(&ids.stdout))
}

/// Moves `branch` from `from` to `to` the way a checkout would. `read-tree -m -u` carries every
/// uncommitted change in files the move leaves alone, staged or not, and refuses when one is
/// in a file it changes; `update-ref` moves the branch only from `from`. A staged file stays
/// staged, where `rebase --autostash` unstages it.
pub fn move_branch(dir: &Path, branch: &str, from: &str, to: &str, why: &str) -> Result<(), Unmoved> {
    let read = |args: &[&str]| git(dir, args).map(|r| r.stdout.trim().to_string()).unwrap_or_default();
    let head = read(&["rev-parse", "HEAD"]);
    let current = read(&["rev-parse", "--abbrev-ref", "HEAD"]);
    if head != from || current != branch {
        return Err(Unmoved::Moved(format!("{branch} is at {}, not {}.", short(&head), short(from))));
    }
    match git_with(dir, &["read-tree", "-m", "-u", from, to], READ_TREE, None) {
        Ok(ran) if ran.ok() => {}
        Ok(ran) => return Err(Unmoved::InTheWay(tail(&ran.stderr, 4))),
        Err(e) => return Err(Unmoved::InTheWay(e)),
    }
    let set = git(dir, &["update-ref", "-m", why, "HEAD", to, from]);
    if let Ok(ran) = &set {
        if ran.ok() {
            return Ok(());
        }
    }
    let _ = git_with(dir, &["read-tree", "-m", "-u", to, from], READ_TREE, None);
    Err(Unmoved::Refused(match set {
        Ok(ran) => tail(&ran.stderr, 4),
        Err(e) => e,
    }))
}

/// Moves the branch as `move_branch` does. With `shelve`, the uncommitted work in files the move
/// changes is shelved first and put back after, moved or not; `Some` says how it went back.
fn move_shelving(dir: &Path, branch: &str, from: &str, to: &str, why: &str, shelve: bool) -> Result<Option<Unshelved>, Unmoved> {
    let files = if shelve { in_the_way(dir, from, to).map_err(Unmoved::InTheWay)? } else { vec![] };
    if files.is_empty() {
        return move_branch(dir, branch, from, to, why).map(|()| None);
    }
    let sha = shelve_files(dir, &files, why).map_err(Unmoved::InTheWay)?;
    let moved = move_branch(dir, branch, from, to, why);
    let back = unshelve(dir, &sha);
    moved.map(|()| Some(back))
}

fn shelve_files(dir: &Path, files: &[String], why: &str) -> Result<String, String> {
    let why = format!("for {}", why.trim_start_matches("dropin: "));
    shelve(dir, files, &why).map_err(|e| format!("Shelving the changes in the way failed: {e}"))
}

/// A notice for a moved branch, told how its shelved work went back.
fn with_shelf(notice: Notice, back: Option<Unshelved>) -> Notice {
    match back {
        None => notice,
        Some(Unshelved::Back) => Notice { text: format!("{} Your shelved changes are back.", notice.text), ..notice },
        Some(Unshelved::Clashed { sha, files }) => Notice::new(
            Tone::Warning,
            format!(
                "{} Your shelved changes clash with the incoming ones in {}: resolve the conflict markers there. \
                 The shelf stays in git's stash ({}) until you drop it.",
                notice.text,
                plural(files.len(), "file"),
                short(&sha)
            ),
            files.join("\n"),
        ),
        Some(Unshelved::Kept { sha, detail }) => Notice::new(
            Tone::Warning,
            format!(
                "{} Your shelved changes could not go back, so they wait in git's stash ({}): unshelve them once the files are free.",
                notice.text,
                short(&sha)
            ),
            detail,
        ),
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

/// Where rebase worktrees go: the repo's own `.claude/tmp` when it has one (gitignored, and
/// inside Claude's working folder), else `~/.cache/dropin/worktrees`. Never under AppData:
/// Claude Desktop's MSIX package gives the processes it starts a private copy of it.
pub fn worktree_base(dir: &Path) -> PathBuf {
    let scratch = dir.join(".claude").join("tmp");
    if scratch.is_dir() {
        return scratch;
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default();
    PathBuf::from(home).join(".cache").join("dropin").join("worktrees")
}

/// Takes a worktree away, and git's record of it; one that will not go is left for the next prune.
pub fn remove_worktree(dir: &Path, worktree: &str) {
    let _ = git_with(dir, &["worktree", "remove", "--force", worktree], LONG, None);
    let _ = git(dir, &["worktree", "prune"]);
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Rebases the branch onto its upstream in a worktree of its own under `base`, then moves the
/// branch there (move_branch). The checkout is never stashed or rewritten mid-rebase, though
/// other sessions edit it too, and staged work stays staged. Stops before starting when an
/// incoming file has uncommitted changes; keeps the worktree for a conflict; compares patch
/// ids before and after to show the commits survived. A branch with nothing of its own just
/// moves to the upstream, with no worktree. With `shelve`, uncommitted work in the incoming
/// files is shelved for the moment the branch moves, then put back.
pub fn rebase_onto(dir: &Path, o: &Outgoing, base: &Path, shelve: bool) -> Result<Rebased, String> {
    let notice = |n: Notice| Ok(Rebased { notice: Some(n), conflict: None, in_the_way: vec![] });
    if o.preview.is_some() || o.upstream.is_empty() {
        return notice(Notice::error("A preview list cannot be rebased.", ""));
    }
    let up = &o.upstream;
    let merge_base = trimmed(dir, &["merge-base", "HEAD", up], "git merge-base")?;
    // A file the upstream changed but HEAD already holds as the upstream has it (the same commit
    // landed on both sides) is left alone by the rebase, so uncommitted work in it is no reason to stop.
    let differs = split_z(&out(dir, &["diff", "--name-only", "-z", "HEAD", up], "git diff")?);
    let incoming = overlap_of(&split_z(&out(dir, &["diff", "--name-only", "-z", &merge_base, up], "git diff")?), &differs);
    let overlap = overlap_of(&incoming, &changed_files(dir)?);
    if !overlap.is_empty() && !shelve {
        let n = overlap.len();
        let text = format!(
            "Nothing was changed: {} also {} uncommitted changes here. Commit them, or shelve them for the rebase.",
            plural(n, "incoming file"),
            if n == 1 { "has" } else { "have" }
        );
        return Ok(Rebased { notice: Some(Notice::error(text, overlap.join("\n"))), conflict: None, in_the_way: overlap });
    }

    let from = trimmed(dir, &["rev-parse", "HEAD"], "git rev-parse")?;
    let onto = trimmed(dir, &["rev-parse", &format!("{up}^{{commit}}")], "git rev-parse")?;
    let why = format!("dropin: rebase onto {up}");
    let ahead = trimmed(dir, &["rev-list", "--count", &format!("{onto}..{from}")], "git rev-list")?;
    if ahead == "0" {
        return notice(match move_shelving(dir, &o.branch, &from, &onto, &why, shelve) {
            Ok(back) => with_shelf(Notice::new(Tone::Success, format!("{} now matches {up}.", o.branch), ""), back),
            Err(unmoved) => unmoved_notice(&o.branch, unmoved, "the rebase ran"),
        });
    }

    std::fs::create_dir_all(base).map_err(|e| format!("cannot make {}: {e}", base.display()))?;
    let worktree = slash(&base.join(format!("push-rebase-{}", short(&from))));
    remove_worktree(dir, &worktree);
    let added = git_with(dir, &["worktree", "add", "--detach", &worktree, &from], WORKTREE, None)?;
    if !added.ok() {
        return notice(Notice::error(
            "A separate checkout for the rebase could not be made, so nothing was changed.",
            tail(&added.stderr, 4),
        ));
    }
    let wt = Path::new(&worktree);
    let mut kept = false;
    let result = (|| {
        let before = patch_ids(dir, &format!("{onto}..{from}"));
        let ran = git_with(wt, &["rebase", &onto], REBASE, None)?;
        let said = tail(&format!("{}\n{}", ran.stdout, ran.stderr), 4);
        if !ran.ok() {
            // While the rebase is stopped, git lists the files it could not merge.
            let files = git(wt, &["diff", "--name-only", "--diff-filter=U", "-z"])
                .ok()
                .filter(Ran::ok)
                .map(|r| split_z(&r.stdout))
                .unwrap_or_default();
            let undone = git_with(wt, &["rebase", "--abort"], REBASE, None).map(|r| r.ok()).unwrap_or(false);
            if undone && !files.is_empty() {
                kept = true;
                let conflict = Conflict {
                    branch: o.branch.clone(),
                    upstream: up.clone(),
                    from: from.clone(),
                    onto: onto.clone(),
                    worktree: worktree.clone(),
                    files,
                };
                return Ok(Rebased { notice: None, conflict: Some(conflict), in_the_way: vec![] });
            }
            return notice(Notice::error("The rebase failed. Your checkout was not touched.", said));
        }

        let to = trimmed(wt, &["rev-parse", "HEAD"], "git rev-parse")?;
        let after = patch_ids(dir, &format!("{onto}..{to}"));
        let back = match move_shelving(dir, &o.branch, &from, &to, &why, shelve) {
            Ok(back) => back,
            Err(unmoved) => return notice(unmoved_notice(&o.branch, unmoved, "the rebase ran")),
        };
        let rebased = match (before, after) {
            (Some(before), Some(after)) if before != after => Notice::new(
                Tone::Warning,
                format!(
                    "Rebased onto {up}, but the patches changed ({} before, {} after): look at the commits again before pushing.",
                    before.len(),
                    after.len()
                ),
                said,
            ),
            (Some(_), Some(after)) => Notice::new(
                Tone::Success,
                format!("Rebased {} onto {up}; every patch is unchanged.", plural(after.len(), "commit")),
                said,
            ),
            _ => Notice::new(Tone::Success, format!("Rebased onto {up}. The patches were too large to compare."), said),
        };
        notice(with_shelf(rebased, back))
    })();
    if !kept {
        remove_worktree(dir, &worktree);
    }
    result
}

/// A move_branch refusal as a notice; `during` names what the branch moved during.
fn unmoved_notice(branch: &str, unmoved: Unmoved, during: &str) -> Notice {
    match unmoved {
        Unmoved::Moved(d) => Notice::error(format!("{branch} moved while {during}, so nothing was changed."), d),
        Unmoved::InTheWay(d) => Notice::error("Your uncommitted changes are in files the rebase changes, so nothing was changed.", d),
        Unmoved::Refused(d) => Notice::error(format!("{branch} could not be moved, so nothing was changed."), d),
    }
}

/// Checks a rebase someone finished in a conflict's worktree (done, nothing uncommitted, on top
/// of the upstream, no conflict markers) and moves the branch there. The worktree is left for
/// the caller: kept on a refusal, so the work can be fixed and adopted again. With `shelve`,
/// uncommitted work in the files the move changes is shelved for the move, then put back.
pub fn adopt(dir: &Path, c: &Conflict, shelve: bool) -> Result<Adopted, String> {
    let failed = |text: String, detail: String| Ok(Adopted { notice: Notice::error(text, detail), to: None, dropped: vec![], in_the_way: vec![] });
    let wt = Path::new(&c.worktree);
    if !wt.is_dir() {
        return failed(format!("{} is gone, so there is nothing to adopt.", c.worktree), String::new());
    }
    for state in ["rebase-merge", "rebase-apply"] {
        let path = git(wt, &["rev-parse", "--path-format=absolute", "--git-path", state])?.stdout.trim().to_string();
        if !path.is_empty() && Path::new(&path).exists() {
            return failed(
                "The rebase in the worktree is still in progress: finish it (git rebase --continue), then adopt.".into(),
                String::new(),
            );
        }
    }
    let dirty = git(wt, &["status", "--porcelain", "--untracked-files=no"])?.stdout.trim().to_string();
    if !dirty.is_empty() {
        return failed("The worktree has uncommitted changes, so its result was not used.".into(), dirty);
    }
    let to = trimmed(wt, &["rev-parse", "HEAD"], "git rev-parse")?;
    if to == c.from || !git(dir, &["merge-base", "--is-ancestor", &c.onto, &to])?.ok() {
        return failed(format!("The worktree's HEAD is not on top of {}, so it was not used.", c.upstream), String::new());
    }

    let touched = split_z(&out(dir, &["diff", "--name-only", "-z", &c.onto, &to], "git diff")?);
    if !touched.is_empty() {
        let mut args = vec!["grep", "-l", "-E", "^(<<<<<<<|>>>>>>>)( |$)", to.as_str(), "--"];
        args.extend(touched.iter().map(String::as_str));
        let marked = git_with(dir, &args, LONG, None)?;
        if marked.ok() {
            return failed("Conflict markers are left in the result, so it was not used.".into(), marked.stdout.trim().to_string());
        }
    }
    let subjects = |range: String| git(dir, &["log", "--reverse", "--format=%s", &range]).map(|r| lines_of(&r.stdout));
    let mine = subjects(format!("{}..{}", c.onto, c.from))?;
    let theirs = subjects(format!("{}..{to}", c.onto))?;
    let dropped = missing_from(&mine, &theirs);

    if !shelve {
        let in_the_way = in_the_way(dir, &c.from, &to)?;
        if !in_the_way.is_empty() {
            let text = format!(
                "Nothing was changed: {} the rebase changes {} uncommitted changes here. Commit them, or shelve them to adopt.",
                plural(in_the_way.len(), "file"),
                if in_the_way.len() == 1 { "has" } else { "have" }
            );
            let notice = Notice::error(text, in_the_way.join("\n"));
            return Ok(Adopted { notice, to: None, dropped: vec![], in_the_way });
        }
    }
    let back = match move_shelving(dir, &c.branch, &c.from, &to, "dropin: rebase resolved in a worktree", shelve) {
        Ok(back) => back,
        Err(unmoved) => {
            let notice = unmoved_notice(&c.branch, unmoved, "the conflict was resolved");
            return Ok(Adopted { notice, to: None, dropped: vec![], in_the_way: vec![] });
        }
    };
    let tone = if dropped.is_empty() { Tone::Success } else { Tone::Warning };
    let text = format!(
        "{} now has {} on top of {}{}. Review them before you push.",
        c.branch,
        plural(theirs.len(), "commit"),
        c.upstream,
        if dropped.is_empty() { String::new() } else { format!("; {} dropped", plural(dropped.len(), "commit")) }
    );
    let notice = with_shelf(Notice::new(tone, text, dropped.join("\n")), back);
    Ok(Adopted { notice, to: Some(to), dropped, in_the_way: vec![] })
}
