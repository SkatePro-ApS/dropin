//! Undoing a commit already on the upstream: a new commit that reverses it, made in a worktree
//! of its own and either pushed on its own (the fast way to take a bad deploy back) or put on top
//! of the branch to push later. The checkout's files and the commits waiting to push are never
//! touched by the push, and only move with the branch otherwise.

use std::path::Path;

use crate::ops::*;
use crate::parse::*;
use crate::run::*;

/// Reverts `sha`, a commit on the upstream. With `push`, the revert goes onto the upstream as it
/// is on the remote now (fetched first) and is pushed alone, so the branch's own commits stay
/// unpushed; a push refused because the upstream moved is tried once more. Without, it goes on
/// top of the branch, which then moves there the way a rebase does.
pub fn revert(dir: &Path, o: &Outgoing, sha: &str, push: bool, base: &Path) -> Result<Pushed, String> {
    let refuse = |text: String, detail: String| Ok(Pushed { notice: Notice::error(text, detail), refetch: false, sent: String::new() });
    if o.preview.is_some() || o.upstream.is_empty() {
        return refuse("A preview list cannot be reverted.".into(), String::new());
    }
    let subject = trimmed(dir, &["log", "-1", "--format=%s", sha], "git log")?;
    if push {
        for attempt in 0..2 {
            let fetched = git_with(dir, &["fetch", "-q", &o.remote, &o.remote_branch], FETCH, None)?;
            if !fetched.ok() {
                return refuse(format!("Fetching {} failed, so nothing was reverted.", o.upstream), tail(&fetched.stderr, 4));
            }
            let onto = trimmed(dir, &["rev-parse", &format!("{}^{{commit}}", o.upstream)], "git rev-parse")?;
            if !git(dir, &["merge-base", "--is-ancestor", sha, &onto])?.ok() {
                return refuse(format!("{} is not on {}, so there is nothing there to revert.", short(sha), o.upstream), String::new());
            }
            let made = match revert_in_worktree(dir, &onto, sha, &subject, base)? {
                Ok(made) => made,
                Err(notice) => return Ok(Pushed { notice, refetch: false, sent: String::new() }),
            };
            let target = format!("{made}:refs/heads/{}", o.remote_branch);
            let ran = git_with(dir, &["push", &o.remote, &target], PUSH, None)?;
            let said = tail(&format!("{}\n{}", ran.stderr, ran.stdout), 4);
            if ran.ok() {
                // A branch with nothing of its own follows the revert, so the checkout shows what is live.
                let followed = o.ahead == 0 && move_branch(dir, &o.branch, &onto, &made, "dropin: follow a pushed revert").is_ok();
                let next = if followed { String::new() } else { format!(" Rebase {} to bring it in here.", o.branch) };
                let text = format!("Reverted \u{201c}{subject}\u{201d} and pushed the revert to {}.{next}", o.upstream);
                return Ok(Pushed { notice: Notice::new(Tone::Success, text, said), refetch: true, sent: made });
            }
            let moved = ["[rejected]", "non-fast-forward", "fetch first"].iter().any(|s| ran.stderr.contains(s));
            if !(moved && attempt == 0) {
                let detail = if said.is_empty() { format!("exit {}", ran.code) } else { said };
                return Ok(Pushed { notice: Notice::error("The revert was made, but git push failed, so nothing went out.", detail), refetch: moved, sent: String::new() });
            }
        }
        unreachable!("the second attempt always returns");
    }

    let from = trimmed(dir, &["rev-parse", "HEAD"], "git rev-parse")?;
    if !git(dir, &["merge-base", "--is-ancestor", sha, &from])?.ok() {
        return refuse(format!("{} is not in {} yet: rebase onto {} first, then revert it.", short(sha), o.branch, o.upstream), String::new());
    }
    let made = match revert_in_worktree(dir, &from, sha, &subject, base)? {
        Ok(made) => made,
        Err(notice) => return Ok(Pushed { notice, refetch: false, sent: String::new() }),
    };
    let notice = match move_branch(dir, &o.branch, &from, &made, "dropin: revert a commit") {
        Ok(()) => Notice::new(Tone::Success, format!("Reverted \u{201c}{subject}\u{201d} on top of {}. Push it when you are ready.", o.branch), ""),
        Err(Unmoved::InTheWay(d)) => Notice::error("Your uncommitted changes are in files the revert changes, so nothing was changed. Commit them first.", d),
        Err(Unmoved::Moved(d)) => Notice::error(format!("{} moved while the revert was made, so nothing was changed.", o.branch), d),
        Err(Unmoved::Refused(d)) => Notice::error(format!("{} could not be moved, so nothing was changed.", o.branch), d),
    };
    Ok(Pushed { notice, refetch: false, sent: String::new() })
}

/// The revert's message in the log's own style: `revert: <subject>`, and reverting a revert
/// reads `reapply: <subject>`, as git does.
pub fn revert_message(subject: &str, sha: &str) -> String {
    let first = match subject.strip_prefix("revert: ") {
        Some(original) => format!("reapply: {original}"),
        None => format!("revert: {subject}"),
    };
    format!("{first}\n\nThis reverts commit {sha}.\n")
}

/// Makes the revert of `sha` on top of `at` in a worktree under `base`, which goes again after.
/// The new commit's sha, or why there is none: a clash with later changes, or nothing to undo.
fn revert_in_worktree(dir: &Path, at: &str, sha: &str, subject: &str, base: &Path) -> Result<Result<String, Notice>, String> {
    std::fs::create_dir_all(base).map_err(|e| format!("cannot make {}: {e}", base.display()))?;
    let worktree = slash(&base.join(format!("push-revert-{}", short(sha))));
    remove_worktree(dir, &worktree);
    let added = git_with(dir, &["worktree", "add", "--detach", &worktree, at], WORKTREE, None)?;
    if !added.ok() {
        return Ok(Err(Notice::error("A separate checkout for the revert could not be made, so nothing was changed.", tail(&added.stderr, 4))));
    }
    let wt = Path::new(&worktree);
    let made = (|| {
        // A merge is undone against its first parent: everything it brought in goes.
        let parents = out(dir, &["rev-list", "--parents", "-n", "1", sha], "git rev-list")?.split_whitespace().count() - 1;
        let mut args = vec!["revert", "--no-commit"];
        if parents > 1 {
            args.extend(["-m", "1"]);
        }
        args.push(sha);
        let ran = git_with(wt, &args, REBASE, None)?;
        if !ran.ok() {
            let files = git(wt, &["diff", "--name-only", "--diff-filter=U", "-z"])
                .ok()
                .filter(Ran::ok)
                .map(|r| split_z(&r.stdout))
                .unwrap_or_default();
            let _ = git(wt, &["revert", "--abort"]);
            if files.is_empty() {
                return Ok(Err(Notice::error("The revert failed, so nothing was changed.", tail(&format!("{}\n{}", ran.stderr, ran.stdout), 4))));
            }
            let text = format!(
                "Later commits changed the same lines in {}, so the revert cannot apply cleanly and nothing was changed. Revert those commits too, or undo it by hand.",
                plural(files.len(), "file")
            );
            return Ok(Err(Notice::error(text, files.join("\n"))));
        }
        if git(wt, &["diff", "--cached", "--quiet"])?.ok() {
            return Ok(Err(Notice::new(Tone::Warning, format!("\u{201c}{subject}\u{201d} is already undone there, so there was nothing to revert."), "")));
        }
        let message = revert_message(subject, sha);
        let committed = git_with(wt, &["commit", "-q", "-F", "-"], LONG, Some(message.as_str()))?;
        if !committed.ok() {
            return Ok(Err(Notice::error("Committing the revert failed, so nothing was changed.", tail(&format!("{}\n{}", committed.stderr, committed.stdout), 4))));
        }
        Ok(Ok(trimmed(wt, &["rev-parse", "HEAD"], "git rev-parse")?))
    })();
    remove_worktree(dir, &worktree);
    made
}
