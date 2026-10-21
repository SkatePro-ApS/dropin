//! The push dialog's side of the app: the commands its page calls, the outgoing list each repo's
//! page shows (a push sends exactly that), and the `dropin push-review` calls waiting on the person.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::oneshot;

use crate::config::{Config, Repo};
use crate::pending::Store;
use crate::pipe;
use crate::prefs;

/// Per repo: the outgoing list the page drew last. A push must name its head.
#[derive(Default)]
pub struct Shown(Mutex<HashMap<String, dropin_git::Outgoing>>);

/// Per repo: `dropin push-review` calls waiting for the person's Push or Cancel, each with whether the
/// person opened it (`--by-person`, e.g. from a status-line button) rather than Claude asking for a review.
#[derive(Default)]
pub struct Waiting(Mutex<HashMap<String, Vec<(oneshot::Sender<Value>, bool)>>>);

impl Waiting {
    pub fn add(&self, repo: &str, answer: oneshot::Sender<Value>, by_person: bool) {
        self.0.lock().unwrap().entry(repo.to_string()).or_default().push((answer, by_person));
    }

    /// Who waits on the repo's review: "claude" when Claude asked for one, "person" when only the
    /// person's own button opened it (it still carries a Rebase conflict back), else false.
    fn who(&self, repo: &str) -> Value {
        let waiting = self.0.lock().unwrap();
        let list = waiting.get(repo).map(Vec::as_slice).unwrap_or_default();
        if list.iter().any(|(_, by_person)| !by_person) {
            json!("claude")
        } else if list.is_empty() {
            json!(false)
        } else {
            json!("person")
        }
    }

    /// Whether any call waits on a review, in any repo.
    pub fn any(&self) -> bool {
        self.0.lock().unwrap().values().any(|list| !list.is_empty())
    }

    /// Answers every call waiting on one repo.
    fn answer(&self, repo: &str, value: &Value) {
        for (waiter, _) in self.0.lock().unwrap().remove(repo).unwrap_or_default() {
            let _ = waiter.send(value.clone());
        }
    }

    /// Answers every waiting call `cancelled`: the window went away, so nobody is reviewing.
    pub fn cancel_all(&self) {
        for (repo, list) in self.0.lock().unwrap().drain() {
            for (waiter, _) in list {
                let _ = waiter.send(json!({
                    "ok": false,
                    "status": "cancelled",
                    "repo": repo,
                    "error": "the person closed the push dialog without pushing",
                }));
            }
        }
    }
}

/// The repo a command last asked the window for. The page reads it when it starts, since an
/// event sent while the app was still starting reaches no page.
#[derive(Default)]
pub struct Wanted(Mutex<Option<String>>);

/// Asks the page to show a repo: `select-repo`, or `review` to read it with a fetch.
pub fn want(app: &AppHandle, name: &str, event: &str) {
    *app.state::<Wanted>().0.lock().unwrap() = Some(name.to_string());
    let _ = app.emit(event, name);
}

/// The repo last asked for, if any.
#[tauri::command]
pub fn wanted(app: AppHandle) -> Option<String> {
    app.state::<Wanted>().0.lock().unwrap().clone()
}

/// A configured repo by name.
pub fn repo_named(app: &AppHandle, name: &str) -> Result<Repo, String> {
    let state = app.state::<Mutex<Config>>();
    let config = state.lock().unwrap();
    config.find(name).cloned().ok_or_else(|| format!("no repo named '{name}'"))
}

/// Hides the window; any review it held is over. The diff window goes with it.
pub fn hide(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    close_diff_window(app.clone());
    app.state::<Waiting>().cancel_all();
}

/// Opens the diff in its own window, or brings that window forward. It shows only while the main
/// window does, so a hidden app (a test, the tray) never pops a window up.
///
/// Async: building a window inside a synchronous command deadlocks on Windows.
#[tauri::command]
pub async fn diff_window(app: AppHandle) -> Result<(), String> {
    let shown = app.get_webview_window("main").and_then(|w| w.is_visible().ok()).unwrap_or(false);
    let window = match app.get_webview_window("diff") {
        Some(window) => window,
        None => {
            let window = WebviewWindowBuilder::new(&app, "diff", WebviewUrl::App("diff.html".into()))
                .title("Diff")
                .inner_size(1100.0, 800.0)
                .min_inner_size(480.0, 300.0)
                .visible(false)
                .build()
                .map_err(|e| e.to_string())?;
            prefs::restore(&window);
            window
        }
    };
    if shown {
        prefs::show(&window);
    }
    Ok(())
}

/// Closes the diff window, when there is one; the main window hears `diff-window-closed`.
#[tauri::command]
pub fn close_diff_window(app: AppHandle) {
    if let Some(window) = app.get_webview_window("diff") {
        let _ = window.destroy();
    }
}

/// What a push of the repo would send, plus its kept conflicts and whether Claude waits on a review.
/// `fetch` refreshes the upstream first.
#[tauri::command]
pub async fn outgoing(app: AppHandle, name: String, fetch: bool) -> Result<Value, String> {
    let repo = repo_named(&app, &name)?;
    let path = repo.path.clone();
    let read = pipe::blocking(move || dropin_git::read_outgoing(Path::new(&path), fetch, None)).await;
    let kept = app.state::<Store>().of_repo(&name);
    let waiting = app.state::<Waiting>().who(&name);
    let o = match read {
        Ok(o) => o,
        // No upstream, a detached HEAD: the page shows why, and the kept conflicts still.
        Err(error) => {
            app.state::<Shown>().0.lock().unwrap().remove(&name);
            return Ok(json!({ "repo": name, "path": repo.path, "error": error, "kept": kept, "waiting": waiting }));
        }
    };
    app.state::<Shown>().0.lock().unwrap().insert(name.clone(), o.clone());
    let mut value = serde_json::to_value(&o).map_err(|e| e.to_string())?;
    value["repo"] = json!(name);
    value["path"] = json!(repo.path);
    value["kept"] = json!(kept);
    value["waiting"] = json!(waiting);
    Ok(value)
}

/// The files a commit changed, in tree order.
#[tauri::command]
pub async fn files(app: AppHandle, name: String, sha: String) -> Result<Vec<dropin_git::FileChange>, String> {
    let repo = repo_named(&app, &name)?;
    pipe::blocking(move || dropin_git::files_of(Path::new(&repo.path), &sha)).await
}

/// One file's raw diff in a commit; the page shapes it.
#[tauri::command]
pub async fn diff(app: AppHandle, name: String, sha: String, file: dropin_git::FileChange) -> Result<Value, String> {
    let repo = repo_named(&app, &name)?;
    let (raw, truncated) = pipe::blocking(move || dropin_git::diff_of(Path::new(&repo.path), &sha, &file)).await?;
    Ok(json!({ "raw": raw, "truncated": truncated }))
}

/// Pushes the list the page drew, named by its head: when the page's head is not the one read
/// last, the person has not seen the current list.
#[tauri::command]
pub async fn push(app: AppHandle, name: String, head: String) -> Result<Value, String> {
    let repo = repo_named(&app, &name)?;
    let o = app.state::<Shown>().0.lock().unwrap().get(&name).cloned();
    let o = o.filter(|o| o.head == head).ok_or("The list changed since it was drawn; check it again before pushing.")?;
    let _held = pipe::hold(&app, &repo, "push")?;
    let path = repo.path.clone();
    let sent = o.clone();
    let pushed = pipe::blocking(move || dropin_git::push(Path::new(&path), &sent)).await?;
    if pushed.notice.tone == dropin_git::Tone::Success {
        let subjects: Vec<&str> = o.commits.iter().map(|c| c.subject.as_str()).collect();
        app.state::<Waiting>().answer(&name, &json!({
            "ok": true,
            "status": "pushed",
            "repo": name,
            "head": o.head,
            "upstream": o.upstream,
            "commits": subjects,
        }));
        let _ = app.emit("repo-changed", &name);
    }
    serde_json::to_value(pushed).map_err(|e| e.to_string())
}

/// Rebases onto the upstream in a worktree, as `dropin rebase` does. A conflict ends a waiting
/// `dropin push-review` with status `conflict`: the session that opened the window resolves it.
#[tauri::command]
pub async fn rebase(app: AppHandle, name: String) -> Result<Value, String> {
    let repo = repo_named(&app, &name)?;
    let value = pipe::rebase(&app, repo).await?;
    if value["status"] == "conflict" {
        let mut handed = value.clone();
        handed["ok"] = json!(false);
        handed["startedBy"] = json!("window");
        handed["repo"] = json!(name);
        app.state::<Waiting>().answer(&name, &handed);
    }
    Ok(value)
}

/// Moves the branch onto a rebase finished in its kept worktree, as `dropin adopt` does.
#[tauri::command]
pub async fn adopt(app: AppHandle, name: String, worktree: String) -> Result<Value, String> {
    let repo = repo_named(&app, &name)?;
    pipe::adopt(&app, repo, Some(worktree)).await
}

/// Drops a kept conflict and its worktree, as `dropin abandon` does.
#[tauri::command]
pub async fn abandon(app: AppHandle, name: String, worktree: String) -> Result<Value, String> {
    let repo = repo_named(&app, &name)?;
    pipe::abandon(&app, repo, Some(worktree)).await
}

/// Cancel: hides the window and answers any waiting review `cancelled`.
#[tauri::command]
pub fn cancel(app: AppHandle) {
    hide(&app);
}

/// `dropin push-review`: shows the dialog on the repo and answers once the person pushes or closes it.
pub async fn push_review(app: &AppHandle, repo: Repo, by_person: bool) -> Result<Value, String> {
    let (answer, answered) = oneshot::channel();
    app.state::<Waiting>().add(&repo.name, answer, by_person);
    // The page selects the repo and reads it with a fetch, so the review starts current.
    want(app, &repo.name, "review");
    pipe::focus(app).await?;
    answered.await.map_err(|_| "Drop In quit before the person answered".to_string())
}
