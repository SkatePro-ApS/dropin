//! Running git with a timeout, no prompts and no optional locks.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::Duration;

use wait_timeout::ChildExt;

use crate::parse::tail;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Keeps git from opening a console window behind a GUI app.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Output past this is dropped and `truncated` is set; a patch-id comparison then gives up.
const STDOUT_CAP: usize = 16 * 1024 * 1024;
const STDERR_CAP: usize = 256 * 1024;

/// What one git run printed.
pub struct Ran {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

impl Ran {
    pub fn ok(&self) -> bool {
        self.code == 0
    }

    /// stdout, or an error naming `what` with the end of git's complaint.
    pub fn must(self, what: &str) -> Result<String, String> {
        if self.code == 0 {
            return Ok(self.stdout);
        }
        let said = tail(if self.stderr.trim().is_empty() { &self.stdout } else { &self.stderr }, 4);
        Err(format!("{what} failed: {}", if said.is_empty() { format!("exit {}", self.code) } else { said }))
    }
}

/// Runs git in `dir` with the default timeout.
pub fn git(dir: &Path, args: &[&str]) -> Result<Ran, String> {
    git_with(dir, args, DEFAULT_TIMEOUT, None)
}

/// Runs git in `dir`. No terminal prompt can block it, and it takes no optional locks, so a
/// read never fights the person's own git (or another Claude session) for index.lock.
/// Err only when git could not start or ran past `timeout`; a failing git is `Ok` with its code.
pub fn git_with(dir: &Path, args: &[&str], timeout: Duration, stdin: Option<&str>) -> Result<Ran, String> {
    let (code, stdout, stderr, truncated) = run(dir, args, timeout, stdin, STDOUT_CAP)?;
    Ok(Ran {
        code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        truncated,
    })
}

/// git's stdout as bytes (a blob, an image), or an error naming the step. More than `cap` bytes
/// is an error: a cut image is no image.
pub fn git_bytes(dir: &Path, args: &[&str], cap: usize, what: &str) -> Result<Vec<u8>, String> {
    let (code, stdout, stderr, truncated) = run(dir, args, DEFAULT_TIMEOUT, None, cap)?;
    if code != 0 {
        let said = tail(&String::from_utf8_lossy(&stderr), 4);
        return Err(format!("{what} failed: {}", if said.is_empty() { format!("exit {code}") } else { said }));
    }
    if truncated {
        return Err(format!("{what}: the file is over {} MB", cap / (1024 * 1024)));
    }
    Ok(stdout)
}

fn run(dir: &Path, args: &[&str], timeout: Duration, stdin: Option<&str>, cap: usize) -> Result<(i32, Vec<u8>, Vec<u8>, bool), String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "core.quotepath=false", "-c", "color.ui=never"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let mut child = cmd.spawn().map_err(|e| format!("git could not start: {e}"))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let text = text.to_owned();
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    // Both pipes drain on their own threads, so a chatty git never blocks on a full pipe.
    let out = drain(child.stdout.take(), cap);
    let err = drain(child.stderr.take(), STDERR_CAP);
    let Some(status) = child.wait_timeout(timeout).map_err(|e| e.to_string())? else {
        let _ = child.kill();
        let _ = child.wait();
        let step = args.iter().find(|a| !a.starts_with('-')).unwrap_or(&"");
        return Err(format!("git {step} ran over {} s and was stopped", timeout.as_secs()));
    };
    let (stdout, truncated) = out.join().unwrap_or_default();
    let (stderr, _) = err.join().unwrap_or_default();
    Ok((status.code().unwrap_or(-1), stdout, stderr, truncated))
}

/// Reads a pipe to its end, keeping the first `cap` bytes.
fn drain<R: Read + Send + 'static>(pipe: Option<R>, cap: usize) -> JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let Some(mut pipe) = pipe else { return (Vec::new(), false) };
        let mut kept = Vec::new();
        let mut buf = [0u8; 64 * 1024];
        let mut truncated = false;
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = cap.saturating_sub(kept.len());
                    truncated |= n > room;
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
        (kept, truncated)
    })
}
