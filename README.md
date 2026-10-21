# Drop In

A push dialog for Windows, built with Rust and Tauri 2, plus a small command line (`dropin`) that Claude Code calls.

A push shares work, and on many repos a push to `main` deploys it. Drop In makes every push go through a person: the
window shows the commits that would go out, origin/main's history, the diff of any file, and a **Rebase** button
when origin is ahead. Claude can read the state, rebase in a separate worktree and finish a conflict through the
CLI, but it cannot push. The CLI has no push command on purpose.

The layout follows PhpStorm's Push dialog. It also has a branches column like VS Code's, an "Uncommitted changes"
row with a commit box, and image diffs. It works with any git repo; nothing in it is tied to a host or a team.

## Contents

- [What it does](#what-it-does)
- [Installing](#installing)
- [Building](#building)
- [Using it](#using-it)
- [How it is put together](#how-it-is-put-together)
- [Where it keeps its files](#where-it-keeps-its-files)
- [Testing](#testing)
- [Claude Code integration](#claude-code-integration)
- [Making changes](#making-changes)
- [Releasing](#releasing)
- [Known limits](#known-limits)

## What it does

| Area | Features |
| --- | --- |
| Push | The commits to push and origin/main (last 20); incoming commits are dotted. Push sends exactly the commit that was reviewed. The alert strip covers behind + Rebase, a kept conflict, and Claude waiting. |
| Diff | Side by side (each side scrolls on its own) or unified. Images show before and after on a checkerboard. Pop the diff out into its own window. F7 / Shift+F7 step change by change. |
| Branches | HEAD, Local and Remote in a tree with `/` folders and ↑↓ ahead/behind. Switch (Enter or double-click), new (Ctrl+Shift+N), rename (F2), delete (Del), delete on origin, rename or delete a folder, fetch all with prune. Ctrl+B hides the column. |
| Commit | The **Uncommitted changes** row has a tree of Changes and Unversioned files with tri-state ticks. Commit (Ctrl+Enter), Commit and Push (Ctrl+Shift+Enter), Amend (off once HEAD is pushed), Rollback (Ctrl+Alt+Z). Only the ticked files go in the commit, and other staged files stay staged. |
| Repos | Switch between repos from the header menu. **Add Repo…** opens a folder picker; **Remove…** only forgets a repo, it never touches its files. |
| Window | Close hides it to the tray; quit from the tray menu or with `dropin quit`. Sizes, splits, column widths and the docked or undocked diff are restored on the next start. Light and dark follow Windows. |
| Updates | The button at the top right checks GitHub for a newer release (quietly on start and every 6 hours; a dot marks one). A click downloads it, checks its signature, installs it and restarts. |

## Installing

Download `Drop.In_<version>_x64-setup.exe` from the [latest release](https://github.com/SkatePro-ApS/dropin/releases/latest)
and run it. It installs for the current user only, so it needs no admin rights:

- the app and the `dropin` CLI go to `%LOCALAPPDATA%\Drop In`;
- that folder is added to your user PATH, so `dropin` works in any terminal opened afterwards;
- a Start menu entry is added, and the uninstaller removes all of it again (your repo list stays).

The installer is not code-signed, so Windows SmartScreen may warn the first time: **More info → Run anyway**.
Updates after that come from the app's update button. Git for Windows must be on PATH.

## Building

Windows 10/11 only. The pipe, the window and the build all use Windows APIs.

**Prerequisites**

1. **Rust, MSVC toolchain**: install [rustup](https://rustup.rs) and use `stable-x86_64-pc-windows-msvc`. The GNU
   toolchain does not work: it lacks `as` and `windres`.
2. **Visual Studio Build Tools**: install only "Desktop development with C++" (MSVC compiler + Windows SDK).
3. **WebView2 runtime**: Windows 11 already has it.
4. **Git for Windows** on PATH: the app runs the real `git.exe`.

No Node or npm is needed for the app. The UI is plain HTML, CSS and JS in `ui/`, embedded at build time.

```bash
cargo build --release
```

This takes about 3–4 minutes, mostly because of LTO. The output goes to `target/release/`:

- `dropin-app.exe` (about 5.5 MB): the window app.
- `dropin.exe` (about 300 KB): the CLI.

`.cargo/config.toml` links the C runtime statically, so the exes run on machines without the Visual C++
redistributable. The CLI starts `dropin-app.exe` from its own folder, so keep the two exes together.

**The installer.** `cargo install tauri-cli --version "^2" --locked` once, then from `app/`:

```bash
cargo tauri build -c '{"bundle":{"createUpdaterArtifacts":false}}'
```

The setup exe lands in `target/release/bundle/nsis/`. The override skips the update signature, which needs the
private signing key (see [Releasing](#releasing)). `app/windows/hooks.nsh` adds the CLI and the PATH entry.

A debug build (`cargo build`) is faster to build and shows a console window, which helps when debugging.

## Using it

Start `dropin-app.exe`, or run any `dropin` command; the CLI starts the app in the background when it isn't
running. Add a repo with the header menu's **Add Repo…**, or with:

```bash
dropin repo add C:/path/to/repo --name myrepo
```

The CLI prints one JSON object per call. Add `--pretty` to get it indented.

| Exit code | Meaning |
| --- | --- |
| `0` | ok |
| `1` | the answer has `"ok": false` |
| `2` | usage error |
| `3` | the app could not be reached or started |

Commands:

```
status             branch, upstream, ahead/behind, changed files
outgoing           commits a push would send, and the ones it lacks [--fetch] [--preview <ref>]
rebase             fetch, then rebase onto the upstream in a separate worktree
adopt              move the branch onto a rebase finished in its worktree [--worktree <path>]
abandon            drop a kept conflict and its worktree [--worktree <path>]
pending            the repo's kept conflicts
show / focus       show the window / bring it to the front
repos              list the known repos
repo add [path]    add the repo holding path (default: here) [--name <name>]
repo remove <name> forget a repo
push-review        show the push dialog and wait: exit 0 "pushed" or exit 1 "cancelled" / "conflict"
ping / quit        is the app running (version, pid) / stop the app
```

Without `--repo <name>`, a command uses the repo that holds the current folder. `dropin help` prints the full text.

### Rebase conflicts

`dropin rebase`, and the window's Rebase button, never rebase your checkout. They rebase in a separate worktree.
After a clean rebase the branch moves the way a checkout would, and uncommitted work stays where it is. If the
rebase conflicts, the worktree is kept and the answer has `status: "conflict"`, the worktree path and the files.
Finish it in that worktree (`git rebase --continue` until it is done), then run `dropin adopt --worktree <path>`, or
`dropin abandon` to drop it. A repo holds one kept conflict at a time, and a new rebase is refused until it is
adopted or abandoned.

## How it is put together

```
Cargo.toml        workspace: app, cli, git
app/              dropin-app.exe: Tauri app, tray, named-pipe server, Tauri commands for the UI
  src/main.rs       start-up, single instance, tray, window events
  src/pipe.rs       the CLI's protocol: one JSON request per line, command dispatch
  src/dialog.rs     push-review: shows the dialog and answers the waiting CLI call
  src/repos.rs      repo list, resolving --repo or the caller's folder
  src/branches.rs   branch commands for the UI
  src/changes.rs    uncommitted changes, commit, rollback for the UI
  src/pending.rs    kept rebase conflicts (survive a restart)
  src/prefs.rs      window sizes, splits, UI choices (ui.json)
  src/update.rs     update check and install (GitHub releases, signed)
  src/config.rs     where config and cache files live
  windows/hooks.nsh installer hooks: install the CLI, add it to PATH
  tauri.conf.json   window, CSP, installer and updater settings
cli/              dropin.exe: no Tauri, so it starts in milliseconds
git/              dropin-git crate: all git work, no Tauri, with its own tests
  src/ops.rs        outgoing, reviewed push, worktree rebase, adopt, abandon
  src/branches.rs   switch, create, rename, delete (local and remote)
  src/changes.rs    status of the working tree, commit of ticked files, rollback
  src/parse.rs      parsing git's porcelain output
  src/run.rs        running git.exe (timeouts, no console window)
  src/tests.rs      tests on throwaway repos
ui/               the window: index.html + app.js (main), diff.html + diff-window.js (popped-out diff),
                  diffview.js (shared diff renderer), style.css
docs/PLAN.md      the build plan and the decisions behind each step
tools/cdp.js      drives the UI over WebView2's debug port for tests
claude/           the Claude Code skill that documents the CLI for Claude
.github/workflows release.yml: builds and publishes a signed release on a v* tag
```

**How the CLI talks to the app.** The app owns a per-user named pipe, `\\.\pipe\dropin-<user>`. Owning the
pipe's first instance is also what keeps the app to a single instance. The CLI connects, writes one JSON line and
reads one JSON line back. If no app is running, the CLI starts `dropin-app.exe --background`, which stays hidden
until a command asks for the window. The CLI doesn't hold a list of commands; it passes everything through to the
app. A new app command therefore needs no CLI change, only an entry in `cli/src/main.rs`'s `USAGE` text.

**Git rules worth knowing before changing `git/`:**

- A push sends the reviewed sha, never "whatever HEAD is now".
- A new branch never tracks its base. A branch made from origin/main must not push to main.
- Switching never stashes. Uncommitted changes come along when git allows it; otherwise git's refusal is shown and
  nothing moves.
- A delete that can lose work asks first. Every notice names the command that undoes it.
- During a merge, the commit box refuses, because git only commits a merge whole.

## Where it keeps its files

| What | Where |
| --- | --- |
| Repo list | `%USERPROFILE%\.config\dropin\config.json` |
| Window and UI state | `%USERPROFILE%\.config\dropin\ui.json` |
| Kept rebase conflicts | `%USERPROFILE%\.config\dropin\pending.json` |
| Rebase worktrees | the repo's `.claude/tmp/` when it has one, else `%USERPROFILE%\.cache\dropin\worktrees` |
| WebView2 profile | `%LOCALAPPDATA%\dk.skatepro.dropin` (Tauri's default for the app id) |
| The installed app | `%LOCALAPPDATA%\Drop In` |

These files deliberately live outside `%APPDATA%`. Claude Desktop runs as an MSIX package, which redirects AppData
writes into a private copy. A repo Claude added from there would never show up in the window you opened yourself.

## Testing

```bash
cargo test -p dropin-git
```

This runs 24 tests on throwaway repos in about 10 seconds. They cover the reviewed push, a refused push, a clean
rebase that keeps staged files staged, conflict → adopt, the branch rules, and commits of ticked files. Add a test
here for any change to git behaviour.

**UI tests without taking over the screen.** Start a release build hidden, with WebView2's debug port open, and
drive it with Playwright over CDP:

```bash
WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333 target/release/dropin-app.exe --background &
node tools/cdp.js 'eval:document.title' 'click:#push' 'shot:out.png'
target/release/dropin.exe quit
```

`tools/cdp.js` needs Playwright (`npm i -g playwright`, or set `PLAYWRIGHT_PATH`). It never moves the real mouse,
types on the real keyboard or steals focus. Some caveats:

- Don't trigger the **Add Repo…** folder picker in a scripted test. It opens a real system dialog, and
  `__TAURI_INTERNALS__.invoke` can't be stubbed from the page.
- A hidden window never saves its size, so test size saving by hand.
- Use a throwaway repo for anything that commits, deletes or pushes.

## Claude Code integration

- **Skill**: [claude/skills/dropin/SKILL.md](claude/skills/dropin/SKILL.md) teaches Claude the CLI. Copy the
  `dropin` folder to `~/.claude/skills/`. It calls `dropin` from PATH, as the installer sets it up.
- **A review button**: a Claude Code status-line mod can run `dropin push-review --by-person` when you press a
  button. If the window's Rebase conflicts while that call waits, the call answers with the conflict, and the mod
  can send it to the Claude session as a prompt. Claude Code lets a mod wait at most 10 minutes. After that, use
  the alert's **Copy for Claude** button instead.
- **Rebase worktrees** go in the repo's `.claude/tmp/` when it has that folder (Claude Code projects often do, and
  it is usually gitignored), so the worktree sits where Claude may already work.

## Making changes

- **UI**: edit `ui/` and rebuild. The files are embedded, so a running app doesn't pick up changes. Run
  `dropin quit` first, because Windows can't overwrite a running exe.
- **New app command**: add it to the `match` in `app/src/pipe.rs` and to `USAGE` in `cli/src/main.rs`.
- **New UI action**: add a `#[tauri::command]` in the relevant `app/src/*.rs` file, register it in `main.rs`, and
  call it with `api.invoke(...)` in `ui/app.js`.
- **Git behaviour**: change `git/` and add a test in `git/src/tests.rs`.
- **Renaming the app id** (`identifier` in `app/tauri.conf.json`) starts a fresh WebView2 profile.
- **Version**: one place, `[workspace.package]` in the root `Cargo.toml`. The app, the CLI and the installer all
  read it, and `dropin ping` reports it.

## Releasing

1. Bump the version in the root `Cargo.toml` and run `cargo build` so `Cargo.lock` follows. Commit.
2. Tag and push the tag: `git tag v0.2.0 && git push origin v0.2.0`.
3. [`.github/workflows/release.yml`](.github/workflows/release.yml) builds the installer on Windows, signs it for
   the updater and publishes a GitHub release with the setup exe, its `.sig` and `latest.json`. Edit the release
   notes on GitHub if you like; the update dialog shows them.

Every installed copy finds the release through
`https://github.com/SkatePro-ApS/dropin/releases/latest/download/latest.json` (`plugins.updater` in
`app/tauri.conf.json`). The updater only installs a file signed with the private key that matches the public key
in that config. The private key and its password live in the repo's Actions secrets `TAURI_SIGNING_PRIVATE_KEY`
and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`; keep an offline copy, because without it no installed copy can be
updated again (a new key means everyone reinstalls by hand).

## Known limits

- A new branch can't be pushed from the window the first time. It has no upstream, so the dialog says to run
  `git push -u origin <name>` once.
- The window uses about 330 MB of memory, most of it WebView2's helper processes; `dropin-app.exe` itself uses about
  30 MB.
- If Claude Desktop starts the app, it runs inside Claude Desktop's package, and quitting or updating Claude Desktop
  can close it. A shortcut in the Startup folder avoids that.
- An update is refused while a push review waits or a rebase, adopt or push runs, because the app exits for the
  installer.
