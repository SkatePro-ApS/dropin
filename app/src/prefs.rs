//! The window's own settings, in `%USERPROFILE%\.config\dropin\ui.json`: each window's size and place,
//! and what the page keeps (pane and column sizes, side by side or unified, docked or not).
//!
//! The app writes the file, not the WebView's storage, for the reason config.json is not in AppData:
//! a Drop In Claude Desktop starts gets the package's private copy of AppData, so the person's own
//! start would read other settings.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Map, Value};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, Runtime, WebviewWindow, Window};

use crate::config;

pub struct Prefs {
    values: Mutex<Map<String, Value>>,
    /// A write is already due: a drag sends moves in bursts, so they are saved once it settles.
    due: AtomicBool,
}

fn file() -> PathBuf {
    config::dir().join("ui.json")
}

impl Prefs {
    /// Reads ui.json. A missing or broken file starts from the defaults: these are only sizes,
    /// unlike the repo list, so the next save may replace it.
    pub fn load() -> Prefs {
        let values = std::fs::read_to_string(file())
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| match value {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default();
        Prefs { values: Mutex::new(values), due: AtomicBool::new(false) }
    }

    fn save(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&*self.values.lock().unwrap()).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(config::dir()).map_err(|e| e.to_string())?;
        std::fs::write(file(), text).map_err(|e| format!("cannot write {}: {e}", file().display()))
    }

    fn window(&self, label: &str) -> Map<String, Value> {
        let values = self.values.lock().unwrap();
        values.get(&format!("window.{label}")).and_then(Value::as_object).cloned().unwrap_or_default()
    }
}

/// Every setting the page keeps, read once when it starts.
#[tauri::command]
pub fn prefs(app: AppHandle) -> Map<String, Value> {
    app.state::<Prefs>().values.lock().unwrap().clone()
}

/// Keeps one of the page's settings.
#[tauri::command]
pub fn set_pref(app: AppHandle, key: String, value: Value) -> Result<(), String> {
    let prefs = app.state::<Prefs>();
    prefs.values.lock().unwrap().insert(key, value);
    prefs.save()
}

/// Gives a window the size and place it was last left at, before it shows. The place only when it
/// is still on a screen: a screen unplugged since would hide the window.
pub fn restore<R: Runtime>(window: &WebviewWindow<R>) {
    let saved = window.app_handle().state::<Prefs>().window(window.label());
    let num = |key: &str| saved.get(key).and_then(Value::as_i64);
    if let (Some(width), Some(height)) = (num("width"), num("height")) {
        let _ = window.set_size(PhysicalSize::new(width.max(1) as u32, height.max(1) as u32));
    }
    if let (Some(x), Some(y)) = (num("x"), num("y")) {
        let monitors = window.available_monitors().unwrap_or_default();
        // The title bar's left end must be on a screen, so the window can be dragged back.
        let on_screen = monitors.iter().any(|m| {
            let (at, size) = (m.position(), m.size());
            let (left, top) = (at.x as i64, at.y as i64);
            x + 60 >= left && x + 60 < left + size.width as i64 && y >= top && y + 20 < top + size.height as i64
        });
        if on_screen {
            let _ = window.set_position(PhysicalPosition::new(x as i32, y as i32));
        }
    }
}

/// Shows, restores and focuses a window, maximized when it was left maximized. Maximizing waits
/// for the show: on Windows it would show a hidden window.
pub fn show<R: Runtime>(window: &WebviewWindow<R>) {
    let _ = window.show();
    let _ = window.unminimize();
    if window.app_handle().state::<Prefs>().window(window.label()).get("maximized") == Some(&json!(true)) {
        let _ = window.maximize();
    }
    let _ = window.set_focus();
}

/// Keeps a window's size and place as the person leaves it. A maximized window keeps the size it
/// goes back to; a hidden or minimized one keeps nothing, since its place then means nothing.
pub fn record<R: Runtime>(window: &Window<R>) {
    if !window.is_visible().unwrap_or(false) || window.is_minimized().unwrap_or(false) {
        return;
    }
    let maximized = window.is_maximized().unwrap_or(false);
    let app = window.app_handle().clone();
    let prefs = app.state::<Prefs>();
    let mut saved = prefs.window(window.label());
    saved.insert("maximized".into(), json!(maximized));
    if !maximized {
        if let (Ok(at), Ok(size)) = (window.outer_position(), window.inner_size()) {
            saved.insert("x".into(), json!(at.x));
            saved.insert("y".into(), json!(at.y));
            saved.insert("width".into(), json!(size.width));
            saved.insert("height".into(), json!(size.height));
        }
    }
    prefs.values.lock().unwrap().insert(format!("window.{}", window.label()), Value::Object(saved));
    if !prefs.due.swap(true, Ordering::SeqCst) {
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let prefs = app.state::<Prefs>();
            prefs.due.store(false, Ordering::SeqCst);
            let _ = prefs.save();
        });
    }
}
