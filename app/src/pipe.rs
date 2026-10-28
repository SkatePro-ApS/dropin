//! The named pipe the `dropin` CLI talks to: one JSON request line in, one JSON answer line out.

use std::io::{BufRead, BufReader, Write};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::config::{self, Config, Repo};
use crate::pending::{Pending, Store};

/// The pipe's name; per user, so two accounts on one machine never share an app.
/// cli/src/main.rs builds the same name.
pub fn name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let user: String = user.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    format!(r"\\.\pipe\dropin-{user}")
}

/// Claims the pipe; fails when another instance already holds it (the single-instance check).
pub fn create_first() -> std::io::Result<NamedPipeServer> {
    ServerOptions::new().first_pipe_instance(true).create(name())
}

/// Sends one request to the running instance and waits for its answer.
pub fn send_to_running(line: &str) -> std::io::Result<String> {
    let pipe = std::fs::OpenOptions::new().read(true).write(true).open(name())?;
    (&pipe).write_all(format!("{line}\n").as_bytes())?;
    let mut answer = String::new();
    BufReader::new(&pipe).read_line(&mut answer)?;
    Ok(answer)
}

/// Accepts clients for as long as the app runs, each answered on its own task,
/// so a request that waits on the person never holds up the others.
pub async fn serve(app: AppHandle, mut server: NamedPipeServer) {
    loop {
        let connected = server.connect().await;
        let next = match ServerOptions::new().create(name()) {
            Ok(next) => next,
            Err(e) => {
                eprintln!("Drop In: cannot create the next pipe instance: {e}");
                return;
            }
        };
        let client = std::mem::replace(&mut server, next);
        if connected.is_ok() {
            tauri::async_runtime::spawn(handle(app.clone(), client));
        }
    }
}

/// Reads one request, answers it, then waits for the client to hang up so the answer is read
/// before the pipe closes.
async fn handle(app: AppHandle, pipe: NamedPipeServer) {
    let mut reader = tokio::io::BufReader::new(pipe);
    let mut line = String::new();
    if reader.read_line(&mut line).await.is_err() {
        return;
    }
    let answer = match serde_json::from_str::<Value>(&line) {
        Ok(request) => dispatch(&app, &request).await.unwrap_or_else(|e| json!({ "ok": false, "error": e })),
        Err(e) => json!({ "ok": false, "error": format!("bad request: {e}") }),
    };
    let mut pipe = reader.into_inner();
    let _ = pipe.write_all(format!("{answer}\n").as_bytes()).await;
    let _ = pipe.flush().await;
    let mut rest = [0u8; 64];
    let _ = pipe.read(&mut rest).await;
}

/// Runs one CLI command. Requests carry `cmd`, `args` (positionals), `cwd` and any `--flag` as a key.
async fn dispatch(app: &AppHandle, request: &Value) -> Result<Value, String> {
    let cmd = request["cmd"].as_str().unwrap_or("");
    let args: Vec<&str> = request["args"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    match cmd {
        "ping" => Ok(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION"), "pid": std::process::id() })),
        "show" => {
            // Selecting a repo is a courtesy here: show works from any folder.
            if let Ok(repo) = resolve(app, request) {
                crate::dialog::want(app, &repo.name, "select-repo");
            }
            focus(app).await
        }
        // The window as the person left it: focus never changes the repo shown.
        "focus" => focus(app).await,
        "quit" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                app.exit(0);
            });
            Ok(json!({ "ok": true }))
        }
        "repos" => {
            let state = app.state::<Mutex<Config>>();
            let config = state.lock().unwrap();
            Ok(json!({ "ok": true, "repos": config.repos }))
        }
        "repo" => match args.first().copied() {
            Some("add") => repo_add(app, request, args.get(1).copied()).await,
            Some("remove") => repo_remove(app, args.get(1).copied()),
            _ => Err("usage: dropin repo add [path] [--name <name>] | dropin repo remove <name>".into()),
        },
        "status" => status_of(resolve(app, request)?).await,
        "outgoing" => {
            let preview = request["preview"].as_str().map(String::from);
            outgoing(resolve(app, request)?, request["fetch"] == json!(true), preview).await
        }
        "push-review" => crate::dialog::push_review(app, resolve(app, request)?, request["by-person"] == json!(true)).await,
        "rebase" => rebase(app, resolve(app, request)?, request["shelve"] == json!(true)).await,
        "adopt" => {
            let worktree = request["worktree"].as_str().map(String::from);
            adopt(app, resolve(app, request)?, worktree, request["shelve"] == json!(true)).await
        }
        "abandon" => abandon(app, resolve(app, request)?, request["worktree"].as_str().map(String::from)).await,
        "pending" => {
            let repo = resolve(app, request)?;
            Ok(json!({ "ok": true, "repo": repo.name, "pending": app.state::<Store>().of_repo(&repo.name) }))
        }
        "" => Err("no command; try `dropin help`".into()),
        other => Err(format!("unknown command '{other}'; try `dropin help`")),
    }
}

/// The repo a request is about: `--repo <name>`, else the one holding the caller's folder.
fn resolve(app: &AppHandle, request: &Value) -> Result<Repo, String> {
    let state = app.state::<Mutex<Config>>();
    let config = state.lock().unwrap();
    if let Some(name) = request["repo"].as_str() {
        return config
            .find(name)
            .cloned()
            .ok_or_else(|| format!("no repo named '{name}'; known: {}", config.names()));
    }
    let cwd = request["cwd"].as_str().unwrap_or("");
    config.for_dir(cwd).cloned().ok_or_else(|| {
        format!(
            "{cwd} is in no known repo; add it with `dropin repo add`, or pass --repo <name> (known: {})",
            config.names()
        )
    })
}

/// Shows and focuses the window. Windows may refuse the focus to a background app;
/// the taskbar button then flashes instead, and the answer says which happened.
pub async fn focus(app: &AppHandle) -> Result<Value, String> {
    let window = app.get_webview_window("main").ok_or("the main window is gone")?;
    crate::show_window(app);
    tokio::time::sleep(Duration::from_millis(150)).await;
    let focused = window.is_focused().unwrap_or(false);
    if !focused {
        let _ = window.request_user_attention(Some(tauri::UserAttentionType::Informational));
    }
    Ok(json!({ "ok": true, "focused": focused, "flashed": !focused }))
}

/// Reads one repo's status off the async runtime (git can take a while on a big repo).
pub async fn status_of(repo: Repo) -> Result<Value, String> {
    let path = repo.path.clone();
    let mut status = tauri::async_runtime::spawn_blocking(move || dropin_git::status(Path::new(&path)))
        .await
        .map_err(|e| e.to_string())??;
    status["ok"] = json!(true);
    status["repo"] = json!(repo.name);
    status["path"] = json!(repo.path);
    Ok(status)
}

/// `dropin repo add [path] [--name <name>]`: the path defaults to the caller's folder, the name to
/// the work tree's folder name.
async fn repo_add(app: &AppHandle, request: &Value, path: Option<&str>) -> Result<Value, String> {
    // A relative path is the caller's, not the app's: the app's own folder is wherever it started.
    let cwd = request["cwd"].as_str().unwrap_or("");
    let dir = match path {
        Some(p) => Path::new(cwd).join(p).to_string_lossy().into_owned(),
        None => cwd.to_string(),
    };
    let top = tauri::async_runtime::spawn_blocking(move || dropin_git::toplevel(Path::new(&dir)))
        .await
        .map_err(|e| e.to_string())??;
    let name = match request["name"].as_str() {
        Some(name) => name.to_string(),
        None => top.rsplit('/').next().unwrap_or(&top).to_string(),
    };
    let repo = crate::repos::add(app, Repo { name, path: top })?;
    Ok(json!({ "ok": true, "repo": repo }))
}

/// `dropin repo remove <name>`: forgets a repo; nothing on disk is touched.
fn repo_remove(app: &AppHandle, name: Option<&str>) -> Result<Value, String> {
    let name = name.ok_or("usage: dropin repo remove <name>")?;
    let removed = crate::repos::remove(app, name)?;
    Ok(json!({ "ok": true, "removed": removed }))
}

/// Repos with a rebase, adopt or push running: one at a time per repo, since each moves the branch.
#[derive(Default)]
pub struct Busy(Mutex<HashMap<String, &'static str>>);

/// Holds a repo's busy mark until dropped.
pub struct Held {
    app: AppHandle,
    key: String,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.app.state::<Busy>().0.lock().unwrap().remove(&self.key);
    }
}

impl Busy {
    /// What is running right now, if anything: an update must not cut a rebase or push short.
    pub fn running(&self) -> Option<&'static str> {
        self.0.lock().unwrap().values().next().copied()
    }
}

pub fn hold(app: &AppHandle, repo: &Repo, what: &'static str) -> Result<Held, String> {
    let key = config::norm(&repo.path);
    let state = app.state::<Busy>();
    let mut busy = state.0.lock().unwrap();
    if let Some(running) = busy.get(&key) {
        return Err(format!("{} is busy with a {running}; try again when it ends", repo.name));
    }
    busy.insert(key.clone(), what);
    Ok(Held { app: app.clone(), key })
}

/// Runs git work off the async runtime.
pub async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|e| e.to_string())?
}

/// Adds `ok`, `repo` and any extra fields to a serialized answer.
fn answer(value: impl serde::Serialize, repo: &Repo, extra: Value) -> Result<Value, String> {
    let mut v = serde_json::to_value(value).map_err(|e| e.to_string())?;
    let map = v.as_object_mut().ok_or("answer is not an object")?;
    map.insert("repo".into(), json!(repo.name));
    if let Value::Object(extra) = extra {
        map.extend(extra);
    }
    map.retain(|_, v| !v.is_null());
    map.entry("ok").or_insert(json!(true));
    Ok(v)
}

/// `dropin outgoing [--fetch] [--preview <ref>]`: what a push would send and what it lacks.
async fn outgoing(repo: Repo, fetch: bool, preview: Option<String>) -> Result<Value, String> {
    let path = repo.path.clone();
    let o = blocking(move || dropin_git::read_outgoing(Path::new(&path), fetch, preview.as_deref())).await?;
    answer(o, &repo, json!({}))
}

/// `dropin rebase`: fetches, then rebases the branch onto its upstream in a worktree. A conflict
/// keeps that worktree for Claude to finish; `dropin adopt` then moves the branch onto the result.
/// `--shelve` shelves uncommitted work in the incoming files while the branch moves, then puts it back.
pub async fn rebase(app: &AppHandle, repo: Repo, shelve: bool) -> Result<Value, String> {
    let _held = hold(app, &repo, "rebase")?;
    // A new rebase would reuse the kept conflict's worktree name and take that work away.
    if let Some(kept) = app.state::<Store>().of_repo(&repo.name).first() {
        return Err(format!(
            "{} has a kept conflict in {}: finish it with `dropin adopt`, or drop it with `dropin abandon`, first",
            repo.name, kept.conflict.worktree
        ));
    }
    let path = repo.path.clone();
    let (o, rebased) = blocking(move || {
        let dir = Path::new(&path);
        let o = dropin_git::read_outgoing(dir, true, None)?;
        if o.behind == 0 {
            return Ok((o, None));
        }
        let rebased = dropin_git::rebase_onto(dir, &o, &dropin_git::worktree_base(dir), shelve)?;
        Ok((o, Some(rebased)))
    })
    .await?;
    let base = json!({ "branch": o.branch, "upstream": o.upstream, "fetchError": o.fetch_error });
    let Some(rebased) = rebased else {
        return answer(base, &repo, json!({ "status": "up-to-date", "ahead": o.ahead }));
    };
    let _ = app.emit("repo-changed", &repo.name);
    if let Some(c) = rebased.conflict {
        app.state::<Store>().add(Pending { repo: repo.name.clone(), conflict: c.clone() })?;
        let subjects = |list: &[dropin_git::Commit]| list.iter().map(|c| c.subject.clone()).collect::<Vec<_>>();
        let next = format!(
            "In \"{wt}\" (detached at the branch's commit): git rebase {onto}, resolve each conflict keeping both sides' intent, \
             git add, git rebase --continue until done; then run `dropin adopt --worktree \"{wt}\"`. To give up: `dropin abandon --worktree \"{wt}\"`.",
            wt = c.worktree,
            onto = c.onto
        );
        return answer(c, &repo, json!({
            "status": "conflict",
            "mine": subjects(&o.commits),
            "incoming": subjects(&o.incoming),
            "next": next,
        }));
    }
    let notice = rebased.notice.ok_or("the rebase gave no answer")?;
    let failed = notice.tone == dropin_git::Tone::Error;
    answer(base, &repo, json!({
        "ok": !failed,
        "status": if failed { "refused" } else { "rebased" },
        "error": if failed { Some(&notice.text) } else { None },
        "notice": notice,
        "inTheWay": rebased.in_the_way,
    }))
}

/// `dropin adopt [--worktree <path>]`: checks a finished rebase in a kept worktree and moves the
/// branch onto it. A refusal keeps the worktree, so the work can be fixed and adopted again.
/// `--shelve` shelves uncommitted work in the files it changes while the branch moves.
pub async fn adopt(app: &AppHandle, repo: Repo, worktree: Option<String>, shelve: bool) -> Result<Value, String> {
    let pending = app.state::<Store>().find(&repo.name, worktree.as_deref())?;
    let _held = hold(app, &repo, "adopt")?;
    let (path, c) = (repo.path.clone(), pending.conflict.clone());
    let adopted = blocking(move || {
        let dir = Path::new(&path);
        let adopted = dropin_git::adopt(dir, &c, shelve)?;
        if adopted.to.is_some() {
            dropin_git::remove_worktree(dir, &c.worktree);
        }
        Ok(adopted)
    })
    .await?;
    let moved = adopted.to.is_some();
    if moved {
        app.state::<Store>().remove(&pending.conflict.worktree)?;
        let _ = app.emit("repo-changed", &repo.name);
    }
    let error = (!moved).then(|| adopted.notice.text.clone());
    answer(adopted, &repo, json!({
        "ok": moved,
        "status": if moved { "adopted" } else { "refused" },
        "error": error,
        "worktree": pending.conflict.worktree,
    }))
}

/// `dropin abandon [--worktree <path>]`: drops a kept conflict and its worktree; the branch was never touched.
pub async fn abandon(app: &AppHandle, repo: Repo, worktree: Option<String>) -> Result<Value, String> {
    let pending = app.state::<Store>().find(&repo.name, worktree.as_deref())?;
    let (path, wt) = (repo.path.clone(), pending.conflict.worktree.clone());
    blocking(move || Ok(dropin_git::remove_worktree(Path::new(&path), &wt))).await?;
    app.state::<Store>().remove(&pending.conflict.worktree)?;
    let _ = app.emit("repo-changed", &repo.name);
    Ok(json!({ "ok": true, "repo": repo.name, "removed": pending.conflict.worktree }))
}
