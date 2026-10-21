//! The Uncommitted row's commands: list the work tree's changes, diff one, commit some, roll some
//! back, and read an image for the diff's preview.

use std::path::Path;

use serde_json::{json, Value};
use tauri::ipc::Response;
use tauri::AppHandle;

use crate::dialog::repo_named;
use crate::pipe;

/// The repo's uncommitted changes, with HEAD's message and whether it is pushed (for Amend).
#[tauri::command]
pub async fn changes(app: AppHandle, name: String) -> Result<dropin_git::Changes, String> {
    let repo = repo_named(&app, &name)?;
    pipe::blocking(move || dropin_git::read_changes(Path::new(&repo.path))).await
}

/// One uncommitted file's raw diff against HEAD; the page shapes it.
#[tauri::command]
pub async fn working_diff(app: AppHandle, name: String, file: dropin_git::FileChange) -> Result<Value, String> {
    let repo = repo_named(&app, &name)?;
    let (raw, truncated) = pipe::blocking(move || dropin_git::working_diff(Path::new(&repo.path), &file)).await?;
    Ok(json!({ "raw": raw, "truncated": truncated }))
}

/// Commits the ticked files, holding the repo so no push or rebase runs meanwhile.
#[tauri::command]
pub async fn commit(app: AppHandle, name: String, message: String, files: Vec<dropin_git::FileChange>, amend: bool) -> Result<dropin_git::Committed, String> {
    let repo = repo_named(&app, &name)?;
    let _held = pipe::hold(&app, &repo, "commit")?;
    pipe::blocking(move || dropin_git::commit_changes(Path::new(&repo.path), &message, &files, amend)).await
}

/// Rolls the files back to HEAD; the page asked first.
#[tauri::command]
pub async fn rollback(app: AppHandle, name: String, files: Vec<dropin_git::FileChange>) -> Result<(), String> {
    let repo = repo_named(&app, &name)?;
    let _held = pipe::hold(&app, &repo, "rollback")?;
    pipe::blocking(move || dropin_git::rollback(Path::new(&repo.path), &files)).await
}

/// A file's bytes at `rev`, or in the work tree when `rev` is empty, for the image preview.
#[tauri::command]
pub async fn image(app: AppHandle, name: String, rev: String, path: String) -> Result<Response, String> {
    let repo = repo_named(&app, &name)?;
    let bytes = pipe::blocking(move || dropin_git::image_of(Path::new(&repo.path), &rev, &path)).await?;
    Ok(Response::new(bytes))
}
