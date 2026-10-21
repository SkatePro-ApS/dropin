#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod branches;
mod changes;
mod config;
mod dialog;
mod pending;
mod pipe;
mod prefs;
mod repos;
mod update;

use std::sync::Mutex;

use serde_json::Value;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};

/// Starts Drop In: one instance per user, resident in the tray, serving the `dropin` CLI over a named pipe.
///
/// `--background` (how the CLI starts it) keeps the window hidden until a command asks for it.
fn main() {
    let background = std::env::args().any(|arg| arg == "--background");
    let server = match tauri::async_runtime::block_on(async { pipe::create_first() }) {
        Ok(server) => server,
        Err(_) => {
            // Another instance owns the pipe. A person starting the app wants its window;
            // the CLI losing a start-up race does not.
            if !background {
                let _ = pipe::send_to_running(r#"{"cmd":"show"}"#);
            }
            return;
        }
    };
    // This is the instance now, so an update that was installing has finished (or was cancelled).
    update::clear_updating();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(Mutex::new(config::load()))
        .manage(pending::Store::load())
        .manage(pipe::Busy::default())
        .manage(dialog::Shown::default())
        .manage(dialog::Waiting::default())
        .manage(dialog::Wanted::default())
        .manage(prefs::Prefs::load())
        .invoke_handler(tauri::generate_handler![
            list_repos,
            repos::repo_pick,
            repos::repo_add,
            repos::repo_remove,
            repo_status,
            dialog::wanted,
            dialog::outgoing,
            dialog::files,
            dialog::diff,
            dialog::push,
            dialog::rebase,
            dialog::adopt,
            dialog::abandon,
            dialog::cancel,
            dialog::diff_window,
            dialog::close_diff_window,
            prefs::prefs,
            prefs::set_pref,
            branches::branches,
            branches::branch_switch,
            branches::branch_create,
            branches::branch_rename,
            branches::branch_delete,
            branches::branch_delete_remote,
            branches::branch_fetch,
            branches::branch_rename_folder,
            branches::branch_delete_folder,
            changes::changes,
            changes::working_diff,
            changes::commit,
            changes::rollback,
            changes::image,
            update::update_check,
            update::update_install,
        ])
        .setup(move |app| {
            tauri::async_runtime::spawn(pipe::serve(app.handle().clone(), server));
            build_tray(app)?;
            if let Some(window) = app.get_webview_window("main") {
                prefs::restore(&window);
            }
            if !background {
                show_window(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| match (window.label(), event) {
            (_, WindowEvent::Resized(_) | WindowEvent::Moved(_)) => prefs::record(window),
            // The diff window really closes; the main window's page puts the diff away.
            ("diff", WindowEvent::Destroyed) => {
                let _ = window.app_handle().emit_to("main", "diff-window-closed", ());
            }
            ("diff", _) => {}
            (_, WindowEvent::CloseRequested { api, .. }) => {
                // Closing hides the window: the app stays resident so the CLI answers instantly.
                // A review waiting on it ends as cancelled.
                api.prevent_close();
                dialog::hide(window.app_handle());
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running Drop In");
}

/// Shows, restores and focuses the main window.
pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        prefs::show(&window);
    }
}

/// Adds the tray icon: a left click shows the window, the menu has Show and Quit.
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut tray = TrayIconBuilder::new()
        .tooltip("Drop In")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// The configured repos, for the window's repo switcher.
#[tauri::command]
fn list_repos(state: tauri::State<'_, Mutex<config::Config>>) -> Vec<config::Repo> {
    state.lock().unwrap().repos.clone()
}

/// Branch, upstream, ahead/behind and changed-file count of one configured repo.
#[tauri::command]
async fn repo_status(app: AppHandle, name: String) -> Result<Value, String> {
    pipe::status_of(dialog::repo_named(&app, &name)?).await
}
