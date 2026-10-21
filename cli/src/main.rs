//! dropin: the command line for Drop In. Sends one request to the running app (starting it when
//! needed) and prints its JSON answer on one line.
//!
//! Exit codes: 0 ok, 1 the app answered `ok: false`, 2 usage, 3 the app could not be reached.

use std::ffi::c_void;
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
    fn GetCurrentPackageFullName(length: *mut u32, name: *mut u16) -> i32;
    fn InitializeProcThreadAttributeList(list: *mut c_void, count: u32, flags: u32, size: *mut usize) -> i32;
    fn UpdateProcThreadAttribute(
        list: *mut c_void,
        flags: u32,
        attribute: usize,
        value: *const c_void,
        size: usize,
        previous: *mut c_void,
        returned: *mut usize,
    ) -> i32;
    fn DeleteProcThreadAttributeList(list: *mut c_void);
    fn CreateProcessW(
        application: *const u16,
        command_line: *mut u16,
        process_attributes: *const c_void,
        thread_attributes: *const c_void,
        inherit_handles: i32,
        creation_flags: u32,
        environment: *const c_void,
        current_directory: *const u16,
        startup_info: *const StartupInfoExW,
        process_info: *mut ProcessInformation,
    ) -> i32;
    fn CloseHandle(handle: isize) -> i32;
}

/// GetCurrentPackageFullName's answer when the process has no package identity.
const APPMODEL_ERROR_NO_PACKAGE: i32 = 15700;
const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
/// ProcThreadAttributeValue(18, thread: false, input: true, additive: false).
const PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY: usize = 0x0002_0012;
/// The processes the new one starts run outside an MSIX package, even when it runs in one.
const PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_ENABLE_PROCESS_TREE: u32 = 0x01;

#[repr(C)]
struct StartupInfoExW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    position_and_size: [u32; 7],
    flags: u32,
    show_window: u16,
    reserved2_size: u16,
    reserved2: *mut u8,
    std_input: isize,
    std_output: isize,
    std_error: isize,
    attribute_list: *mut c_void,
}

#[repr(C)]
struct ProcessInformation {
    process: isize,
    thread: isize,
    process_id: u32,
    thread_id: u32,
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
    if in_package() {
        // Under an MSIX-packaged program (Claude Desktop) whose policy keeps descendants in its
        // package, the app and everything it starts would see the package's private copy of AppData
        // and HKCU: the update installer would install there, not in the real install folder. So the
        // app's own children (the installer, the restarted app) are sent outside the package. The
        // plain spawn below is the fallback.
        if spawn_children_outside_package(&exe, flags | CREATE_BREAKAWAY_FROM_JOB)
            .or_else(|_| spawn_children_outside_package(&exe, flags))
            .is_ok()
        {
            return Ok(());
        }
    }
    spawn(flags | CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| spawn(flags))
        .map(|_| ())
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))
}

/// Whether this process runs with an MSIX package identity.
fn in_package() -> bool {
    let mut length = 0u32;
    unsafe { GetCurrentPackageFullName(&mut length, std::ptr::null_mut()) != APPMODEL_ERROR_NO_PACKAGE }
}

/// Starts `exe --background` the way start_app does, with the policy that every process it starts
/// runs outside an MSIX package (PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY), which std's Command can't
/// ask for. No handles are inherited, so the child gets no std handles, as with Stdio::null().
fn spawn_children_outside_package(exe: &Path, flags: u32) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let mut command_line = wide(format!("\"{}\" --background", exe.display()).as_ref());
    let folder = wide(exe.parent().unwrap_or(Path::new(".")).as_os_str());
    let policy = PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_ENABLE_PROCESS_TREE;
    unsafe {
        let mut size = 0usize;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        // usize elements keep the opaque list pointer-aligned.
        let mut buffer = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        let list = buffer.as_mut_ptr().cast::<c_void>();
        if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let result = (|| {
            if UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY,
                (&policy as *const u32).cast(),
                std::mem::size_of::<u32>(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let mut startup: StartupInfoExW = std::mem::zeroed();
            startup.cb = std::mem::size_of::<StartupInfoExW>() as u32;
            startup.attribute_list = list;
            let mut process: ProcessInformation = std::mem::zeroed();
            if CreateProcessW(
                std::ptr::null(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                flags | EXTENDED_STARTUPINFO_PRESENT,
                std::ptr::null(),
                folder.as_ptr(),
                &startup,
                &mut process,
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            CloseHandle(process.thread);
            CloseHandle(process.process);
            Ok(())
        })();
        DeleteProcThreadAttributeList(list);
        result
    }
}
