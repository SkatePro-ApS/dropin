//! dropin: the command line for Drop In. Sends one request to the running app (starting it when
//! needed) and prints its JSON answer on one line.
//!
//! Exit codes: 0 ok, 1 the app answered `ok: false`, 2 usage, 3 the app could not be reached.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

const USAGE: &str = "\
usage: dropin <command> [args] [--repo <name>] [--pretty]

  status             branch, upstream, ahead/behind, changed files
  outgoing           commits a push would send, and the ones it lacks [--fetch] [--preview <ref>]
  rebase             fetch, then rebase onto the upstream in a separate worktree; a conflict
                     answers status \"conflict\" with the worktree to finish it in
  adopt              move the branch onto a rebase finished in its worktree [--worktree <path>]
  abandon            drop a kept conflict and its worktree [--worktree <path>]
  pending            the repo's kept conflicts
  show               show the window (on the repo, when one is known)
  focus              bring the window to the front
  repos              list the known repos
  repo add [path]    add the repo holding path (default: here) [--name <name>]
  repo remove <name> forget a repo
  push-review        show the push dialog on the repo; waits until the person pushes (exit 0,
                     status \"pushed\") or closes it (exit 1, status \"cancelled\"); --by-person
                     when the person opened it (no \"Claude is waiting\" banner)
  ping               is the app running; its version and pid
  quit               stop the app

Without --repo, a command uses the repo holding the current folder.";

/// Windows error codes the connect loop handles.
const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PIPE_BUSY: i32 = 231;

/// Process creation flags for starting the app so it outlives this CLI and its console.
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

#[link(name = "user32")]
extern "system" {
    fn AllowSetForegroundWindow(process_id: u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetStdHandle(which: u32) -> isize;
    fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
}

/// STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE (-10, -11, -12 as DWORD).
const STD_HANDLES: [u32; 3] = [-10i32 as u32, -11i32 as u32, -12i32 as u32];
const HANDLE_FLAG_INHERIT: u32 = 1;

/// Stops this process's own stdin/stdout/stderr from being inherited. CreateProcess hands every
/// inheritable handle to the child, whatever its own std handles are; a caller's pipe
/// (`dropin ping | cat`) would then stay open as long as the app runs, and the reader never ends.
fn keep_std_handles_home() {
    for which in STD_HANDLES {
        unsafe {
            let handle = GetStdHandle(which);
            if handle != 0 && handle != -1 {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

fn main() {
    std::process::exit(run());
}

/// Parses, sends, prints; returns the exit code.
fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (request, pretty) = match parse(&args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            println!("{USAGE}");
            return 0;
        }
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            return 2;
        }
    };
    if matches!(request["cmd"].as_str(), Some("show" | "focus")) {
        // Lets the app take the foreground, which Windows otherwise allows only the active app.
        // ASFW_ANY: the app's pid is not known before it answers.
        unsafe { AllowSetForegroundWindow(u32::MAX) };
    }
    let (answer, code) = match call(&request) {
        Ok(answer) => {
            let code = if answer["ok"] == json!(true) { 0 } else { 1 };
            (answer, code)
        }
        Err(error) => (json!({ "ok": false, "error": error }), 3),
    };
    let text = if pretty { serde_json::to_string_pretty(&answer) } else { serde_json::to_string(&answer) };
    println!("{}", text.unwrap_or_default());
    code
}

/// Turns `cmd pos... --key value --flag` into the request object; `None` asks for the usage.
///
/// The CLI stays generic (any command, any flags) so a new app command needs no CLI release.
fn parse(args: &[String]) -> Result<Option<(Value, bool)>, String> {
    let Some(cmd) = args.first() else { return Ok(None) };
    if matches!(cmd.as_str(), "help" | "-h" | "--help") {
        return Ok(None);
    }
    if cmd.starts_with("--") {
        return Err(format!("expected a command before {cmd}"));
    }
    let mut request = Map::new();
    let mut positional = Vec::new();
    let mut pretty = false;
    let mut rest = args[1..].iter().peekable();
    while let Some(arg) = rest.next() {
        if arg == "--pretty" {
            pretty = true;
        } else if let Some(key) = arg.strip_prefix("--") {
            let value = match rest.peek() {
                Some(next) if !next.starts_with("--") => json!(rest.next().unwrap()),
                _ => json!(true),
            };
            request.insert(key.to_string(), value);
        } else {
            positional.push(json!(arg));
        }
    }
    let cwd = std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default();
    request.insert("cmd".into(), json!(cmd));
    request.insert("args".into(), Value::Array(positional));
    request.insert("cwd".into(), json!(cwd));
    Ok(Some((Value::Object(request), pretty)))
}

/// Sends the request and reads the answer line.
fn call(request: &Value) -> Result<Value, String> {
    let pipe = connect()?;
    (&pipe)
        .write_all(format!("{request}\n").as_bytes())
        .map_err(|e| format!("cannot send to Drop In: {e}"))?;
    let mut line = String::new();
    BufReader::new(&pipe)
        .read_line(&mut line)
        .map_err(|e| format!("cannot read Drop In's answer: {e}"))?;
    serde_json::from_str(&line).map_err(|e| format!("Drop In answered something that is not JSON ({e}): {line}"))
}

/// The pipe's name; must match app/src/pipe.rs.
fn pipe_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let user: String = user.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    format!(r"\\.\pipe\dropin-{user}")
}

/// Opens the pipe, starting the app once when nobody is listening.
fn connect() -> Result<File, String> {
    let name = pipe_name();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut started = false;
    loop {
        match OpenOptions::new().read(true).write(true).open(&name) {
            Ok(pipe) => return Ok(pipe),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {}
            Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
                if !started {
                    start_app()?;
                    started = true;
                }
            }
            Err(e) => return Err(format!("cannot open {name}: {e}")),
        }
        if Instant::now() > deadline {
            return Err("Drop In did not answer within 10 s".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Starts dropin-app.exe (beside this exe) in the background, detached from this console and,
/// where the caller's job object allows it, from the job, so it outlives the shell that ran dropin.
fn start_app() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("dropin-app.exe");
    let spawn = |flags: u32| {
        Command::new(&exe)
            .arg("--background")
            // Its own folder, not the caller's: a process's current folder cannot be deleted while it runs.
            .current_dir(exe.parent().unwrap_or(Path::new(".")))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    keep_std_handles_home();
    let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    spawn(flags | CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| spawn(flags))
        .map(|_| ())
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))
}
