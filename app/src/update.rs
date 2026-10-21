//! Updates from the GitHub releases: the page asks whether a newer version is out, and on the
//! person's click the app downloads the signed installer, runs it and restarts on the new version.
//!
//! The updater plugin checks each installer against the public key in `tauri.conf.json` before it
//! runs it, so a download that was not signed by the release workflow is refused.

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

use crate::dialog::Waiting;
use crate::pipe::Busy;

/// The running version and, when the latest release is newer, that release's version and notes.
#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<Value, String> {
    let current = app.package_info().version.to_string();
    let update = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?;
    Ok(match update {
        Some(update) => json!({
            "current": current,
            "available": update.version,
            "notes": update.body.unwrap_or_default(),
        }),
        None => json!({ "current": current, "available": null }),
    })
}

/// Downloads and installs the latest release, then the installer starts the new version.
///
/// Refused while a review waits or a rebase, adopt or push runs: the app exits for the installer,
/// and that would cut them short.
#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<(), String> {
    if app.state::<Waiting>().any() {
        return Err("a push review is waiting; push or cancel it first".into());
    }
    if let Some(running) = app.state::<Busy>().running() {
        return Err(format!("a {running} is running; update when it ends"));
    }
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("Drop In is already up to date")?;

    let mut done = 0usize;
    let progress = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                done += chunk;
                let _ = progress.emit_to("main", "update-progress", json!({ "done": done, "total": total }));
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string())?;
    // On Windows the installer has taken over and the app has exited by now; elsewhere restart.
    app.restart();
}
