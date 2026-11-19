//! Watches the shown repo's branches, so a commit made anywhere (a terminal, an editor, a Claude
//! session) updates the To push list at once, even while the window sits in the background.
//! Only local refs: origin's side changes by a fetch, which the window does on focus.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tauri::{AppHandle, Emitter, Manager};

use crate::{dialog, pipe};

/// The repo watched now and its watcher; dropping the watcher ends the watch and its settle task.
#[derive(Default)]
pub struct Watching(Mutex<Option<(String, RecommendedWatcher)>>);

/// A commit or a rebase writes several refs in a row: the list is read once they are quiet.
const SETTLE: Duration = Duration::from_millis(500);

/// Watches `name` instead of the repo watched so far; None stops watching.
#[tauri::command]
pub async fn watch_repo(app: AppHandle, name: Option<String>) -> Result<(), String> {
    let state = app.state::<Watching>();
    if state.0.lock().unwrap().as_ref().map(|(n, _)| n) == name.as_ref() {
        return Ok(());
    }
    *state.0.lock().unwrap() = None;
    let Some(name) = name else { return Ok(()) };
    let repo = dialog::repo_named(&app, &name)?;
    let (git_dir, common) = pipe::blocking(move || git_dirs(Path::new(&repo.path))).await?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok_and(|e| e.paths.iter().any(|p| moves_branch(p))) {
            let _ = tx.send(());
        }
    })
    .map_err(|e| e.to_string())?;
    // HEAD lives in the worktree's own git folder; packed-refs and refs/heads in the shared one.
    watcher.watch(&git_dir, RecursiveMode::NonRecursive).map_err(|e| e.to_string())?;
    if common != git_dir {
        watcher.watch(&common, RecursiveMode::NonRecursive).map_err(|e| e.to_string())?;
    }
    // Missing when every branch is packed; packed-refs then tells.
    let _ = watcher.watch(&common.join("refs").join("heads"), RecursiveMode::Recursive);

    let (app2, watched) = (app.clone(), name.clone());
    tauri::async_runtime::spawn(async move {
        while rx.recv().await.is_some() {
            loop {
                match tokio::time::timeout(SETTLE, rx.recv()).await {
                    Ok(Some(())) => continue,
                    Ok(None) => return,
                    Err(_) => break,
                }
            }
            let _ = app2.emit("repo-changed", &watched);
        }
    });
    *state.0.lock().unwrap() = Some((name, watcher));
    Ok(())
}

/// The repo's git folder and the folder its worktrees share, absolute.
fn git_dirs(dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    let ran = dropin_git::git(dir, &["rev-parse", "--path-format=absolute", "--git-dir", "--git-common-dir"])?.must("git rev-parse")?;
    let mut lines = ran.lines().map(|l| PathBuf::from(l.trim()));
    match (lines.next(), lines.next()) {
        (Some(git_dir), Some(common)) => Ok((git_dir, common)),
        _ => Err("git rev-parse gave no git folder".into()),
    }
}

/// Whether a changed path moves a branch: HEAD (a switch), packed-refs, or a branch's ref file.
/// The index, objects, FETCH_HEAD and git's `.lock` files are left out: they change all the time.
fn moves_branch(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if name.ends_with(".lock") {
        return false;
    }
    if name == "HEAD" || name == "packed-refs" {
        // Not logs/HEAD or a worktree's: their own folder is not watched recursively.
        return path.parent().and_then(Path::file_name).is_none_or(|p| p != "logs");
    }
    let parts: Vec<&str> = path.iter().filter_map(|c| c.to_str()).collect();
    parts.windows(2).any(|w| w == ["refs", "heads"])
}
