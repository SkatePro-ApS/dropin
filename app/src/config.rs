//! The repo list, kept in `%USERPROFILE%\.config\dropin\config.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One repo the app can switch to.
#[derive(Clone, Serialize, Deserialize)]
pub struct Repo {
    pub name: String,
    /// The work tree's top level, as `git rev-parse --show-toplevel` prints it (`C:/dev/dropin`).
    pub path: String,
}

/// The app's settings file.
#[derive(Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub repos: Vec<Repo>,
    /// Set when config.json exists but could not be read: saving is then refused,
    /// so a typo in the file never costs the person their repo list.
    #[serde(skip)]
    pub load_error: Option<String>,
}

/// The app's own folder. Not %APPDATA%: Claude Desktop is an MSIX app, and a Drop In it
/// starts sees the package's private copy of AppData, so the person's Drop In would read another file.
pub fn dir() -> PathBuf {
    let base = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join(".config").join("dropin")
}

fn file() -> PathBuf {
    dir().join("config.json")
}

/// Reads config.json; a missing file is an empty list.
pub fn load() -> Config {
    let path = file();
    match std::fs::read_to_string(&path) {
        Err(_) => Config::default(),
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| Config {
            load_error: Some(format!("{} could not be read: {e}", path.display())),
            ..Config::default()
        }),
    }
}

/// Lowercases and uses forward slashes, so `C:\Dev\x\` and `c:/dev/x` compare equal.
pub fn norm(path: &str) -> String {
    path.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

impl Config {
    /// Writes config.json, unless it could not be read at start-up.
    pub fn save(&self) -> Result<(), String> {
        if let Some(error) = &self.load_error {
            return Err(format!("{error}; fix or delete it first"));
        }
        let path = file();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// The repo with this name, ignoring case.
    pub fn find(&self, name: &str) -> Option<&Repo> {
        self.repos.iter().find(|r| r.name.eq_ignore_ascii_case(name))
    }

    /// The repo whose work tree holds `dir`; the deepest one when repos are nested.
    pub fn for_dir(&self, dir: &str) -> Option<&Repo> {
        let dir = norm(dir);
        self.repos
            .iter()
            .filter(|r| {
                let root = norm(&r.path);
                dir == root || dir.starts_with(&format!("{root}/"))
            })
            .max_by_key(|r| r.path.len())
    }

    /// The configured names, for error messages.
    pub fn names(&self) -> String {
        if self.repos.is_empty() {
            return "none yet".into();
        }
        self.repos.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ")
    }
}
