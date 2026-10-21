//! Adding and removing repos, from the window's repo menu or the `dropin repo` commands.

use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::config::{self, Config, Repo};
use crate::pending::Store;

/// A folder the person picked, as the repo it would be added as.
#[derive(Serialize)]
pub struct Picked {
    /// The work tree's top level: picking a subfolder adds the whole repo.
    pub path: String,
    /// The work tree's folder name, or that name with a number when it is taken.
    pub name: String,
}

/// Opens Windows' folder picker, starting next to the repo `near`, and answers the repo the folder
/// is in. None when the person cancelled.
#[tauri::command]
pub async fn repo_pick(app: AppHandle, near: Option<String>) -> Result<Option<Picked>, String> {
    let start = near.and_then(|name| {
        let config = app.state::<Mutex<Config>>();
        let config = config.lock().unwrap();
        let path = config.find(&name)?.path.clone();
        Path::new(&path).parent().map(Path::to_path_buf)
    });
    let window = app.get_webview_window("main").ok_or("the main window is gone")?;
    let handle = app.clone();
    let folder = tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = handle.dialog().file().set_title("Add a Git Repo").set_parent(&window);
        if let Some(start) = start {
            dialog = dialog.set_directory(start);
        }
        dialog.blocking_pick_folder()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(folder) = folder else { return Ok(None) };
    let folder = folder.into_path().map_err(|e| e.to_string())?;
    let dir = folder.to_string_lossy().into_owned();
    let top = tauri::async_runtime::spawn_blocking(move || dropin_git::toplevel(Path::new(&dir)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|_| format!("{} is not in a Git repo.", folder.display()))?;
    let config = app.state::<Mutex<Config>>();
    let config = config.lock().unwrap();
    if let Some(same) = added_as(&config, &top) {
        return Err(format!("{top} is added already, as '{}'.", same.name));
    }
    let base = top.rsplit('/').next().unwrap_or(&top).to_string();
    let name = (1..)
        .map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") })
        .find(|name| config.find(name).is_none())
        .unwrap_or(base);
    Ok(Some(Picked { path: top, name }))
}

/// Adds the repo at `path` (a work tree's top level, as `repo_pick` answered it) as `name`.
#[tauri::command]
pub fn repo_add(app: AppHandle, path: String, name: String) -> Result<Repo, String> {
    add(&app, Repo { name: name.trim().to_string(), path })
}

/// Forgets the repo `name`; nothing on disk is touched.
#[tauri::command]
pub fn repo_remove(app: AppHandle, name: String) -> Result<String, String> {
    remove(&app, &name)
}

fn added_as<'a>(config: &'a Config, path: &str) -> Option<&'a Repo> {
    config.repos.iter().find(|r| config::norm(&r.path) == config::norm(path))
}

/// Adds `repo` to config.json and tells the window, which switches to it.
pub fn add(app: &AppHandle, repo: Repo) -> Result<Repo, String> {
    if repo.name.is_empty() {
        return Err("Give the repo a name.".into());
    }
    {
        let state = app.state::<Mutex<Config>>();
        let mut config = state.lock().unwrap();
        if let Some(same) = added_as(&config, &repo.path) {
            return Err(format!("{} is added already, as '{}'.", repo.path, same.name));
        }
        if config.find(&repo.name).is_some() {
            return Err(format!("The name '{}' is taken; pick another.", repo.name));
        }
        config.repos.push(repo.clone());
        if let Err(e) = config.save() {
            config.repos.pop();
            return Err(e);
        }
    }
    let _ = app.emit("repos-changed", &repo.name);
    Ok(repo)
}

/// Forgets a repo: its kept conflict, if any, must be adopted or abandoned first, since the conflict
/// is reached through the repo's name.
pub fn remove(app: &AppHandle, name: &str) -> Result<String, String> {
    if let Some(kept) = app.state::<Store>().of_repo(name).first() {
        return Err(format!("{name} has a kept conflict in {}: adopt or abandon it first.", kept.conflict.worktree));
    }
    let state = app.state::<Mutex<Config>>();
    let mut config = state.lock().unwrap();
    let index = config
        .repos
        .iter()
        .position(|r| r.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("no repo named '{name}'; known: {}", config.names()))?;
    let removed = config.repos.remove(index);
    if let Err(e) = config.save() {
        config.repos.insert(index, removed);
        return Err(e);
    }
    drop(config);
    let _ = app.emit("repos-changed", "");
    Ok(removed.name)
}
