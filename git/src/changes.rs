//! The work tree's uncommitted changes, as PhpStorm's commit view shows them: list them, diff one
//! against HEAD, commit some, roll some back, and read a file's image for a preview.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::ops::in_tree_order;
use crate::parse::*;
use crate::run::*;

/// git's tree with nothing in it: what a repo without a commit compares with.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// A commit runs the repo's hooks, which can take a while.
const COMMIT: Duration = Duration::from_secs(180);
/// The biggest image a preview reads.
pub const IMAGE_CAP: usize = 25 * 1024 * 1024;

/// The uncommitted changes and what the commit box needs to know about HEAD.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    /// Tracked changes first, then untracked files, each in tree order.
    pub files: Vec<FileChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<Head>,
    /// A merge is in progress: git refuses a commit of some files only.
    pub merging: bool,
}

/// HEAD, for an amend: its message to start from, and whether a remote has it already.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Head {
    pub sha: String,
    pub short: String,
    pub message: String,
    /// A remote-tracking branch holds HEAD: amending it would rewrite pushed history.
    pub pushed: bool,
}

/// The commit `commit` made.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Committed {
    pub sha: String,
    pub short: String,
    pub subject: String,
    /// git's own report (`[main 1a2b3c4] Subject`, the hooks' say).
    pub said: String,
}

/// The repo's uncommitted changes: tracked ones against HEAD, then untracked files (.gitignore
/// respected).
pub fn read_changes(dir: &Path) -> Result<Changes, String> {
    let text = git(dir, &["status", "--porcelain=v2", "-z", "--untracked-files=all"])?.must("git status")?;
    let (untracked, tracked): (Vec<_>, Vec<_>) = parse_status_v2(&text).into_iter().partition(|f| f.status == "?");
    let mut files = in_tree_order(tracked);
    files.extend(in_tree_order(untracked));
    let head = read_head(dir)?;
    let merging = git(dir, &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"])?.ok();
    Ok(Changes { files, head, merging })
}

fn read_head(dir: &Path) -> Result<Option<Head>, String> {
    let at = git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"])?;
    if !at.ok() {
        return Ok(None);
    }
    let sha = at.stdout.trim().to_string();
    let shown = git(dir, &["log", "-1", "--format=%h%x1f%B", &sha])?.must("git log")?;
    let (short, message) = shown.split_once('\x1f').unwrap_or((&sha[..7], ""));
    let holders = git(dir, &["for-each-ref", "--count=1", "--contains", &sha, "--format=%(refname)", "refs/remotes"])?
        .must("git for-each-ref")?;
    Ok(Some(Head {
        short: short.to_string(),
        message: message.trim().to_string(),
        pushed: !holders.trim().is_empty(),
        sha,
    }))
}

/// One file's raw diff against HEAD, work tree and index taken together, and whether it was cut
/// short. An untracked file shows as all new.
pub fn working_diff(dir: &Path, file: &FileChange) -> Result<(String, bool), String> {
    let ran = if file.status == "?" {
        // --no-index exits 1 when the files differ, which they always do here.
        let ran = git(dir, &["diff", "--no-index", "--", "/dev/null", &file.path])?;
        if ran.code > 1 {
            return Err(ran.must("git diff").unwrap_err());
        }
        ran
    } else {
        let base = if git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"])?.ok() { "HEAD" } else { EMPTY_TREE };
        let mut args = vec!["diff", "-M", base, "--"];
        if let Some(from) = &file.from {
            args.push(from);
        }
        args.push(&file.path);
        let ran = git(dir, &args)?;
        if !ran.ok() {
            return Err(ran.must("git diff").unwrap_err());
        }
        ran
    };
    Ok((ran.stdout, ran.truncated))
}

/// NUL-separated paths, as `--pathspec-from-file=- --pathspec-file-nul` reads them.
fn nul_list(paths: &[&str]) -> String {
    paths.iter().map(|p| format!("{p}\0")).collect()
}

/// Runs a git step on a list of paths, taken literally (a `[` in a name is a `[`).
fn with_paths(dir: &Path, step: &[&str], paths: &[&str], timeout: Duration, what: &str) -> Result<String, String> {
    let mut args = vec!["--literal-pathspecs"];
    args.extend_from_slice(step);
    args.extend_from_slice(&["--pathspec-from-file=-", "--pathspec-file-nul"]);
    git_with(dir, &args, timeout, Some(&nul_list(paths)))?.must(what)
}

/// Commits exactly `files`, each in its work-tree version, with `message`. Whatever is staged for
/// other paths stays staged and out of the commit. Untracked files are added first, and taken
/// out again when the commit fails. `amend` rewrites HEAD, never when a remote has it already;
/// with no files it changes only the message.
pub fn commit_changes(dir: &Path, message: &str, files: &[FileChange], amend: bool) -> Result<Committed, String> {
    if message.trim().is_empty() {
        return Err("Write a commit message first.".into());
    }
    let now = read_changes(dir)?;
    if now.merging {
        return Err("A merge is in progress, and git commits a merge only whole. Finish it from a terminal.".into());
    }
    if amend {
        match &now.head {
            None => return Err("There is no commit to amend yet.".into()),
            Some(head) if head.pushed => {
                return Err(format!("{} is pushed already; amending it would rewrite history others may have.", head.short));
            }
            _ => {}
        }
    } else if files.is_empty() {
        return Err("Tick the files to commit.".into());
    }
    // Only what is still changed, as it is now: a file committed or rolled back since the list was
    // drawn is skipped, so git does not refuse the whole commit over it.
    let picked: Vec<&FileChange> = now.files.iter().filter(|n| files.iter().any(|f| f.path == n.path)).collect();
    if picked.is_empty() && !amend {
        return Err("None of the ticked files has changes any more. Check the list again.".into());
    }
    let added: Vec<&str> = picked.iter().filter(|f| f.status == "?").map(|f| f.path.as_str()).collect();
    let mut paths: Vec<&str> = Vec::new();
    for f in &picked {
        if let Some(from) = &f.from {
            paths.push(from);
        }
        paths.push(&f.path);
    }

    if !added.is_empty() {
        with_paths(dir, &["add"], &added, DEFAULT_TIMEOUT, "git add")?;
    }
    let note = message_file(message)?;
    let note_arg = note.to_string_lossy().into_owned();
    let mut step = vec!["commit", "-F", &note_arg, "--cleanup=strip"];
    if amend {
        step.push("--amend");
    }
    // --only with no paths is valid only with --amend: a message-only amend.
    step.push("--only");
    let ran = if paths.is_empty() {
        git_with(dir, &step, COMMIT, None)
    } else {
        let mut args = vec!["--literal-pathspecs"];
        args.extend_from_slice(&step);
        args.extend_from_slice(&["--pathspec-from-file=-", "--pathspec-file-nul"]);
        git_with(dir, &args, COMMIT, Some(&nul_list(&paths)))
    };
    let _ = std::fs::remove_file(&note);
    let ran = match ran {
        Ok(ran) if ran.ok() => ran,
        failed => {
            if !added.is_empty() {
                let _ = with_paths(dir, &["rm", "--cached", "--quiet"], &added, DEFAULT_TIMEOUT, "git rm");
            }
            let said = match failed {
                Ok(ran) => tail(&format!("{}\n{}", ran.stderr, ran.stdout), 6),
                Err(e) => e,
            };
            return Err(format!("git commit failed: {said}"));
        }
    };
    let line = git(dir, &["log", "-1", "--format=%H%x1f%h%x1f%s"])?.must("git log")?;
    let mut field = line.trim().split('\x1f');
    Ok(Committed {
        sha: field.next().unwrap_or_default().to_string(),
        short: field.next().unwrap_or_default().to_string(),
        subject: field.next().unwrap_or_default().to_string(),
        said: tail(&format!("{}\n{}", ran.stdout, ran.stderr), 6),
    })
}

/// The message in a file of its own, so any text (a leading `-`, many lines) reaches git as written.
fn message_file(message: &str) -> Result<PathBuf, String> {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let path = std::env::temp_dir().join(format!("dropin-commit-{}-{stamp}.txt", std::process::id()));
    std::fs::write(&path, message).map_err(|e| format!("could not write the message to {}: {e}", path.display()))?;
    Ok(path)
}

/// Undoes `files`' changes back to HEAD. A file added to git is only taken out of the index (it
/// stays on disk, untracked), as is a rename's new side; nothing tracked is deleted. An untracked
/// file is deleted from disk: the page asks first.
pub fn rollback(dir: &Path, files: &[FileChange]) -> Result<(), String> {
    let (mut restore, mut unadd, mut delete) = (Vec::new(), Vec::new(), Vec::new());
    for f in files {
        match f.status.as_str() {
            "?" => delete.push(f.path.as_str()),
            "A" | "C" => unadd.push(f.path.as_str()),
            "R" => {
                unadd.push(f.path.as_str());
                if let Some(from) = &f.from {
                    restore.push(from.as_str());
                }
            }
            _ => restore.push(f.path.as_str()),
        }
    }
    if !unadd.is_empty() {
        with_paths(dir, &["rm", "--cached", "--quiet"], &unadd, DEFAULT_TIMEOUT, "git rm --cached")?;
    }
    if !restore.is_empty() {
        with_paths(dir, &["restore", "--source=HEAD", "--staged", "--worktree"], &restore, DEFAULT_TIMEOUT, "git restore")?;
    }
    for path in delete {
        let full = inside(dir, path)?;
        std::fs::remove_file(&full).map_err(|e| format!("could not delete {path}: {e}"))?;
    }
    Ok(())
}

/// `path` under the work tree, refused when it would reach outside it.
fn inside(dir: &Path, path: &str) -> Result<PathBuf, String> {
    let rel = Path::new(path);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("{path} is not a path inside the repo"));
    }
    Ok(dir.join(rel))
}

/// A file's bytes for an image preview: at `rev` (`HEAD`, a commit, `abc^`), or from the work tree
/// when `rev` is empty.
pub fn image_of(dir: &Path, rev: &str, path: &str) -> Result<Vec<u8>, String> {
    if rev.is_empty() {
        let full = inside(dir, path)?;
        let size = std::fs::metadata(&full).map_err(|e| format!("could not read {path}: {e}"))?.len();
        if size as usize > IMAGE_CAP {
            return Err(format!("{path} is over {} MB", IMAGE_CAP / (1024 * 1024)));
        }
        return std::fs::read(&full).map_err(|e| format!("could not read {path}: {e}"));
    }
    if rev.starts_with('-') {
        return Err(format!("{rev} is not a revision"));
    }
    git_bytes(dir, &["cat-file", "blob", &format!("{rev}:{path}")], IMAGE_CAP, "git cat-file")
}
