//! When commits reached GitHub: git keeps no push time, so it comes from GitHub's Activity API,
//! read through the `gh` CLI (its login, its proxy). Only the app calls this, never the CLI.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::run::git;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const GH_TIMEOUT: Duration = Duration::from_secs(15);
const NO_SHA: &str = "0000000000000000000000000000000000000000";

/// `owner/repo` from a GitHub remote URL (https, scp-style or ssh); None for any other host.
pub fn github_repo(url: &str) -> Option<String> {
    // After the host: `git@github.com:o/r`, `git@github.com:/o/r`, `https://user@github.com/o/r`.
    let (_, rest) = url.trim().split_once("github.com")?;
    let rest = rest.strip_prefix(':').or_else(|| rest.strip_prefix('/'))?;
    let path = rest.trim_matches('/').trim_end_matches(".git");
    let (owner, repo) = path.split_once('/')?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains('/')).then(|| format!("{owner}/{repo}"))
}

/// One push from `gh api …/activity --jq '[.before,.after,(.timestamp|fromdateiso8601)]|@tsv'`.
pub fn parse_pushes(stdout: &str) -> Vec<(String, String, i64)> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let (before, after, secs) = (f.next()?, f.next()?, f.next()?.trim().parse::<i64>().ok()?);
            Some((before.to_string(), after.to_string(), secs * 1000))
        })
        .collect()
}

/// When each of `shas` was pushed to `upstream` (`origin/main`), in milliseconds since the epoch.
/// A commit gets the time of the push that brought it; one GitHub no longer lists is left out.
pub fn push_times(dir: &Path, upstream: &str, shas: &[String]) -> Result<HashMap<String, i64>, String> {
    let (remote, branch) = upstream.split_once('/').ok_or_else(|| format!("{upstream} is no remote branch"))?;
    let url = git(dir, &["remote", "get-url", remote])?.must("git remote get-url")?;
    let repo = github_repo(&url).ok_or("The remote is not on GitHub")?;
    // Every type: a PR merged on GitHub is `pr_merge`, not `push`; a deletion's zero `after` is skipped.
    let path = format!("repos/{repo}/activity?ref=refs/heads/{branch}&per_page=100");
    let pushes = parse_pushes(&gh(&["api", &path, "--jq", ".[] | [.before, .after, (.timestamp | fromdateiso8601)] | @tsv"])?);
    Ok(times_of(dir, pushes, shas))
}

/// Gives each of `shas` the time of the push (newest first, as GitHub lists them) that brought it.
pub fn times_of(dir: &Path, pushes: Vec<(String, String, i64)>, shas: &[String]) -> HashMap<String, i64> {
    let mut left: Vec<&String> = shas.iter().collect();
    let mut times = HashMap::new();
    // Newest push first; each brought `before..after`.
    // shortcut: a force push that re-sends old commits gives them its later time; fine on a protected main.
    for (before, after, at) in pushes {
        if left.is_empty() {
            break;
        }
        let not = format!("^{before}");
        let mut args = vec!["rev-list", "--max-count=1000", after.as_str()];
        if before != NO_SHA {
            args.push(&not);
        }
        // A sha not fetched here, or force-pushed away, fails: that push is skipped.
        let Ok(ran) = git(dir, &args) else { continue };
        if !ran.ok() {
            continue;
        }
        let brought: std::collections::HashSet<&str> = ran.stdout.lines().collect();
        left.retain(|sha| {
            let hit = brought.contains(sha.as_str());
            if hit {
                times.insert((*sha).clone(), at);
            }
            !hit
        });
    }
    times
}

/// Runs `gh` with no prompt and no console window; its stdout, or its complaint.
fn gh(args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("gh");
    cmd.args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let mut child = cmd.spawn().map_err(|e| format!("gh could not start (is the GitHub CLI installed?): {e}"))?;
    // Pipes drain on their own threads, so gh never blocks on a full one.
    let drain = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = p {
                let _ = p.read_to_string(&mut s);
            }
            s
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let Some(status) = child.wait_timeout(GH_TIMEOUT).map_err(|e| e.to_string())? else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("gh api ran over {} s and was stopped", GH_TIMEOUT.as_secs()));
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if status.success() {
        Ok(out)
    } else {
        Err(format!("gh api failed: {}", err.trim()))
    }
}
