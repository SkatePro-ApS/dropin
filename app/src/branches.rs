//! The Branches column's commands: list, switch, create, rename and delete. Each step holds the
//! repo like a push or rebase does, and a step that changed something tells the page to reread.

use std::path::Path;

use tauri::{AppHandle, Emitter, Manager};

use crate::config::Repo;
use crate::dialog::repo_named;
use crate::pending::Store;
use crate::pipe;

/// The repo's local and remote-tracking branches.
#[tauri::command]
pub async fn branches(app: AppHandle, name: String) -> Result<dropin_git::Branches, String> {
    let repo = repo_named(&app, &name)?;
    pipe::blocking(move || dropin_git::list_branches(Path::new(&repo.path))).await
}

/// Runs one branch step on the repo while holding it, and asks the page to reread when it worked.
async fn step(
    app: &AppHandle,
    repo: Repo,
    what: &'static str,
    work: impl FnOnce(&Path) -> Result<dropin_git::Notice, String> + Send + 'static,
) -> Result<dropin_git::Notice, String> {
    let _held = pipe::hold(app, &repo, what)?;
    let path = repo.path.clone();
    let notice = pipe::blocking(move || work(Path::new(&path))).await?;
    if notice.tone == dropin_git::Tone::Success {
        let _ = app.emit("repo-changed", &repo.name);
    }
    Ok(notice)
}

/// A kept rebase conflict still means to move this branch; renaming or deleting it would strand that.
fn refuse_if_kept(app: &AppHandle, repo: &Repo, branch: &str) -> Result<(), String> {
    let kept = app.state::<Store>().of_repo(&repo.name);
    if kept.iter().any(|p| p.conflict.branch == branch) {
        return Err(format!("{branch} has a kept rebase conflict; adopt or abandon it first."));
    }
    Ok(())
}

#[tauri::command]
pub async fn branch_switch(app: AppHandle, name: String, branch: String, remote: String) -> Result<dropin_git::Notice, String> {
    let repo = repo_named(&app, &name)?;
    step(&app, repo, "branch switch", move |dir| dropin_git::switch_branch(dir, &branch, &remote)).await
}

#[tauri::command]
pub async fn branch_create(app: AppHandle, name: String, branch: String, base: String, switch: bool) -> Result<dropin_git::Notice, String> {
    let repo = repo_named(&app, &name)?;
    step(&app, repo, "new branch", move |dir| dropin_git::create_branch(dir, &branch, &base, switch)).await
}

#[tauri::command]
pub async fn branch_rename(app: AppHandle, name: String, from: String, to: String) -> Result<dropin_git::Notice, String> {
    let repo = repo_named(&app, &name)?;
    refuse_if_kept(&app, &repo, &from)?;
    step(&app, repo, "branch rename", move |dir| dropin_git::rename_branch(dir, &from, &to)).await
}

#[tauri::command]
pub async fn branch_delete(app: AppHandle, name: String, branch: String, force: bool) -> Result<dropin_git::Notice, String> {
    let repo = repo_named(&app, &name)?;
    refuse_if_kept(&app, &repo, &branch)?;
    step(&app, repo, "branch delete", move |dir| dropin_git::delete_branch(dir, &branch, force)).await
}

#[tauri::command]
pub async fn branch_delete_remote(app: AppHandle, name: String, remote: String, branch: String) -> Result<dropin_git::Notice, String> {
    let repo = repo_named(&app, &name)?;
    step(&app, repo, "remote branch delete", move |dir| dropin_git::delete_remote_branch(dir, &remote, &branch)).await
}

#[tauri::command]
pub async fn branch_fetch(app: AppHandle, name: String) -> Result<dropin_git::Notice, String> {
    let repo = repo_named(&app, &name)?;
    step(&app, repo, "fetch", dropin_git::fetch_all).await
}

/// Renames every local branch under a folder: `from/x` becomes `to/x`.
#[tauri::command]
pub async fn branch_rename_folder(app: AppHandle, name: String, from: String, to: String) -> Result<dropin_git::Notice, String> {
    let repo = repo_named(&app, &name)?;
    let path = repo.path.clone();
    let folder = from.clone();
    for branch in pipe::blocking(move || dropin_git::branches_in_folder(Path::new(&path), &folder)).await? {
        refuse_if_kept(&app, &repo, &branch)?;
    }
    step(&app, repo, "folder rename", move |dir| dropin_git::rename_folder(dir, &from, &to)).await
}

/// Deletes the local branches under a folder, or only `branches` (each under it) on the second,
/// forced step the page asks about.
#[tauri::command]
pub async fn branch_delete_folder(
    app: AppHandle,
    name: String,
    folder: String,
    force: bool,
    branches: Option<Vec<String>>,
) -> Result<dropin_git::FolderDelete, String> {
    let repo = repo_named(&app, &name)?;
    let _held = pipe::hold(&app, &repo, "folder delete")?;
    let path = repo.path.clone();
    let names = pipe::blocking(move || dropin_git::branches_in_folder(Path::new(&path), &folder)).await?;
    let names: Vec<String> = match branches {
        Some(only) => names.into_iter().filter(|n| only.contains(n)).collect(),
        None => names,
    };
    for branch in &names {
        refuse_if_kept(&app, &repo, branch)?;
    }
    let path = repo.path.clone();
    let done = pipe::blocking(move || dropin_git::delete_branches(Path::new(&path), &names, force)).await?;
    // Some went even when others wait for a yes: the lists change either way.
    let _ = app.emit("repo-changed", &repo.name);
    Ok(done)
}
