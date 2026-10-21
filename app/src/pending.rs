//! Rebases that stopped on a conflict, waiting in their worktrees to be finished and adopted.
//! Kept in `%USERPROFILE%\.config\dropin\pending.json`, so a restart of the app loses none.

use std::path::PathBuf;
use std::sync::Mutex;

use dropin_git::Conflict;

use crate::config;

/// One kept conflict, with the repo it belongs to.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Pending {
    pub repo: String,
    #[serde(flatten)]
    pub conflict: Conflict,
}

pub struct Store(Mutex<Vec<Pending>>);

fn file() -> PathBuf {
    config::dir().join("pending.json")
}

impl Store {
    /// Reads pending.json; a missing or unreadable file is an empty list (worktrees can be
    /// abandoned by hand with `git worktree remove`).
    pub fn load() -> Self {
        let list = std::fs::read_to_string(file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Store(Mutex::new(list))
    }

    fn save(list: &[Pending]) -> Result<(), String> {
        let text = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(config::dir()).map_err(|e| e.to_string())?;
        std::fs::write(file(), text).map_err(|e| format!("pending.json could not be written: {e}"))
    }

    pub fn add(&self, pending: Pending) -> Result<(), String> {
        let mut list = self.0.lock().unwrap();
        list.retain(|p| !same(&p.conflict.worktree, &pending.conflict.worktree));
        list.push(pending);
        Self::save(&list)
    }

    /// The conflict kept in `worktree`, or the repo's only one when no worktree is named.
    pub fn find(&self, repo: &str, worktree: Option<&str>) -> Result<Pending, String> {
        let list = self.0.lock().unwrap();
        let mine: Vec<&Pending> = list.iter().filter(|p| p.repo.eq_ignore_ascii_case(repo)).collect();
        let hit = match worktree {
            Some(w) => mine.into_iter().find(|p| same(&p.conflict.worktree, w)),
            None if mine.len() == 1 => mine.into_iter().next(),
            None if mine.is_empty() => None,
            None => return Err(format!("{repo} has {} kept conflicts: pass --worktree <path>", mine.len())),
        };
        hit.cloned().ok_or_else(|| match worktree {
            Some(w) => format!("no kept conflict in {w}"),
            None => format!("{repo} has no kept conflict"),
        })
    }

    pub fn remove(&self, worktree: &str) -> Result<(), String> {
        let mut list = self.0.lock().unwrap();
        list.retain(|p| !same(&p.conflict.worktree, worktree));
        Self::save(&list)
    }

    pub fn of_repo(&self, repo: &str) -> Vec<Pending> {
        self.0.lock().unwrap().iter().filter(|p| p.repo.eq_ignore_ascii_case(repo)).cloned().collect()
    }
}

fn same(a: &str, b: &str) -> bool {
    config::norm(a) == config::norm(b)
}
